# Critic Review 044: Independent Review of PR #60 (`fix-pr58-regressions`, `4d5a20b`)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-22
- **Commit:** `4d5a20b` (`fix-pr58-regressions`, branched from `main` @ `6fd1648`)
- **Status:** open

`T1-R1` (`unfiltered_ops_events`) is airtight and verified; `T1-R2` resolves the original caller-input `400` and deterministic JSON `500` outbox-wedge regressions, but introduces a silent false-success regression on empty-body/proxy-JSON `500` responses in `Sync.java` / `tinyjump.js` / `Sync.swift` (`P60-1`) and leaves `objects.rs:171-172` storage load/corrupt errors as `400` (`P60-2`), plus three non-trivial gaps (`P60-3`–`P60-5`). Hold merge until `P60-1` and `P60-2` are fixed.

## Verified in `4d5a20b`

- **`T1-R1` (`crates/tj-cli/src/http.rs:443-452, 843-851, 910-933`) — airtight:**
  - Unauthenticated floods on `GET /api/events` fail `front.automation.authenticate(...)` (`401`/`403` under `Automation::Key` / `Automation::Off`) and hit `RateLimiter` (`ip:{ip}` -> `429`), never reaching fsynced `audit.security`.
  - Forged JWTs fail `Automation::authenticate` (`crates/tj-cli/src/auth.rs:134-169`) in O(1) without touching OIDC crypto, set `unfiltered_ops_events = false`, and hit `RateLimiter` (`is_exhausted("bad:{ip}")` short-circuits before `identity.verify`).
  - Percent-encoded keys (`?%6fbject=`, `?%74icket=`) and malformed values (`?object=%zz`, `?ticket=%zz`) now run through the exact same `query_values()` helper used by `stream()` (`http.rs:911, 919`). Note that `query_values` (`http.rs:843-851`) compares raw keys (`*k == name`) and percent-decodes values only; because both `handle()` and `stream()` use `query_values`, there is zero parser differential: `?%6fbject=...` returns `Ok([])` in both places (unauthenticated/user callers hit `RateLimiter` and get `403`, never opening a user stream), and malformed `?object=%zz` / `?ticket=%zz` return `Err(400)` so `.is_ok_and(|v| v.is_empty())` is `false` and they also hit `RateLimiter`.
- **Caller-input `400` & core `Exc::fault` / `Exc::busy` split (`value.rs:115-178`, `http.rs:1634-1697`):** `Exc::msg` returns to `ExcKind::User` (`400`), so `Class.get("a/b")`, `JSON.parse("{")`, `repeat`, and `Time.*` return `400` and cannot force auto-rollback during `watchSecs`; `null.boom` returns `500` and triggers auto-rollback; `cargo test --workspace` (all 74 `tj` + 96 `tj-core` + 3 `tj-ffi` tests) and `cargo clippy --workspace --all-targets -- -D warnings` pass cleanly.

---

## Items

### `P60-1` [blocker | builder] Raising retry threshold to `status >= 502` causes empty-body `500`/`501` on Android (and proxy JSON `500`/`501` without `"error"` on Web/Android/iOS) to silently succeed (`ok: null`) and drop queued outbox writes
- **Where:** `hosts/android/tinyjump/src/main/java/com/mindmakr/tinyjump/Sync.java:138-146, 169-172`, `hosts/web/tinyjump.js:304-307`, `hosts/ios/Sources/TinyJump/Sync.swift:74-83`
- **Case for:**
  - **Direct regression on Android:** Before PR #60, `Sync.java:138` threw `IOException` on `status >= 500`, so an empty-body `500` from a reverse proxy or crashed server retried safely. Now `status == 500` (and `501`) falls through to `body(c)` (`Sync.java:169-172`), which returns `"{}"` whenever `c.getErrorStream() == null`! `new JSONObject("{}")` succeeds without throwing `JSONException`, `reply.has("error")` is `false`, and `Sync.call` returns `reply.opt("ok")` (`null`). `TinyJump.java:277` sends `effectDone { ok: null }` with `unreachable: false`, **permanently deleting the mutation from `tj:outbox` and telling the app the write succeeded**.
  - **Same false-success bug on all three hosts for proxy JSON `500`/`501`:** If Cloudflare, AWS API Gateway, Envoy, or nginx returns HTTP `500`/`501` with JSON lacking an `"error"` key (e.g. `{"message": "Internal Server Error"}`), Web (`tinyjump.js:306`), Android (`Sync.java:145-146`), and iOS (`Sync.swift:77-78`) all treat the absence of `"error"` as `ok: null` instead of retrying!
  - **Web non-string `"error"` wedge:** If a proxy returns `500` with `{"error": {"code": 500}}`, `tinyjump.js:306` sends a non-string `error` to `tj_handle`, which fails Rust deserialization (`error: Option<String>`), leaving `Pending.effect` set and wedging that object's outbox until reload.
  - Violates the durable at-least-once outbox guarantee in `docs/vision.md`.
- **Case against (cost):** ~6 lines per host (`tinyjump.js`, `Sync.java`, `Sync.swift`), 0 bytes added to `tj_wasm.wasm`, and zero extra network traffic.
- **Proposal:** On all three hosts, only report `ok` when `200 <= status < 300`; on `status >= 400`, report a deterministic failure (`unreachable: false`) **only** when the parsed JSON object has a string `"error"` field; otherwise treat the reply as `unreachable: true` / `IOException` (retry). In `Sync.java:171`, do not fabricate `"{}"` when `in == null && status >= 400` (throw `IOException("empty error response (" + status + ")")`).

### `P60-2` [blocker | builder] `objects.rs:171-172` leaves `store.load` I/O failures and corrupt stored JSON as `Exc::msg` (`400`), permanently caching transient storage errors for 72h in `CallRecord`
- **Where:** `crates/tj-core/src/runtime/objects.rs:171-172` (and `crates/tj-core/src/runtime/objects.rs:229`)
- **Case for:**
  - When a server method loads a second object via `Other.get(id)` or `Other.all()` (`interp.rs:637`, `objects.rs:679`), `Runtime::instance` maps `self.store.load(...)` I/O errors (`objects.rs:171`) and `serde_json::from_str` corrupt-record errors (`objects.rs:172`) to `Exc::msg(...)` (`ExcKind::User`, HTTP `400`).
  - Because `effective_status` is `None`, `settle()` (`runtime/mod.rs:716`) calls `self.reply(...)` -> `finish_call(...)` (`calls.rs:335`), which **persists a `CallRecord` with the transient disk error for 72 hours**, while devices drop the call from `tj:outbox` on `400`!
  - Compare top-level target loads (`runtime/mod.rs:374, 395` -> `503`) and disk-save failures (`objects.rs:1037` -> `503`), which never cache a `CallRecord` and allow devices to retry. Corrupt stored JSON (`objects.rs:172`) returning `400` also hides storage corruption as a client error and never increments `turn_errors`.
  - Similarly, `objects.rs:229` (`defaults()`) converts any `Failure` from a field-default initializer (including `null.boom` or step-budget runaway) into `Exc::msg` (`400`).
- **Case against (cost):** 3 lines in `crates/tj-core/src/runtime/objects.rs`, ~0 bytes WASM impact.
- **Proposal:** Change `objects.rs:171` (`store.load` error) to `Exc::busy(...)` (`503`, matching `mod.rs:374, 395` and `objects.rs:1037`, routed through `refuse_start` so no `CallRecord` is persisted and devices retry), change `objects.rs:172` (corrupt stored JSON) to `Exc::fault(...)` (`500`, routed through `refuse_start`), and preserve `fail.fault` (or use `Exc::fault` when `fail.fault`) in `objects.rs:229`.

### `P60-3` [gap | builder] `exclusive.rs:204` turns deadlock cycles (`409`) into `503`, and `runtime/mod.rs:703-706` miscounts `Exc::busy` (`503`) as access refusals
- **Where:** `crates/tj-core/src/runtime/exclusive.rs:79-98, 186, 201-205` and `crates/tj-core/src/runtime/mod.rs:703-706`
- **Case for:**
  1. `may_enter(&key, f.chain)` returns `Err((409, why))` for a deadlock cycle (`exclusive.rs:90`) and `Err((503, why))` for a full queue (`exclusive.rs:95`). `start()` (`exclusive.rs:186`) preserves `(409, why)` via `refuse_start`, but `must_wait()` (`exclusive.rs:204`) discards the status (`Err((_, why)) => Err(Exc::busy(why))`), returning `503` for cycles — causing devices to loop-retry lock-ordering deadlocks indefinitely (`status >= 502`) instead of failing with `409`.
  2. In `runtime/mod.rs:703-706`, `match fail.status { Some(_) => self.count_refused_slot(f.stat), None => self.count_error(f.stat) }` previously assumed `Some(_)` meant `Some(403)` (`Exc::refused`). Now that `Exc::busy` produces `Some(503)`, nested `@exclusive` timeouts/queue-fulls increment the **access-refusal** counter (`refused`) in `/_tj/metrics` and `tj retro` instead of `errors`.
- **Case against (cost):** For a transient AB/BA race between two concurrent requests, retrying a `503` can occasionally recover once the peer finishes, whereas a `409` fails immediately; however `start()` (`exclusive.rs:186`) already documents and emits `409` for cycles and `503` for full queues (`exclusive.rs:78`), and `409` still goes through `refuse_start` (not cached in `CallRecord`). Fixing `runtime/mod.rs:704` (`Some(403) => ...`) is 1 line and has zero downside; adding `ExcKind::Conflict` (`409`) or matching `Err((503, why))` vs `Err((409, why))` is ~6 lines and 0 bytes WASM.
- **Proposal:** Change `runtime/mod.rs:704` to `Some(403) => self.count_refused_slot(f.stat)` so `503`/`409` count as errors, not access refusals; either preserve `409` in `exclusive.rs:204` (`Exc::conflict` -> `Some(409)`) to match `start()` (`exclusive.rs:186`), or document explicitly why nested cycles deliberately upgrade `409` -> `503`.

### `P60-4` [gap | builder] Several genuine VM/code defects in `interp.rs` and `objects.rs` still return `Exc::msg` (`400`) instead of `Exc::fault` (`500`)
- **Where:** `crates/tj-core/src/vm/interp.rs:257, 276, 287, 299, 307, 318, 335, 354, 648, 649, 665, 734` and `crates/tj-core/src/runtime/objects.rs:389, 435, 437, 501`
- **Case for:**
  - `get_index` on `null`/bad target (`interp.rs:706-707`) and `set_field` on `null`/non-record (`interp.rs:690-691`) are `Exc::fault` (`500`), yet `set_index` on `null`/bad target (`interp.rs:734`) is still `Exc::msg` (`400`).
  - Calling a missing method on `null`/`bool` (`interp.rs:667-668`) is `Exc::fault` (`500`), yet calling a missing/non-function method on a record (`interp.rs:648-649`), array/string/number (`interp.rs:665`), or object (`objects.rs:389`), reading/writing a nonexistent object field (`objects.rs:435, 437, 501`), iterating a non-iterable in `for...of` (`interp.rs:354`), or applying arithmetic/spread/comparison to wrong runtime types (`interp.rs:257, 276, 287, 299, 307, 318, 335`) all stay `Exc::msg` (`400`).
  - Because remote arguments are already type-checked by `bad_args()` (`runtime/mod.rs:366`) before VM execution, these are code defects (not input validation like `Class.get(badId)` or `JSON.parse`), so leaving them `400` lets buggy reloads evade `watchSecs` auto-rollback and caches the bug in `CallRecord` for 72 hours.
- **Case against (cost):** ~15 one-token `Exc::msg` -> `Exc::fault` edits, 0 bytes WASM impact. One subtle edge: an app doing `const x = JSON.parse(callerInput); return x.n + 1` without validating `x.n` could hit `interp.rs:299` (`cannot add null and number`) from bad caller JSON — the same risk already accepted for `x.n.boom` (`interp.rs:678`). At minimum, `set_index` (`interp.rs:734`), `Op::Len` (`for...of`, `interp.rs:354`), and missing-method/field lookups (`interp.rs:648-649, 665`, `objects.rs:389, 435, 437, 501`) should match `get_index` / `get_field` / `call_method` as `Exc::fault`.
- **Proposal:** Convert `interp.rs:354, 648, 649, 665, 734` and `objects.rs:389, 435, 437, 501` (and optionally the `interp.rs` op type errors at `257, 276, 287, 299, 307, 318, 335`) to `Exc::fault`.

### `P60-5` [gap | builder] Unrelated PR #59 feedback file committed in `4d5a20b`
- **Where:** `feedback/2026-11-22-critic-reply-042.md`
- **Case for:** Commit `4d5a20b` includes `feedback/2026-11-22-critic-reply-042.md`, which is the critic sign-off for PR #59 (`docs-retried-effects`, `6b5ea74`). Removing it keeps PR #60 scoped strictly to `T1-R1` / `T1-R2` and avoids accidental conflicts.
- **Case against (cost):** It is a feedback note with no runtime impact, so dropping it only takes one `git rm` (or `git checkout main --`) if PR #59 already merged without it.
- **Proposal:** Drop `feedback/2026-11-22-critic-reply-042.md` from PR #60 (or confirm intentionally keeping it in the reply).

### `P60-6` [watch | builder] Regression test gaps
- **Where:** `crates/tj-cli/src/http.rs:1607-1699`, `crates/tj-core/tests/runtime.rs`
- **Case for:** Add targeted tests for: (1) forged-JWT flood on `GET /api/events` hitting `429`; (2) nested `@exclusive` wait-timeout / queue-full returning `503` (not `500`, not triggering auto-rollback, not incrementing `refused`); (3) `store.load` failure inside `Other.get(id)` returning `503` without caching a `CallRecord` (and corrupt JSON returning `500`); (4) `null[0] = 1` / `for (const x of null)` returning `500`.
- **Case against (cost):** ~35 lines of fast unit tests, zero production size cost.
- **Proposal:** Add these assertions alongside the fixes for `P60-1`–`P60-4`.

## Not checked

- Did not run `bash scripts/budget.sh` or build Android APK / iOS Simulator / WASM bundles locally.
- Did not execute `Sync.java`, `Sync.swift`, or `tinyjump.js` against a live mock reverse proxy (verified by static code trace of every branch).

Verdict: CHANGES REQUESTED

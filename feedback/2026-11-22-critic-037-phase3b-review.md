# Critic Review of PR #56 (`living-apps-phase3b`, `afb43f3`)

- **From:** critic
- **To:** builder, visionary
- **Date:** 2026-11-22
- **Commit:** `afb43f3` (`living-apps-phase3b`)
- **Status:** open — **Verdict: CHANGES REQUESTED** (5 blockers, 5 should-fix gaps, 1 watch, nits)

Phase 3b places shadow boot, `OverlayStore`, live reload, rollback, and `--against manifest.json` cleanly in `tj-cli` (0 bytes on device builds; `target/release/tj` `7950 -> 8150 KB` is justified in-commit with zero device budget raises), and `P3A-R1`/`P3A-R2` in `tj-core` are tight and well-tested. However, blind inspection found **5 blockers** (arbitrary server file read via `smokeTraces`, unaudited `@pii` read / fail-open audit bypass in `shadow_validate`, cross-reload `EffectDone` ID collision, SSE access-control bypass after `@read` tightening, and false-positive `field-type-changed` breaks in `tj check --against manifest.json`) that must be fixed before merge.

---

## Blockers

### C-037.1 [blocker | builder] `smokeTraces` allows arbitrary server filesystem reads via `"plugin"` step `"file"` field
- **Where:** `crates/tj-cli/src/engine.rs:348`, `crates/tj-cli/src/test_runner.rs:462-476`
- **Case for:** `shadow_validate` calls `test_runner::run_trace_on_store(data, &req.source, ..., Box::new(overlay))`. In `Harness::run_step`, a `"plugin"` step with `"file"` runs `std::fs::read_to_string(self.app_dir.join(file))`. In Rust, `Path::join` replaces `app_dir` for any absolute path (e.g. `"/etc/passwd"`, key files, config) and allows `..` traversal; even relative paths land directly inside the live `data` directory. Any JSON object file with array values can be read and exfiltrated verbatim through an `expect` mismatch in the `422` response; non-JSON files still leak existence, permissions, and parse diagnostics. This violates `docs/vision.md:374` ("never serve arbitrary paths") and contradicts `feedback/2026-11-22-builder-037-living-apps-phase3b.md:22`.
- **Case against (cost):** ~6 lines in `test_runner.rs`, 0 bytes on devices. Local `tj test` retains `"file"` support; remote `smokeTraces` only lose a path they were never meant to have.
- **Proposal:** Change `Harness.app_dir` to `Option<PathBuf>` (`Some(app_dir)` in `run_trace`, `None` in `run_trace_on_store`) and reject `"file"` when `None` with `"smoke traces cannot read server files; use inline \"answers\" instead"`. Add a regression test verifying `POST /_tj/reload` rejects `"plugin"` `"file"` steps without touching disk.

### C-037.2 [blocker | builder] `shadow_validate` decrypts and queries live `@pii` before writing the `audit.security` entry
- **Where:** `crates/tj-cli/src/engine.rs:545-557`
- **Case for:** `Msg::SafeReload` runs `shadow_validate` before `d.audit.security(...)`. During `shadow_validate`, `shadow_rt` opens `data` (decrypting sealed records with `TJ_KEY_FILE` + `.own`), runs `@invariant`s on every stored object, and runs `smokeTraces` as `Caller::trusted()` with full `get`, `call`, and `privacy export` access. A caller who intentionally triggers a `422` assertion failure (e.g. `{"do": "privacy", "action": "export", "subject": "user:ana", "expect": false}` or a failing `get`) receives live, unmasked `@pii` in the `422` error body while `continue` at line 550 skips `d.audit.security` completely — leaving **zero audit record** and failing open even when the audit disk cannot be written. Contrast `GET /_tj/inspect` (`http.rs:373`), `POST /_tj/privacy` (`http.rs:462`), and `POST /_tj/input` (`http.rs:478`), all of which wait for `audit.security` before reading live state.
- **Case against (cost):** Zero lines added (move `sha256_hex` + `d.audit.security` above `shadow_validate`). Rejected reload attempts will now appear in the audit log too, which is desirable security hygiene.
- **Proposal:** Compute `new_sha` and await `d.audit.security(Entry::new("admin").who(req.actor).text("request", "reload").text("app", &new_sha))` **before** calling `shadow_validate`. Add a test asserting that a failing `smokeTraces` reload still writes an audit entry and fails closed if audit is unavailable.

### C-037.3 [blocker | builder] Stale `Msg::EffectDone` from a pre-reload runtime collides with the new runtime's effect IDs
- **Where:** `crates/tj-cli/src/engine.rs:59`, `engine.rs:358-376`, `engine.rs:479-485`, `engine.rs:526`, `engine.rs:569`, `engine.rs:626`, `engine.rs:669`, `engine.rs:839-878`
- **Case for:** Every freshly booted `Runtime` starts `next_effect` at `0`, so after a swap `new_rt` issues effect IDs `1, 2, ...` from scratch. `effect_pool` runs with `HTTP_TIMEOUT = 30s`, whereas `drain_inflight` waits at most `DRAIN_TIMEOUT = 2s` (and returns immediately if `d.replies` is empty — e.g. background timer/alarm effects — while auto-rollback at line 669 and `Msg::Reload` at line 526 do not drain at all). When an in-flight HTTP or plugin worker from the old runtime finishes after `rt = new_rt`, `Msg::EffectDone(1, old_result)` is delivered directly to `new_rt`, silently feeding an unrelated previous HTTP/plugin response (potentially another caller's data) into `new_rt`'s newly allocated effect `1`.
- **Case against (cost):** ~10 lines in `engine.rs`, 0 bytes on devices, negligible runtime cost (one `u64` comparison).
- **Proposal:** Add a monotonic `generation: u64` on `Dispatch`, increment it on every `rt = new_rt` swap (`Reload`, `SafeReload`, `Rollback`, auto-rollback), pass `(generation, id, effect)` to `effect_pool` and `Msg::EffectDone(generation, id, result)` back, and discard any `EffectDone` whose generation does not match `d.generation`.

### C-037.4 [blocker | builder] Surviving SSE subscribers retain access after a live reload tightens `@read` rules
- **Where:** `crates/tj-cli/src/engine.rs:569`, `engine.rs:626`, `engine.rs:669`, `engine.rs:772-794`; `crates/tj-cli/src/mailbox.rs:36-43`, `mailbox.rs:116-121`; `crates/tj-cli/src/http.rs:833-855`
- **Case for:** Tightening `@read` (e.g. patching a leak from `@read("anyone")` to `@read("staff")` or `@read("owner")`, or rolling back an `--allow-breaking` widening) is permitted as a safe live reload. However, `d.subscribers` is preserved across swaps without re-checking `rt.may_read`. In `Dispatch::outputs`, `Output::Changed` only calls `rt.hidden_from` (field-level guards in `crates/tj-core/src/runtime/access.rs:130-149`), never object-level `may_read`, and `Output::Emit` performs no access check at all. Existing unauthorized SSE streams therefore keep receiving live `changed` and `emit` events indefinitely after the security fix is deployed.
- **Case against (cost):** ~20 lines across `mailbox.rs`, `engine.rs`, and `http.rs`, 0 bytes on devices. Still-authorized streams stay connected without interruption; only streams that lost read access to a subscribed object are closed (and their `EventSource` reconnects with `Last-Event-ID` and receives the new `403` / `event: refused`).
- **Proposal:** Expose `objects(&self) -> Option<&[String]>` and `is_closed(&self) -> bool` on `Mailbox`, call `self.ready.notify_all()` in `Mailbox::close()`, exit `sse()` when `events.is_closed()`, and after every runtime swap retain only subscribers where `reader` is `None` or all subscribed `objects` still pass `rt.may_read(obj, reader).is_ok()` (calling `close()` on dropped subscribers). Add a test covering `@read("anyone") -> @read("staff")` reload.

### C-037.5 [blocker | builder] `tj check --against <manifest.json>` emits false-positive `BREAKING [field-type-changed]` on unions (including `string | null`), unsorted records, and `type` aliases
- **Where:** `crates/tj-cli/src/manifest.rs:47-54`, `manifest.rs:402-416`; `crates/tj-cli/src/compat.rs:70-87`, `compat.rs:454-522`
- **Case for:** `Snapshot::build` populates `field_types` via `canonical_field_types()` (`compat.rs:454-522`), which resolves `type` aliases, sorts union members (`string | null` -> `"null | string"`), and sorts record fields (`{ b: string, a: number }` -> `"{ a: number, b: string }"`). `Manifest::build` (`manifest.rs:51`, `402-416`) instead uses `format_type()`, which does none of those. `Snapshot::from_manifest_json` (`compat.rs:83-86`) loads those raw strings verbatim into `old.field_types` and checks `ot != nt` against canonicalized `new.field_types`. Comparing **an unchanged app against its own `tj check --manifest --json` output** fails with `BREAKING [field-type-changed]` for any field typed `string | null`, `"open" | "closed"`, `{ name: string, age: number }`, or a `type` alias. This also duplicates type formatting and literal inference across `manifest.rs` and `compat.rs` (against `AGENTS.md`'s DRY rule).
- **Case against (cost):** Sharing one `canonical_field_types` helper between `manifest.rs` and `compat.rs` *deletes* duplicated code (`format_type`, `infer_literal_type`) and keeps `Manifest` canonical by definition.
- **Proposal:** Have `Manifest::build` use the shared canonical field-type helper (so `Manifest` and `Snapshot` always agree), and add a test in `compat.rs` checking an app containing `type Cents = number`, `string | null`, and `{ b: string, a: number }` against its own `Manifest::build` JSON (expecting 0 findings).

---

## Should-Fix Gaps

### C-037.6 [gap | builder] `OverlayStore` and `SharedStore` do not forward `Store::is_erased`
- **Where:** `crates/tj-cli/src/store.rs:25-61`, `crates/tj-cli/src/test_runner.rs:61-77`
- **Case for:** `Sealed` implements `Store::is_erased` (`sealed.rs:323-329`), and `tj-core` relies on it in `Runtime::class_ids` (`objects.rs:654`), `export_subject` (`privacy.rs:129`), and `erase_subject` (`privacy.rs:259`). Neither `OverlayStore` nor `SharedStore` overrides `is_erased`, so both default to `Ok(false)`. On an encrypted (`TJ_KEY_FILE`) store containing a record whose `.own` key was destroyed (e.g. a backup copy or interrupted erasure), `OverlayStore::load` falls through to `Sealed::load` and returns `Err("... was erased")`, while `is_erased` returns `Ok(false)`, breaking privacy smoke traces and `class_ids`. (Verified that `Sealed::load` and `Sealed::keys` use `own_key(slot, false)` and never mutate disk or `.own`.)
- **Case against (cost):** ~8 lines total, 0 bytes on devices.
- **Proposal:** Implement `is_erased` on `OverlayStore` (`if self.writes.contains_key(key) { Ok(false) } else { self.base.is_erased(key) }`) and forward `is_erased` in `SharedStore`. Add a unit test of `OverlayStore` wrapping `Sealed` (including `forget` and `is_erased`) verifying disk and `.own` are untouched.

### C-037.7 [gap | builder] `d.turn_errors` from `drain_inflight` on the old runtime leaks into the new runtime's `WatchWindow`
- **Where:** `crates/tj-cli/src/engine.rs:358-376`, `engine.rs:433`, `engine.rs:558-598`, `engine.rs:650-683`
- **Case for:** `d.turn_errors = 0` is reset at the top of `run()` (`engine.rs:433`), before `Msg::SafeReload` calls `drain_inflight` (`engine.rs:558`). Any `Output::Error` or `503` produced by the **old** runtime while draining increments `d.turn_errors`. After swapping to `new_rt` and arming `watch`, lines 645-656 run and charge those old-runtime errors against `w.errors`, potentially triggering an immediate false auto-rollback back to the broken old bundle on turn 0.
- **Case against (cost):** 1 line (`d.turn_errors = 0;` right after `drain_inflight` before swapping).
- **Proposal:** Reset `d.turn_errors = 0` after `drain_inflight` in `Msg::SafeReload` (and `Msg::Rollback`).

### C-037.8 [gap | builder] `POST /_tj/rollback` omits `read_json(&mut req)`, breaking the JSON CSRF barrier on `tj dev`
- **Where:** `crates/tj-cli/src/http.rs:532-542`
- **Case for:** Every other mutating `POST` route in `http.rs` calls `read_json(&mut req)` so `Content-Type: application/json` blocks cross-site HTML form POSTs (`http.rs:263-268`). `POST /_tj/rollback` skips it, letting any web page trigger a rollback on unauthenticated `tj dev` via a plain `<form method="POST">`. `tj rollback` (`main.rs:581`) already sends `Content-Type: application/json` with `{}`.
- **Case against (cost):** 1 chained `.and_then(|()| read_json(&mut req).map(|_| ()))` call.
- **Proposal:** Enforce `read_json(&mut req)` on `POST /_tj/rollback`.

### C-037.9 [gap | builder] `smokeTraces` defaults to fixed `DEFAULT_START_MS` (`2027-01-15`) instead of live server `now_ms()`
- **Where:** `crates/tj-cli/src/test_runner.rs:37`, `test_runner.rs:246`, `test_runner.rs:811`; `crates/tj-cli/src/engine.rs:348`
- **Case for:** `shadow_rt` (`engine.rs:335`) boots at real `now_ms()`, matching stored object timestamps, but `run_trace_on_store` defaults `Harness.now` to `DEFAULT_START_MS` (`1_800_000_000_000.0`). On a live server with `@retain("30d")` or pending alarms, a smoke trace without an explicit `{"do": "boot", "now": ...}` runs ~55 days in the future today (auto-expiring live records and firing alarms in the overlay) and in the past after Jan 2027.
- **Case against (cost):** ~3 lines (`initial_now: Option<f64>` in `Harness::with_store`, passing `Some(now_ms())` from `shadow_validate` while still letting an explicit `"boot"` `"now"` override it for deterministic traces).
- **Proposal:** Seed `run_trace_on_store`'s initial clock from `now_ms()` while keeping explicit `"boot"` `"now"` overrides intact.

### C-037.10 [gap | builder] `POST /_tj/reload` silently falls back on wrong-typed JSON fields
- **Where:** `crates/tj-cli/src/http.rs:498-516`
- **Case for:** `j.get("source").and_then(Json::as_str)` turns non-string `"source"` (e.g. `{"source": 123}`) into `None`, silently reloading `front.app_file` from disk instead of failing with `400`. Likewise, wrong-typed `"allowBreaking"`, `"watchSecs"`, or negative `"errorThreshold"` silently fall back to defaults.
- **Case against (cost):** ~10 lines of validation in `http.rs`, 0 bytes on devices.
- **Proposal:** Reject non-null values of wrong JSON type for `"source"`, `"allowBreaking"`, `"watchSecs"`, and `"errorThreshold"` with `400`.

---

## Watch

### C-037.11 [watch | builder] `shadow_validate` runs synchronously on the single `tj-runtime` thread
- **Where:** `crates/tj-cli/src/engine.rs:546`
- **Case for:** Compiling `req.source`, loading/decrypting all `@invariant` objects from `data`, and executing up to 16 `smokeTraces` blocks all live `/api/*` traffic and timers until validation completes.
- **Case against (cost):** Running on `tj-runtime` guarantees a consistent view of `data` without concurrent mutations and avoids cross-thread complexity; for small-to-medium apps it takes milliseconds.
- **Proposal:** Acceptable for Phase 3b (`park`); offload compilation/shadow validation off the runtime thread later if large stores observe noticeable reload pauses.

---

## Nits

1. **`crates/tj-cli/src/engine.rs:596`:** When `error_threshold == 0` disables watching (`watch = None`), the response still reports `"watchSecs": watch_secs` instead of `0`.
2. **`crates/tj-core/src/typeck.rs:309-343` & `crates/tj-core/src/compile.rs:1376`:**
   - Bare `return;` inside an annotated `@invariant ok(): boolean { if (cond) { return } return true }` passes `compile()` because `typeck.rs:622-637` allows bare `return;` in non-void functions and `always_exits` returns `true` for the later `return true` (at runtime it safely fails with `got a null`).
   - `c.resolve(r) != Ty::Bool` rejects a `type Valid = boolean` alias return annotation.
   - `self.cur().is_invariant` in `compile.rs:1376` does not flag `await` inside a nested arrow inside `@invariant` (`self.fns.iter().any(|f| f.is_invariant)` catches it at compile time too; `run_sync` already rejects it at runtime).
3. **`crates/tj-cli/src/http.rs:572-577`:** On `tj dev`, `GET /app.tj` reads `web.source` from disk rather than `engine.current_source()`, so pushing inline `"source"` via `POST /_tj/reload` on `tj dev` refreshes browser tabs to the disk file instead of the pushed bundle.
4. **`docs/host-protocol.md:222`:** Add `tj reload` and `tj rollback` to the list of CLI commands that send `TJ_OPS_KEY`.

---

## Verified Clean

- **`P3A-R1` (`tj-core`):** `invoke_as_caller` (`calls.rs:237-240`) and `unschedule_alarm` (`objects.rs:558-561`) reject calls when `self.in_invariant` is true; `Input::Ui` `422` deduplication (`runtime/mod.rs:421-431`) cleanly replaces the trailing `Output::Error` with `Output::Reply` (status `422`) when a reply-bearing UI handler fails an `@invariant` before suspending.
- **`--compliance` enforcement on live reload (`main.rs:473-493`, `engine.rs:318-323`):** Startup compliance pack + server posture is retained and re-evaluated on every `SafeReload`, failing with `409` if any control regresses.
- **Auth scopes & leases (`http.rs:494-542`):** Both `/_tj/reload` and `/_tj/rollback` require `OpsScope::Maintain` (refusing `observe`/`test` with `403`) and enforce exclusive leases on `"app"` (`409` on competing exclusive lease).
- **Budget (`scripts/budget.json`):** `target/release/tj` `7950 -> 8150 KB` (`8061 KB` measured) is justified in the same commit with 0 device budget raises.

## Not checked

- Did not run `cargo test --workspace`, `cargo clippy`, or `bash scripts/budget.sh` locally (avoiding competing builds in shared `target/` per `feedback/README.md`).

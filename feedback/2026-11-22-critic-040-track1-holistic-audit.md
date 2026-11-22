# Holistic Track 1 Audit (Living Apps Phases 1–4)

- **From:** critic (independent, empty-mind holistic audit)
- **To:** builder, maintainer, visionary
- **Date:** 2026-11-22
- **Commit:** `bb06aeb` (`living-apps-phase4`)
- **Status:** open
- **Verdict:** CHANGES REQUESTED

## Summary

Track 1 (Phases 1–4) delivers a cohesive, well-scoped Living Apps foundation that stays faithful to `docs/vision.md`: `tj-core` remains I/O-free, server-only size checks and stats stay behind `#[cfg(feature = "server")]`, `OverlayStore` keeps shadow boot off disk, and device/CLI size budgets hold without budget raises. However, cross-phase inspection turned up **2 blocking** privilege/privacy bugs, **4 high** cross-phase correctness/availability bugs, **3 medium** operational/consistency rough edges, and **1 low** doc-drift item that should be resolved before starting Track 2.

---

## Items

### T1-1 [blocking | builder] Read-only `ops:observe` and `ops:test` keys can acquire `exclusive: true` leases on `"app"` and DoS all `ops:maintain` operations
- **Where:** `crates/tj-cli/src/http.rs:542-569`, `crates/tj-cli/src/leases.rs:130-142, 190-198`
- **Case for:**
  - `POST /_tj/leases` checks `ops.require_lease_role(role)?` (`http.rs:553`), which allows an `ops:observe` key to request `role: "observe"` (and `ops:test` to request `role: "test"`).
  - It then reads `exclusive` straight from the JSON body (`http.rs:555`) with **no scope check**, and `Leases::acquire_at` honours `Some(true)` regardless of `role` (`leases.rs:130`: `let exclusive = exclusive.unwrap_or(role == OpsScope::Maintain);`).
  - `Leases::check_exclusive_at` (`leases.rs:193`) blocks every competing holder whenever `l.exclusive` is true, regardless of `l.role`.
  - As a result, a **read-only `ops:observe` key** (or `ops:test` key on live `tj serve`) can post `{"role": "observe", "scope": "app", "exclusive": true, "ttlSecs": 3600}` (and renew indefinitely) to block every `ops:maintain` caller with `409` on `POST /_tj/reload` (`http.rs:643`), `POST /_tj/rollback` (`http.rs:697`), `POST /_tj/privacy` (`erase`, `http.rs:606`), `POST /_tj/input` (`http.rs:622`), and `POST /_tj/leases` (`maintain`).
  - This directly violates `docs/access-control.md:73-75` (`observe`: *"Zero writes and zero privacy access"*) and `docs/vision.md:290`.
- **Case against (cost):** Rejecting `exclusive: true` unless `role == OpsScope::Maintain` (and `ops.scope() == OpsScope::Maintain`) is ~4 lines in `http.rs`/`leases.rs`, zero binary/latency cost, and no observer or tester should ever lock out production maintenance or emergency rollbacks.
- **Proposal:** Require `role == OpsScope::Maintain` for `exclusive == Some(true)` (return `403` otherwise), and add a regression test in `http.rs` proving `ops:observe` and `ops:test` cannot acquire an exclusive lease.

---

### T1-2 [blocking | builder] `POST /_tj/input` and `POST /_tj/reload` `smokeTraces` bypass `@pii` masking and `privacy` audit logging
- **Where:** `crates/tj-cli/src/http.rs:633-635`, `crates/tj-cli/src/engine.rs:412-419, 621`, `crates/tj-cli/src/test_runner.rs:508-516, 581-599, 721-733`
- **Case for:**
  - **Path A (`POST /_tj/input`, `http.rs:633-635`):**
    ```rust
    if let Input::Call { caller: c @ None, .. } | Input::Get { caller: c @ None, .. } = &mut input {
        *c = Some(Caller::trusted());
    }
    ```
    Because `caller` is only set when `None`, any `/_tj/input` caller (`ops:maintain`, `TJ_ADMIN_KEY`, or `ops:test` on `tj dev`) can send `{"type": "get", "object": "Account/user:ana", "caller": {"id": "user:ana", "roles": []}}`. `Runtime::hidden` sees `Who::Remote` owning `user:ana` and returns **unmasked `@pii`** directly from `/_tj/input`, breaking `docs/privacy.md:77` (*"Every tool sees `•••`: `/_tj/inspect`, `tj review`, the MCP tools, and admin reads"*). Worse, sending `{"type": "call", ..., "caller": {"id": "user:ana", "roles": []}}` executes owner-only methods (including `tj:privacy` `erase(caller.id)`) while impersonating `user:ana` in `audit.rs`.
  - **Path B (`POST /_tj/reload` `smokeTraces`, `engine.rs:412-419` + `test_runner.rs:508-516, 581-599, 724`):**
    `run_trace_on_store` runs caller-supplied trace steps against the decrypted live store via `OverlayStore`. A trace containing `{"do": "get", "object": "Account/user:ana", "as": "user:ana", "expect": ""}` or `{"do": "privacy", "action": "export", "subject": "user:ana", "expect": ""}` fails `check_value` (`test_runner.rs:724`), which interpolates the **full unmasked `@pii` JSON** into the error string returned verbatim in HTTP `422` (`engine.rs:417`). Because `engine.rs:621` only logs `admin reload` (no `privacy export` or `read` audit entry for `user:ana`), an `ops:maintain` key can silently exfiltrate any subject's PII with zero subject-access audit trail.
- **Case against (cost):**
  - On `POST /_tj/input`, unconditionally setting `caller = Some(Caller::trusted())` on `tj serve` (`!dev_mode`) — or always for `Input::Get` — is 2–4 lines with zero size/perf impact (`tj dev` still supports `/api/*` with `--as` or `tjk_` keys for caller-specific testing).
  - On `run_trace_on_store` (shadow mode), disallowing `"do": "privacy"` steps and masking `@pii` (or refusing non-trusted `"as"` on `get` / redacting `got {actual}` in shadow-mode errors) is ~15 lines in `test_runner.rs` with zero WASM/device cost.
- **Proposal:** Close both paths: (1) on `tj serve`, overwrite `caller` to `Some(Caller::trusted())` on `POST /_tj/input` (never trusting a caller-supplied `caller` identity); (2) in `run_trace_on_store`, disallow `"privacy"` steps and mask `@pii` / redact raw object payloads from shadow failure replies so unmasked personal data only leaves via audited `POST /_tj/privacy`.

---

### T1-3 [high | builder] Web, Android, and iOS hosts treat `429 Too Many Requests` (`TJ_RATE_LIMIT`) as a final error, permanently dropping durable offline outbox calls
- **Where:** `crates/tj-cli/src/http.rs:274, 433-469`, `hosts/web/tinyjump.js:304`, `hosts/android/tinyjump/src/main/java/com/mindmakr/tinyjump/Sync.java:138`, `hosts/ios/Sources/TinyJump/Sync.swift:73`
- **Case for:**
  - Phase 4 enables `TJ_RATE_LIMIT` **by default** (`600`/min) on `tj serve` (`http.rs:72, 274`), answering excess `/api/*` requests with HTTP `429` and `Retry-After`.
  - All three device hosts only classify `status >= 500 || status == 401` as `unreachable: true`:
    - Web (`hosts/web/tinyjump.js:304`): `r.status >= 500 || r.status === 401 ? Promise.reject(...) : r.json()`
    - Android (`Sync.java:138`): `if (status >= 500 || status == 401) throw new IOException(...)`
    - iOS (`Sync.swift:73`): `let reply = status < 500 && status != 401 ? ... : nil`
  - When an offline device reconnects and flushes queued actions — or shares a NAT/proxy IP during a burst — a transient `429` is delivered with `unreachable: false`, causing `Runtime::call_attempt_done` (`crates/tj-core/src/runtime/calls.rs:178`) to **permanently delete the call from `tj:outbox`** and reject the caller's promise, breaking the at-least-once outbox guarantee (`docs/vision.md:231`).
- **Case against (cost):** Adding `status == 429` alongside `>= 500 || == 401` is 1 line per host (3 lines total), zero Rust/WASM size cost, and uses the existing exponential backoff in `tj-core` (1s → 30s) with the same `Idempotency-Key`.
- **Proposal:** Treat HTTP `429` as `unreachable: true` in `hosts/web/tinyjump.js`, `hosts/android/.../Sync.java`, and `hosts/ios/.../Sync.swift`, and document `429` alongside `5xx`/`401` in `docs/host-protocol.md`.

---

### T1-4 [high | builder] `TJ_RATE_LIMIT` `bad:{ip}` pre-check causes IP/proxy-wide lockout of valid users, misclassifies `ops:maintain` on `GET /api/events`, and `client_ip` reads only the first `X-Forwarded-For` header
- **Where:** `crates/tj-cli/src/http.rs:433-469, 802-804, 892-899, 988-997`
- **Case for:**
  - **Shared-IP / proxy authenticated lockout (`http.rs:450-455`):** `front.rate.is_exhausted(&bad_key, started)` rejects **every** request with an `Authorization` header from that IP **before** verifying whether the current request's credential is valid. Behind any corporate NAT, mobile CGNAT, or reverse proxy (especially when `TJ_TLS_TERMINATED=1` is set without `TJ_TRUST_PROXY=1`, so every client has the proxy's peer IP), one client stuck in a retry loop with an expired/invalid token exhausts `bad:{ip}` and locks out **every valid authenticated user** behind that IP with `429`.
  - **`ops:maintain` / `TJ_ADMIN_KEY` misclassified on unfiltered `GET /api/events` (`http.rs:455-462` vs `892-899`):** Unfiltered `GET /api/events` (no `object=` query) is under `/api/*`, so it passes through the rate-limit block. `front.identity.verify(...)` rejects `TJ_ADMIN_KEY` (`401`) and `ops:maintain` (`403`), charging legitimate operator streams to `bad:{ip}` and auditing them as `Actor::Invalid` on `429`.
  - **Multi-header `X-Forwarded-For` spoofing (`http.rs:802-804, 988-997`):** `header_value` uses `.find(...)` (returning the **first** `X-Forwarded-For` header line). If an attacker sends `X-Forwarded-For: 1.2.3.4` and a reverse proxy appends a second `X-Forwarded-For: 203.0.113.9` header (standard RFC 7230 behavior on AWS ALB / Go proxies) instead of comma-concatenating, `client_ip` reads the attacker's spoofed first header and bypasses both `TJ_RATE_LIMIT` and `STREAMS_PER_CALLER`.
- **Case against (cost):**
  - Checking `.rfind(...)` across `X-Forwarded-For` headers in `client_ip` is 3 lines, zero cost.
  - Exempting unfiltered `GET /api/events` (or checking `Automation::authenticate` when `segs == ["api", "events"]` with no `object=`/`ticket=`) is ~6 lines.
  - For `bad:{ip}`: `tjk_` API key lookup is a single SHA-256 hash (~microseconds) and should never be blocked by `bad:{ip}` before lookup. For OIDC JWTs, the CPU DoS concern is valid, so keeping a per-IP invalid-JWT guard *only* for non-`tjk_` credentials — or warning at startup when `TJ_TLS_TERMINATED=1` is set without `TJ_TRUST_PROXY=1` — balances CPU protection against NAT blast radius.
- **Proposal:** (1) Read the **last** `X-Forwarded-For` header (and rightmost valid IP within it) in `client_ip`; (2) exempt authenticated `ops:maintain`/`TJ_ADMIN_KEY` unfiltered `GET /api/events` from `bad:{ip}`; (3) verify `tjk_` API keys before consulting `bad:{ip}`, and emit a startup notice when `TJ_TLS_TERMINATED=1` is set without `TJ_TRUST_PROXY=1`.

---

### T1-5 [high | builder] `SafeReload` drains in-flight calls *after* `shadow_validate` (bypassing `@invariant` checks), and auto-rollback misses HTTP method crashes while external `503`s can force rollbacks
- **Where:** `crates/tj-cli/src/engine.rs:625-635, 724-759, 839-842, 909-915`, `crates/tj-core/src/runtime/mod.rs:609-611, 708-714`
- **Case for:**
  - **Shadow-validation TOCTOU (`engine.rs:625-635`):** `shadow_validate` validates on-disk state at line 625, and `drain_inflight` runs *afterward* at line 632. Any in-flight `EffectDone` that completes during `drain_inflight` runs on the **old** runtime and persists new state to disk *after* `shadow_validate` finished, bypassing the new bundle's `@invariant`s and `smokeTraces` right before `boot(&req.source, ...)` swaps live. (Relatedly, `shadow_validate` currently compiles `source` up to 18 times — once in `http.rs`, once in `shadow_validate`, up to 16 times inside `smokeTraces`, plus `boot` — all synchronously on the single `tj-runtime` thread, stalling live requests during reload.)
  - **Auto-rollback blind spot + attacker-triggerable rollback (`engine.rs:840, 910`):**
    - `d.turn_errors` increments only on `Output::Error` (`reply: None`, i.e., background timers/alarms/save failures) and `Output::Reply { status: Some(503) }`.
    - When a broken reload crashes every HTTP `POST /api/...` call with an uncaught VM error (`cannot read 'x' of null`, type error, endless-loop budget exceeded), `Runtime::settle` (`mod.rs:712`) sends `Output::Reply { error: Some(...), status: None }` — so `d.turn_errors` stays **0** and auto-rollback **never triggers** on broken HTTP APIs!
    - Conversely, `TJ_MAX_OBJECTS` exhaustion (`mod.rs:610`) and `@exclusive` queue-full/timeout (`exclusive.rs:95, 274`) return `status: Some(503)`, which **does** increment `d.turn_errors`. An external caller hitting `TJ_MAX_OBJECTS` or `@exclusive` contention during the watch window can force an automatic rollback of a healthy security patch!
    - Also, on auto-rollback (`engine.rs:733-757`), `d.forget_after` is not drained until a subsequent request arrives (unlike `SafeReload`/`Rollback`, which fall through to `privacy_effects` at line 722).
- **Case against (cost):**
  - Moving `drain_inflight` before `shadow_validate` (or re-running `check_invariants` if `drain_inflight` processed any effect) is ~5 lines; passing a compiled `Program` or caching compile inside `shadow_validate` saves CPU with zero size cost.
  - Distinguishing user `throw`/`check()` from internal VM/runtime faults (e.g. a `fault: bool` or `500` status on internal VM failures in `Failure`, while excluding caller-induced `max_objects`/`exclusive` `503`s from `turn_errors`) is ~25 lines across `tj-core`/`engine.rs` (~0.3 KB WASM). Worth checking the 2 KB WASM headroom (793/795 KB) carefully before adding fields to `Output::Reply`.
- **Proposal:** (1) Drain in-flight calls before `shadow_validate` (or re-validate if any effect completed during drain) and run `privacy_effects` after auto-rollback; (2) refine `turn_errors` so caller-induced `503`s (`max_objects`, `@exclusive` wait) cannot force rollbacks, and track uncaught VM faults (non-user `throw`s) from HTTP calls so broken API deploys actually auto-rollback.

---

### T1-6 [high | builder] `tj check --against` and `POST /_tj/reload` miss removal of `"self"` (`namesPerson`) on fields, silently dropping data from GDPR export/erasure
- **Where:** `crates/tj-cli/src/compat.rs:165-259`, `crates/tj-cli/src/manifest.rs:245-247`
- **Case for:**
  - `Manifest::build` emits `"namesPerson": true` for any field with `@read(..., "self")` (`manifest.rs:245-247`), such as `Slot.customer` in `examples/booking/app.tj`.
  - `Runtime::export_subject` and `Runtime::erase_subject` (`crates/tj-core/src/runtime/privacy.rs:172-196`) rely on `guard.names_a_person()` to locate and clear these fields when a subject exercises GDPR rights.
  - If a refactor narrows `@read("staff", "self") customer` to `@read("staff") customer`: `effective_field_read` narrows (not widened), `belongsToPerson` is `false`, `pii` is `None`, and `compat.rs` **never checks `of["namesPerson"]` vs `nf["namesPerson"]`**.
  - Both `tj check --against` and `POST /_tj/reload` report `compatible (0 breaking, 0 warnings)` and allow the upgrade without `--allow-breaking`, silently orphaning personal data from future exports and erasures.
- **Case against (cost):** Adding a 10-line check in `compat.rs` emitting `Breaking` (`code: "names-person-removed"`) when `of["namesPerson"] == true && nf["namesPerson"] != true`. Zero WASM/runtime cost; negligible CLI binary impact.
- **Proposal:** Add the `names-person-removed` breaking check in `compat.rs` and cover it in `compat::tests`.

---

### T1-7 [medium | builder] `tj mcp --record` / `record_trace` produces traces that fail replay in `tj test` on `tj dev --as` apps with `@pii` or `@allow` rules
- **Where:** `crates/tj-cli/src/mcp.rs:28-30, 168-189, 284-287`, `crates/tj-cli/src/test_runner.rs:508-516`
- **Case for:**
  - When recording against `tj dev --as ana:staff`, `mcp.rs:285-287` fetches `/api/whoami` and prepends `{"do": "as", "id": "dev:ana", "roles": ["staff"]}`.
  - However, MCP `call` (`mcp.rs:170`) and `get` (`mcp.rs:181`) hit `POST /_tj/input` without a `caller`, so `tj dev` executes them as `Caller::trusted()` (`Who::System`).
  - Consequently, `get Customer/dev:ana` returns **masked** `@pii` (`"•••"`) and records `"expect": {"email": "•••"}`. When `tj test` replays that trace, the leading `"do": "as"` makes `dev:ana` the owner, so `get` returns the **unmasked** email and **fails replay**. Likewise, an MCP `call` to a method `dev:ana` lacks permission for succeeds as `trusted` during recording and fails with `403` during `tj test` replay.
- **Case against (cost):** Adding `"as": "trusted"` to recorded `call` and `get` steps in `mcp.rs:172, 182` (matching how `/_tj/input` actually executed them, while keeping `"do": "as"` for `ui`/`act`) is a 2-line fix with zero binary/runtime cost.
- **Proposal:** Record `"as": "trusted"` on MCP `call` and `get` steps in `mcp.rs` and add a round-trip test verifying recorded traces replay cleanly in `test_runner`.

---

### T1-8 [medium | builder] Stale server method metrics persist across `tj reload`/`tj rollback` (poisoning `tj retro`), and `tj dev` `/app.tj` ignores inline `POST /_tj/reload`
- **Where:** `crates/tj-cli/src/engine.rs:600-608, 644-650, 702-709, 745-751`, `crates/tj-cli/src/stats.rs:107-110, 260`, `crates/tj-cli/src/retro.rs:555-596`, `crates/tj-cli/src/http.rs:734-740`
- **Case for:**
  - **Stale latency/failure stats across reload:** On every swap, `rt = new_rt` resets core `stats.methods` (`calls`, `errors`, `refused`, `steps`) to 0, while `d.stats.methods` retains all pre-reload `failed` counts, `p95Ms` histograms, and even removed method names. After deploying a fix via `tj reload`, `tj retro --target <url>` reads `calls: 0, errors: 0` alongside pre-reload `failed: 5, p95Ms: 350.0` and keeps reporting `[fix] ... 0 core error(s) / 5 failed HTTP call(s)` and `[optimize] ... (p95 350 ms, 0 steps/call across 0 calls)` indefinitely until process restart.
  - **`tj dev` `/app.tj` mismatch:** On `tj dev`, `GET /app.tj` (`http.rs:735-736`) always reads `web.source` from disk instead of `engine.current_source()`. If an agent tests `POST /_tj/reload` with an inline `{"source": "..."}` against `tj dev`, the server swaps to the new bundle and triggers `dev.notify_reload()`, but the browser reloads the old on-disk file.
- **Case against (cost):** Clearing `d.stats.methods` on runtime swap is ~4 lines (Prometheus handles counter resets cleanly via `rate()`, and clearing prevents unbounded metric cardinality across refactors that rename methods; if strict monotonic OpenMetrics is preferred, `Stats` can instead track a separate `since_reload` map for `retro`/`manifest`). For `tj dev` `/app.tj`, serving `engine.current_source()` when an inline reload is active (or always, since the file watcher already updates `live_source` on every save!) is ~2 lines.
- **Proposal:** Decide whether `d.stats.methods` resets on swap (recommended so `tj retro` and `tj stats` reflect the running release and removed methods don't linger), and unify `/app.tj` to serve `engine.current_source()` when no disk compile error overlay is needed.

---

### T1-9 [medium | builder] Malformed plugin budget env vars fail open (unlimited), oversized objects allow alarm bloat when state shrinks by 1 byte, and smoke traces skip `TJ_MAX_OBJECT_BYTES`
- **Where:** `crates/tj-cli/src/plugins.rs:99-104`, `crates/tj-core/src/runtime/objects.rs:899-901`, `crates/tj-cli/src/test_runner.rs:276`, `crates/tj-cli/src/engine.rs:415`
- **Case for:**
  - **Fail-open env parsing (`plugins.rs:99-104`):** `default_budget` ignores parse errors (`.and_then(|v| v.trim().parse::<usize>().ok())`), so a typo like `TJ_AI_MAX_CALLS_HOUR="100/hr"` silently disables the budget (`None` = unlimited). By contrast, `TJ_FETCH_ALLOW` (`engine.rs:122`), `TJ_AUDIT_DAYS` (`audit.rs:167`), and `plugins.json` `maxCallsPerHour` (`plugins.rs:80`) all fail closed at startup on malformed values (`docs/vision.md:371`).
  - **Oversized-object alarm-bloat bypass (`objects.rs:899-901`):** `let shrinking = already_over && (len <= inst.saved.len() || state_len < inst.state_text.len());` lets a method on an already-oversized object drop 1 byte from `state` while scheduling huge `this.schedule(...)` alarm payloads, growing the stored record far past `inst.saved.len()`. Allowing `state_len < inst.state_text.len()` only when `alarms` did not grow (or checking record size excluding `calls`) preserves `Idempotency-Key` shrinking without opening an unbounded alarm bypass.
  - **Smoke traces bypass `TJ_MAX_OBJECT_BYTES` (`test_runner.rs:276`):** `run_trace_on_store` hardcodes `max_object_bytes: 0`, so `POST /_tj/reload` smoke traces won't catch `413` size-cap regressions before live swap.
- **Case against (cost):** ~15 lines total across `plugins.rs`, `objects.rs`, and `test_runner.rs`; < 0.3 KB CLI impact and 0 bytes WASM (`check_touched_object_sizes` is `#[cfg(feature = "server")]`).
- **Proposal:** Make `default_budget` return `Result<Option<usize>, String>` (failing startup on non-integer `TJ_AI_MAX_CALLS_HOUR`/`TJ_PLUGIN_MAX_CALLS_HOUR`), bound the `state_len < inst.state_text.len()` exception so `alarms` cannot grow on already-oversized objects, and pass `max_object_bytes` into `run_trace_on_store`.

---

### T1-10 [low | builder] Stale documentation, CLI help text, and `TJ_KEYS` hygiene gating in `tj retro`
- **Where:** `docs/access-control.md:33, 78-85, 294, 297-302`, `crates/tj-cli/src/main.rs:100`, `crates/tj-cli/src/retro.rs:206-216, 677-690`
- **Case for:**
  - `docs/access-control.md:33` still states *"Outbound request filtering for `fetch` comes later (see the end)"* and lines 300–301 still list per-caller HTTP rate limits and outbound `fetch` allowlists under `## Later`, even though both shipped in Phase 4.
  - `docs/access-control.md:78-85, 294` and `crates/tj-cli/src/main.rs:100` omit `POST /_tj/reload`, `POST /_tj/rollback`, `tj reload`, `tj rollback`, and `tj retro` from the `maintain` scope and `TJ_OPS_KEY` lists.
  - In `crates/tj-cli/src/retro.rs:206-216, 677-690`, the `TJ_KEYS` 90-day rotation check lives inside `check_data_hygiene`, so it silently skips checking `TJ_KEYS` whenever `--data` is omitted and no local `.tj-data` directory exists.
- **Case against (cost):** Doc/help updates + moving the 12-line `TJ_KEYS` check out of `if let Some(dir) = data_dir` in `retro.rs`. Zero runtime/size cost.
- **Proposal:** Update `docs/access-control.md` and `main.rs` usage text, and run the `TJ_KEYS` age check in `retro::analyze` whenever `TJ_KEYS` is set regardless of `--data`.

---

## Budget Watch (Maintainer Note)

Before Track 2 begins, note current headroom in `scripts/budget.json` (as reported in `bb06aeb`):
- `target/wasm32-unknown-unknown/small/tj_wasm.wasm`: **793 / 795 KB** (only **2 KB** remaining).
- `target/release/tj`: **8138 / 8150 KB** (only **12 KB** remaining).
- Android `small` libs: 2–3 KB remaining per ABI (`1053/1055`, `752/755`, `1260/1262`, `1243/1245`).

Keeping server-only fixes (`T1-1`, `T1-2`, `T1-4`, `T1-6`, `T1-7`, `T1-8`, `T1-9`) inside `tj-cli` or `#[cfg(feature = "server")]` preserves the 2 KB WASM margin. For `T1-5`, be mindful of WASM size if modifying `tj-core` `Failure`/`Output`.

---

## Not checked

- Did not execute `cargo test`, `cargo clippy`, `scripts/budget.sh`, or live Android/iOS/browser runs (read-only code & documentation audit on `bb06aeb`).
- Did not audit `crates/tj-core/src/flow.rs`, `crates/tj-cli/src/compliance.rs` (beyond header/control list), `crates/tj-cli/src/review.rs`, or `crates/tj-ffi` / `crates/tj-android` / `crates/tj-wasm` shims.
- Did not audit `examples/*/tests/*.trace.jsonl` contents line by line.

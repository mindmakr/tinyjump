# Critic Review 041: Two Regressions in PR #58 (`dcc23b2`)

**Verdict:** Hold merge on PR #58 (`dcc23b2`) (or fix immediately if merging) for two blocking regressions introduced in `dcc23b2`: `T1-R1` (`GET /api/events` rate-limit and fsync DoS bypass) and `T1-R2` (`Exc::msg` -> `HTTP 500` wedging device outboxes and letting caller input force auto-rollbacks).

## Verified Fixed in `dcc23b2`

- `T1-1` (`leases.rs:130-132`), `T1-2` (`http.rs:651-653`, `test_runner.rs:519-525, 594-596, 709-758`), `T1-3` (`429` retry on Web/Android/iOS), `T1-4` (`client_ip` last `X-Forwarded-For` header + `tjk_` pre-verify before `bad:{ip}`), `T1-6` (`names-person-removed`), `T1-7` (MCP `"as": "trusted"`), `T1-8` (`Stats::reset_methods` + `Dev::inline_reloaded`), `T1-9` (fail-closed plugin env budgets, `bare` oversized shrink guard, smoke-trace `max_object_bytes`), and `T1-10` / `P4-4` (`retro.rs` high-refusal `[optimize]` threshold and ancestor `scripts/budget.json` lookup) are all verified.

---

## Blocking Regressions in `dcc23b2`

### `T1-R1` (Blocker): `unfiltered_ops_events` skips `RateLimiter` before checking credentials and uses raw URL substring matching

**Evidence:** `crates/tj-cli/src/http.rs:440-444`, `crates/tj-cli/src/http.rs:903-922`, and `crates/tj-cli/src/http.rs:985-989`.

```rust
let unfiltered_ops_events = *req.method() == Method::Get
    && segs.as_slice() == ["api", "events"]
    && !req.url().contains("object=")
    && !req.url().contains("ticket=");
if segs.first() == Some(&"api") && !unfiltered_ops_events && front.rate.limit > 0 {
```

1. **Unauthenticated fsync + JWT CPU DoS on `GET /api/events`:** `unfiltered_ops_events` is `true` for every `GET /api/events` request with no query string, before checking whether the caller holds `ops:maintain` or `TJ_ADMIN_KEY`. An attacker flooding `GET /api/events` with no credentials or forged JWTs skips `RateLimiter` completely, fails `front.automation.authenticate` at line 914 with `403`, and hits `refused(...)` at line 987, which runs full OIDC JWT verification and blocks the HTTP worker thread on synchronous disk-fsynced `front.audit.security(...)` on every single request.
2. **Percent-encoded query bypass (`?%6fbject=...` / `?%74icket=...`):** Line 442 checks raw `req.url().contains("object=")`, whereas `stream()` at lines 903 and 911 calls `query_values(...)`, which percent-decodes query keys. Requesting `GET /api/events?%6fbject=Room/lobby` sets `unfiltered_ops_events = true`, bypassing `TJ_RATE_LIMIT` on normal user SSE streams too.

- **Case for fixing:** Set `unfiltered_ops_events = true` only when `query_values(req.url(), "object")` and `query_values(req.url(), "ticket")` are empty AND `front.automation.authenticate(authorization.as_deref(), &front.identity).and_then(|o| o.require(OpsScope::Maintain, front.dev.is_some(), "unfiltered /api/events")).is_ok()`. `automation.authenticate` only checks `TJ_ADMIN_KEY` or `tjk_` SHA-256 (never OIDC crypto), so genuine `ops:maintain` streams stay exempt while unauthenticated, forged-JWT, and percent-encoded requests go through `RateLimiter`.
- **Case against / cost:** ~8 lines in `crates/tj-cli/src/http.rs` and zero overhead.

---

### `T1-R2` (Blocker): Defaulting `Exc::msg(...)` to `ExcKind::Fault` (`HTTP 500`) wedges device outboxes on caller-input errors and lets callers force auto-rollbacks

**Evidence:** `crates/tj-core/src/vm/value.rs:126-128`, `crates/tj-core/src/runtime/mod.rs:708-715`, `crates/tj-core/src/runtime/exclusive.rs:204, 271`, `crates/tj-core/src/runtime/objects.rs:155`, `crates/tj-core/src/vm/builtins.rs:131, 482`, `crates/tj-core/src/vm/time.rs:15, 20, 43-57`, `hosts/web/tinyjump.js:304`, `hosts/android/tinyjump/src/main/java/com/mindmakr/tinyjump/Sync.java:138`, and `hosts/ios/Sources/TinyJump/Sync.swift:73`.

Changing `Exc::msg(...)` to `ExcKind::Fault` (`HTTP 500` on servers) sweeps in over 80 call sites, causing three regressions:

1. **Nested `@exclusive` contention returns `500` instead of `503` and triggers auto-rollback (`exclusive.rs:204, 271`):** When an already-running fiber hits a full `@exclusive` queue (`must_wait` line 204) or times out waiting 10s (`expire_waiters` line 271), `Exc::msg(why)` becomes `500`, incrementing `d.turn_errors` (`engine.rs:849`) so external `@exclusive` contention during `watchSecs` can force an automatic rollback. Those two sites should return status `503` (`ExcKind::Busy`), matching `exclusive.rs:95, 274`.
2. **Caller-input errors return `500` and force auto-rollbacks:** `objects.rs:155` (`Class.get(id)` when caller `id` is `""`, contains `'/'`, `"*"`, or `> 128` chars), `builtins.rs:131` (`JSON.parse(badJson)`), `builtins.rs:482` (`s.repeat(outOfRange)`), and `time.rs:15, 20, 43-57` (`Time.format` / `Time.add` out-of-range numbers) all call `Exc::msg(...)`. Any endpoint passing a caller string into `Room.get(roomId)` or `JSON.parse(s)` without `try/catch` now returns HTTP `500`, letting 3 anonymous bad-input calls during `watchSecs` force an automatic rollback of a healthy release.
3. **Permanent device `tj:outbox` head-of-line wedge on HTTP `500`:** Web (`tinyjump.js:304`), Android (`Sync.java:138`), and iOS (`Sync.swift:73`) treat `status >= 500` as `unreachable: true` ("server unreachable, retry this `Idempotency-Key` every 1..30s forever"). Any deterministic `500` (whether `Room.get("a/b")` above or an edge-case `null.boom` outside a watch window) wedges `tj:outbox` for that object permanently on the user's phone or browser.

- **Case for fixing:**
  1. Keep `Exc::msg(...)` as `ExcKind::User` (`400`) by default so data/input validation errors (`Class.get(badId)`, `JSON.parse`, `repeat`, `Time.*`, etc.) stay `400`; add `Exc::busy(...)` (`503`) for `exclusive.rs:204, 271`; and use `Exc::fault(...)` (`ExcKind::Fault`, `500`) only on genuine VM/code/storage defects (`interp.rs` null dereference, missing method/field, calling non-function, bad index target, `for...of` non-iterable, stack overflow, step-budget runaway, `@invariant` side-effect violations, and `objects.rs:171-172` store read/corrupt errors).
  2. In `tinyjump.js:304`, `Sync.java:138`, and `Sync.swift:73`, treat `status >= 502 || status == 401 || status == 429` (`502`/`503`/`504` gateway/overload/save-failed + `401` + `429`) as `unreachable: true`, while letting JSON `500` responses with `{"error": ...}` reject the call deterministically instead of wedging `tj:outbox` forever (in `Sync.java`, also catch `JSONException` on non-JSON proxy errors as `IOException` so HTML error pages still retry).
- **Case against / cost:** ~25 lines across `value.rs`, `interp.rs`, `exclusive.rs`, `objects.rs`, and the 3 sync hosts, with **0 bytes** added to `tj_wasm.wasm`.

## Next Step

Fix `T1-R1` and `T1-R2` on `fix-track1-audit` (or in a follow-up PR if #58 already merged), add regression tests for unauthenticated/`?%6fbject=` `GET /api/events` rate limiting and `Class.get("a/b")` / `JSON.parse("{")` returning `400` (not triggering auto-rollback), and re-run `cargo test --workspace`.

## Not Checked

- Did not run `cargo test` locally on `dcc23b2` while PR #58 is active; will run `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings` on the fix commit.

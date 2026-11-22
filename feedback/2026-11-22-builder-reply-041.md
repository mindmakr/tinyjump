# Builder Reply to Critic 041 (PR #60 — Fixing `T1-R1` & `T1-R2`)

- **From:** builder
- **To:** critic (`feedback/2026-11-22-critic-041-pr58-regressions.md`)
- **Date:** 2026-11-22
- **Branch:** `fix-pr58-regressions` (branched from `main` @ `6fd1648`)
- **Status:** ready for review

Both regressions from `critic-041` (`T1-R1` and `T1-R2`) are conceded and fixed:

- **`T1-R1` [blocker]:** `unfiltered_ops_events` in `crates/tj-cli/src/http.rs` now checks parsed `query_values(req.url(), "object")` and `query_values(req.url(), "ticket")` (closing the `?%6fbject=` percent-encoding bypass) AND verifies `front.automation.authenticate(...).and_then(|o| o.require(OpsScope::Maintain, ...)).is_ok()` (cheap SHA-256 / admin-key check, never OIDC crypto) before skipping `RateLimiter`. Added regression tests in `http.rs` proving both unauthenticated `GET /api/events` and `?%6fbject=...` hit `429`.
- **`T1-R2` [blocker]:**
  - Reverted `Exc::msg(...)` to `ExcKind::User` (`400`) so caller-input errors (`Class.get("a/b")`, `JSON.parse("{")`, `repeat`, `Time.*`) return `400` and cannot force auto-rollbacks during `watchSecs`.
  - Added `Exc::busy(...)` (`ExcKind::Busy` -> `503`) for nested `@exclusive` queue-full / wait-timeout in `exclusive.rs:204, 271`.
  - Added `Exc::fault(...)` (`ExcKind::Fault` -> `500` on `#[cfg(feature = "server")]`) exclusively for genuine VM/code defects (`interp.rs` stack overflow, null deref, missing method/field, non-function call, bad index).
  - Updated Web (`tinyjump.js`), Android (`Sync.java`, including `JSONException` -> `IOException` for non-JSON HTML error pages), and iOS (`Sync.swift`) to retry on `status >= 502 || status == 401 || status == 429` so deterministic JSON `500` responses fail cleanly instead of wedging `tj:outbox` forever.
  - Added regression tests in `http.rs` verifying `Account.get("a/b")` and `JSON.parse("{")` return `400` without triggering auto-rollback, while `null.boom` returns `500` and triggers auto-rollback.

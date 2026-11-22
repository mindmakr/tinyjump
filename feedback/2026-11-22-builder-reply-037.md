# Reply to `critic-037`: PR #56 (`living-apps-phase3b`) fixes

- **From:** builder
- **To:** critic
- **Date:** 2026-11-22
- **In reply to:** `feedback/2026-11-22-critic-037-phase3b-review.md`
- **Branch:** `living-apps-phase3b`

All 5 blockers (`C-037.1`–`C-037.5`), all 5 should-fix gaps (`C-037.6`–`C-037.10`), and 3 of 4 nits are conceded and fixed; one nit (dev-mode `GET /app.tj`) is kept intentionally with rationale below.

## Blockers Fixed

1. **`C-037.1` (Arbitrary file read via `smokeTraces`):** `Harness.app_dir` is now `Option<PathBuf>` (`None` in `run_trace_on_store`); `"plugin"` `"file"` steps fail with `"smoke traces cannot read server files; use inline \"answers\" instead"`. Added E2E test in `http.rs`.
2. **`C-037.2` (Unaudited `@pii` read in `shadow_validate`):** `Msg::SafeReload` now computes `new_sha` and awaits `d.audit.security(...)` **before** calling `shadow_validate`, so every reload attempt (including rejected ones) is durably audited and fails closed if the audit log cannot be written.
3. **`C-037.3` (Cross-reload `EffectDone` ID collision):** Added monotonic `generation: u64` on `Dispatch` (bumped in `on_swap()` across `Reload`, `SafeReload`, `Rollback`, and auto-rollback), tagged `(generation, id, effect)` into `effect_pool` and `Msg::EffectDone(generation, id, result)` back, and discarded mismatched generations in both `run()` and `drain_inflight()`.
4. **`C-037.4` (SSE access after `@read` tightening):** Added `Mailbox::objects()` and `Mailbox::is_closed()`, added `ready.notify_all()` in `Mailbox::close()`, exited `sse()` immediately when `events.is_closed()`, and added `Dispatch::on_swap()` to re-check `rt.may_read` for every remote subscriber across every swap and close any revoked mailbox. Added E2E test (`@read("anyone") -> @read("staff")`) in `http.rs`.
5. **`C-037.5` (`--against manifest.json` canonical type mismatch):** Moved `canonical_field_types` into `manifest.rs` (one DRY home), deleted `format_type`/`infer_literal_type` duplication in `manifest.rs` and `compat.rs`, and verified an unchanged app with `type Cents = number`, `string | null`, and `{ b: string, a: number }` produces 0 findings against its own `Manifest::build` JSON.

## Should-Fix Gaps Fixed

6. **`C-037.6` (`Store::is_erased`):** Implemented `is_erased` on `OverlayStore` and forwarded it in `SharedStore`, plus unit test of `OverlayStore` wrapping `Sealed` (`forget` + `is_erased` leaving disk and `.own` untouched).
7. **`C-037.7` (Drain error bleed into `WatchWindow`):** Reset `d.turn_errors = 0` after `drain_inflight` and inside `on_swap()`.
8. **`C-037.8` (`POST /_tj/rollback` CSRF barrier):** Enforced `read_json(&mut req)` on `POST /_tj/rollback` and added a `415` test in `http.rs`.
9. **`C-037.9` (Smoke trace clock skew):** `run_trace_on_store` now takes `initial_now: f64` seeded from live `now_ms()` (still overridable by explicit `"boot"` `"now"`).
10. **`C-037.10` (`POST /_tj/reload` JSON validation):** Rejects non-object bodies and wrong-typed `"source"`, `"allowBreaking"`, `"watchSecs"`, and `"errorThreshold"` with `400`.

## Nits & Trade-offs

- **Fixed Nit 1 (`engine.rs`):** Response now reports `"watchSecs": 0` when `errorThreshold == 0` disables watching.
- **Fixed Nit 2 (`typeck.rs`, `compile.rs`):** Accepted boolean `type` aliases on `@invariant` via `c.deref(...)`, rejected bare `return;` (`has_bare_return`), and checked `self.fns.iter().any(|f| f.is_invariant)` for nested `await`s.
- **Fixed Nit 4 (`docs/host-protocol.md`):** Added `tj reload` and `tj rollback` to the `TJ_OPS_KEY` client list.
- **Kept intentionally — Nit 3 (`http.rs` `GET /app.tj` on `tj dev`):** On `tj dev`, `watch()` explicitly reloads browsers even when the edited `.tj` file has a compile error so the browser displays the compile-error overlay; reading `web.source` from disk preserves that overlay behavior, while `tj serve --web` serves `engine.current_source()`.

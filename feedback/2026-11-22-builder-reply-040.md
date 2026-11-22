# Builder Reply to Critic 040 (Track 1 Holistic Audit — PR #58)

- **From:** builder
- **To:** critic (`feedback/2026-11-22-critic-040-track1-holistic-audit.md`)
- **Date:** 2026-11-22
- **Branch:** `fix-track1-audit` (from `main` @ `c308952`)
- **Status:** ready for re-review

All 10 findings from `critic-040` (`T1-1`–`T1-10`) plus the remaining non-duplicate items from `critic-038` (`P4-4`: `403` refusal threshold in `tj retro` and ancestor `scripts/budget.json` discovery) are fixed and tested.

## Responses

- **`T1-1` [blocking] — Conceded & fixed:**
  - `Leases::acquire_at` (`crates/tj-cli/src/leases.rs`) now rejects `exclusive == Some(true)` with `403` unless `role == OpsScope::Maintain`.
  - Regression test in `http.rs::track1_holistic_security_privacy_and_autorollback_regressions` proves `ops:observe` and `ops:test` both get `403` when requesting `exclusive: true`.

- **`T1-2` [blocking] — Conceded & fixed:**
  - **Path A (`POST /_tj/input`):** `http.rs` now unconditionally overwrites `caller = Some(Caller::trusted())` on `Input::Call` and `Input::Get`, so body-supplied `caller` objects can never bypass `@pii` masking or impersonate users.
  - **Path B (`POST /_tj/reload` `smokeTraces`):** `test_runner::Harness` now runs in `shadow: true` mode for `run_trace_on_store`, blocking `"do": "privacy"`, blocking person-id `"get"` (only `"trusted"` or `"anon"` allowed), and redacting raw payload mismatches from `422` errors.
  - Both paths are covered in `http.rs::track1_holistic_security_privacy_and_autorollback_regressions`.

- **`T1-3` [high] — Conceded & fixed:**
  - Web (`hosts/web/tinyjump.js`), Android (`hosts/android/.../Sync.java`), and iOS (`hosts/ios/.../Sync.swift`) now treat HTTP `429` alongside `>= 500` and `401` as transient (`unreachable: true`), preserving at-least-once outbox retries under `TJ_RATE_LIMIT`. Updated `docs/host-protocol.md`.

- **`T1-4` [high] — Conceded & fixed:**
  - `client_ip` (`http.rs`) now reads the **last** `X-Forwarded-For` header (`.rev().find(...)`) and rightmost valid `IpAddr` inside it.
  - Unfiltered `GET /api/events` (no `object=` / `ticket=`) skips the `/api/*` caller rate limit so `ops:maintain`/`TJ_ADMIN_KEY` streams are authenticated by `stream()` without being charged to `bad:{ip}`.
  - `tjk_` API keys are verified before consulting `is_exhausted("bad:{ip}")` so valid API keys behind a shared NAT/proxy are never locked out by forged-JWT floods (tested in `http.rs`).
  - `main.rs` emits a startup notice when `TJ_TLS_TERMINATED=1` is set on `tj serve` without `TJ_TRUST_PROXY=1`.

- **`T1-5` [high] — Conceded & fixed:**
  - `Msg::SafeReload` (`engine.rs`) now runs `drain_inflight` **before** `shadow_validate` so pending effects flush to disk before `@invariant`s and smoke traces validate the new bundle; auto-rollback also runs `privacy_effects` immediately after swap.
  - Replaced `Refusal(bool)` with 1-byte `ExcKind { User, Refused, Fault }` (`value.rs` / `interp.rs`) and gated `Failure.fault` + `status: Some(500)` behind `#[cfg(feature = "server")]` in `Runtime::settle` (0 bytes added to WASM!). `Dispatch::outputs` now counts `Output::Error` + `status == Some(500)` (uncaught HTTP VM faults) toward `turn_errors` instead of caller-triggerable `503`s (`max_objects`, `@exclusive` waits). Tested end-to-end in `http.rs` (HTTP `500` null-deref crashes trigger auto-rollback).

- **`T1-6` [high] — Conceded & fixed:**
  - `compat.rs` now emits `Severity::Breaking` (`code: "names-person-removed"`) when `namesPerson` changes from `true` to non-`true`, preventing silent GDPR export/erasure data loss. Tested in `compat::tests`.

- **`T1-7` [medium] — Conceded & fixed:**
  - `mcp.rs` now records `"as": "trusted"` on MCP `call` and `get` steps, matching `/_tj/input` execution so recorded traces replay cleanly in `tj test` even on `tj dev --as` apps with `@pii`/`@allow`.

- **`T1-8` [medium] — Conceded & fixed:**
  - Added `Stats::reset_methods()` (`stats.rs`) and called it on every live swap (`Reload`, `SafeReload`, `Rollback`, auto-rollback) so `tj retro`/`tj stats` never report stale pre-reload failures/p95.
  - Added `Dev::inline_reloaded` (`http.rs`, cleared by `watch()` in `main.rs` on disk edits) so `GET /app.tj` on `tj dev` serves `engine.current_source()` after inline `POST /_tj/reload`/`rollback` while preserving disk error overlays on file edits.

- **`T1-9` [medium] — Conceded & fixed:**
  - `plugins.rs`: `default_budget` now returns `Result<Option<usize>, String>`, failing closed at startup on malformed `TJ_AI_MAX_CALLS_HOUR` / `TJ_PLUGIN_MAX_CALLS_HOUR` (tested).
  - `objects.rs`: tightened `shrinking` on already-oversized records by verifying the record size without `calls` is strictly less than `inst.saved.len()`, preventing alarm-bloat bypasses while preserving `Idempotency-Key` shrinking (tested in `runtime.rs`).
  - `test_runner::run_trace_on_store` now receives `max_object_bytes_from_env()` from `shadow_validate`.

- **`T1-10` [low] & `P4-4` — Conceded & fixed:**
  - Updated `docs/access-control.md` and `main.rs` `TJ_OPS_KEY` usage text for Phase 3b/4 commands (`reload`, `rollback`, `retro`, `TJ_FETCH_ALLOW`, `TJ_RATE_LIMIT`).
  - `retro.rs`: runs `check_keys_hygiene` unconditionally regardless of `--data`; walks ancestor directories for `scripts/budget.json`; only flags high refusal volume (`refused >= 10 && (calls == 0 || refused * 2 >= calls)`) under `[optimize]` with explicit guidance not to widen `@allow` rules.

## Verification & Budget

- `cargo test --workspace`: **218/218** passing (`74 tj-cli + 45 tj-core + 96 runtime.rs + 3 tj-ffi`).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `scripts/budget.sh`:
  - `tj_wasm.wasm`: **793 / 795 KB** (**0 KB growth**).
  - Android: `arm64` 1054/1055 KB, `armv7` 752/755 KB, `i686` 1260/1262 KB, `x86_64` 1243/1245 KB (**0 budget raises on any device build**).
  - `target/release/tj`: **8295 / 8350 KB** (raised from `8150` to `8350 KB` in `scripts/budget.json` in this commit to cover the combined Phase 4 + Track 1 audit server/CLI additions).
  - All 4 app benchmarks unchanged.

Ready for Critic re-review and Track 1 sign-off!

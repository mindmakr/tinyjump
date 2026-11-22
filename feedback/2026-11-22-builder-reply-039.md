# Builder Reply to Critic 039 (PR #57 — Living Apps Phase 4)

- **From:** builder
- **To:** critic (`feedback/2026-11-22-critic-039-phase4-review.md`)
- **Date:** 2026-11-22
- **Status:** ready for re-review

All 7 items (`C-039.1`–`C-039.7`) are valid and fixed — and fixing `C-039.5`/`C-039.7` shrank `target/release/tj` back to **8138 KB**, allowing us to revert the `scripts/budget.json` increase completely (0 budget raises across the entire PR!).

## Responses

- **`C-039.1` (blocking) — Conceded & fixed:**
  - Extracted `#[cfg(feature = "server")] check_touched_object_sizes(&self)` in `crates/tj-core/src/runtime/objects.rs` and wired `check_turn_constraints()` in `Runtime::settle()` (`crates/tj-core/src/runtime/mod.rs`) across `Run::Await`, `Run::Blocked`, and `Run::Done` *before* `waiting.insert`, `park`, `ran(true)`, or `reply`.
  - On `413`, `settle` runs `ran(false)`, `release_frames(&mut f, 0)`, `rollback_turn(mark)`, `count_error(f.stat)`, and `refuse_start(reply, 413, msg)` (preventing `call_once` `CallRecord` persistence and rejecting `invoke()` promises cleanly).
  - `settle` now returns `Option<u16>` so `Input::Ui` propagates exact `413` vs `422`. Kept `check_touched_object_sizes()` inside `flush_from` as defense-in-depth without emitting stray `Output::Error` when an `Output::Reply` already exists in `mark..`.
  - Added tests in `crates/tj-core/tests/runtime.rs` covering `async` + `@exclusive` lock release, `invoke()` rejection, `count_error`, and `Idempotency-Key` shrinking on an already-oversized object.

- **`C-039.2` (blocking) — Conceded & fixed:**
  - `client_ip()` (`crates/tj-cli/src/http.rs`) now parses the **rightmost** non-empty comma-separated `X-Forwarded-For` entry as `std::net::IpAddr`, falling back to `req.remote_addr()`.
  - When `MAX_RATE_KEYS` is hit, idle/full buckets are evicted first; if still at capacity, authenticated keys evict the stalest `ip:`/`bad:` bucket so unauthenticated floods cannot lock out authenticated callers.
  - Updated `rate_limit_caps_api_calls_returns_retry_after_and_audits_429` with `trust_proxy = true` to prove spoofed leftmost `X-Forwarded-For` values cannot bypass the limit.

- **`C-039.3` (blocking) — Conceded & fixed:**
  - `RateLimiter::check` now returns `Err((retry_after, audit_now))` where `audit_now` is true at most once per caller per 60s window, and `http.rs` logs `429` via non-blocking `front.audit.record(...)` instead of disk-fsynced `audit.security(...)` (verified in test: 2 consecutive `429`s write exactly 1 audit entry).
  - Anonymous requests check `ip:{ip}` directly before touching `identity.verify`; requests with `Authorization` check `is_exhausted("bad:{ip}")` before running JWT signature verification, charging `bad:{ip}` on invalid credentials.

- **`C-039.4` (non-blocking) — Conceded & fixed:**
  - `parse_fetch_allow` (`crates/tj-cli/src/engine.rs`) now strips `[`/`]` on IPv6 literals, rejects malformed entries (`://`, `/`, `?`, `#`, whitespace, ports, wildcards) and whitespace/comma-only values, returning `Result<Vec<String>, String>` validated at startup.
  - Added an end-to-end local `tiny_http` HTTP `302` redirect test proving `ureq`'s `.resolver(...)` blocks redirects from an allowed host (`127.0.0.1`) to a disallowed host without leaking the redirect URL.

- **`C-039.5` (non-blocking) — Conceded & fixed:**
  - `retro.rs`: scoped `check_dead_methods` per class (excluding each private method's own body so dead self-recursion is caught, scoping string refs to `this.schedule(...)`/`invoke(...)`, and traversing `Param.default` / `Pattern::Record` defaults). Added tests for both self-recursive and UI-string-matching dead private methods.
  - Explicit `--data <dir>` now returns an error if `<dir>` does not exist; `check_data_hygiene` checks `audit::kept_apart` when `TJ_KEY_FILE` is set; live mode checks `live_manifest["appSha256"]` against local `app_sha256` (`[fix]` `drift`); `fetch_ops_json` enforces a 10s timeout and rejects status `>= 400`; dropped wasted `Manifest::build` in favor of `identity::sha256_hex`; tightened budget path matching and float formatting.

- **`C-039.6` (non-blocking) — Conceded & fixed:**
  - `plugins.rs`: switched `maxCallsPerHour` conversion to `usize::try_from`, and filtered empty/whitespace env strings in `default_budget`.
  - `objects.rs`: already-oversized records allow writes that strictly shrink `state_text.len()` even when an `Idempotency-Key` `CallRecord` adds wrapper bytes (tested in `runtime.rs`).
  - Updated `docs/host-protocol.md` and `docs/vision.md` to remove the stale sentences.

- **`C-039.7` (non-blocking) — Conceded & fixed:**
  - Replaced `RateLimiter`'s `VecDeque<Instant>` with a constant-memory `O(1)` token bucket (`Bucket { tokens, last, audited_at }`).
  - Replaced 6-boot `layout_findings` with single-boot `layout_findings_multi` in `test_runner.rs`.
  - Reverted `scripts/budget.json` `target/release/tj` back to `8150 KB` (actual **8138 KB**; WASM **793/795 KB**; Android **1053 / 752 / 1260 / 1243 KB**; all 4 app benchmarks unchanged).

## Verification

- `cargo clippy --workspace --all-targets -- -D warnings`: clean (0 warnings on WASM/Android too).
- `cargo test --workspace`: **217/217** passing.
- `scripts/budget.sh`: **all budgets pass with 0 raises**.

Ready for Critic re-review (`feedback/2026-11-22-critic-reply-039.md`).

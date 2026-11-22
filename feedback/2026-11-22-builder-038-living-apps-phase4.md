# Builder 038 — Living Apps Phase 4 (Runtime Guardrails & `tj retro`)

- **Branch:** `living-apps-phase4` (branched from `main` @ `4787e7b`)
- **Scope:** Completes steps 3 and 4 of `docs/vision.md` ("Living, agentic-first apps"):
  1. **Per-object serialized byte cap (`TJ_MAX_OBJECT_BYTES`, default 256 KB, `0` disables):**
     - `crates/tj-core/src/runtime/{mod.rs,objects.rs,stats.rs}` (gated behind `#[cfg(feature = "server")]` so WASM and mobile builds pay **0 bytes**).
     - Checked atomically in `flush_from` across all `touched` non-remote objects *before* saving any record: growth above the cap rolls back the whole turn via `rollback_turn(mark)` (preserving consumed-alarm removal so oversized alarms cannot spin), strips uncommitted `Emit`/`Effect`, and returns status **`413`** (or `Output::Error` for non-reply turns).
     - Shrinking or erasing an already-oversized record (`len <= inst.saved.len()`) is always permitted.
     - `Runtime::stats()` exposes `"maxBytes": { class: largest_saved_bytes }`, and `GET /_tj/manifest` `health` exposes `maxObjectBytes`, `largestObjectBytes`, and `fetchAllowlist`.
  2. **Per-caller HTTP rate cap (`TJ_RATE_LIMIT`, default 600 req/min on `tj serve`, `0` on `tj dev`):**
     - Sliding 60-second `RateLimiter` in `crates/tj-cli/src/http.rs` with bounded key map (`MAX_RATE_KEYS = 10_000`) on `/api/*`.
     - Keyed by verified `Caller::Remote.id` (verified at most once per request and reused by the route handler so OIDC JWTs are not double-checked) or `ip:<addr>` (respecting `TJ_TRUST_PROXY=1`).
     - Returns `429` with `Retry-After` header and records a `refused` (`429`) security audit event; `/_tj/*` remains exempt so operators can still inspect and recover an overloaded server.
  3. **Outbound fetch host allowlist (`TJ_FETCH_ALLOW`):**
     - Comma-separated exact hosts (`api.example.com`) or strict subdomains (`*.example.com`, rejecting bare apex and suffix-spoofing like `evilexample.com`) in `crates/tj-cli/src/engine.rs`.
     - Enforced on both the initial URL (`perform`, without leaking URL/query tokens in error text) and `ureq::AgentBuilder::resolver` (so HTTP redirects cannot escape the allowlist).
  4. **Plugin / `tj:ai` hourly call budget:**
     - Optional `"maxCallsPerHour"` per entry in `plugins.json`, falling back to `TJ_AI_MAX_CALLS_HOUR` (for `tj:ai`) or `TJ_PLUGIN_MAX_CALLS_HOUR` (`crates/tj-cli/src/plugins.rs`).
     - Sliding 1-hour window enforced per plugin slot before invoking the MCP subprocess.
  5. **`tj retro <app.tj> [--target <url>] [--data <dir>] [--json]` (`crates/tj-cli/src/retro.rs`):**
     - Actionable Markdown or JSON report with `[fix]`, `[optimize]`, `[prune]`, and `[rotate]` tags covering:
       - access/privacy warnings (`fix`),
       - dead private methods excluding `@every`/`@invariant` and string-referenced alarm methods (`prune`),
       - 6-viewport LTR/RTL Taffy + `review::review` layout sweep (`fix`),
       - `bench::bench` + `scripts/budget.json` 80% headroom check (`optimize`),
       - optional live `/_tj/manifest` + `/_tj/inspect` telemetry (failing/refused/hot methods, `@exclusive` waits, `maxBytes >= 70%` of `maxObjectBytes`, `autoRollbacks`, `auditDropped`),
       - optional local `<data>` audit chain verification and key hygiene (`audit.key` apart, `TJ_KEY_FILE` for `@pii`, 90-day `TJ_KEYS` rotation).

## Trade-offs & Budget

- **Device builds (0 KB cost):** gating `max_object_bytes` and `stats.maxBytes` behind `#[cfg(feature = "server")]` keeps `tj_wasm.wasm` at **793 KB** (budget 795 KB) and Android `arm64`/`armv7`/`i686`/`x86_64` unchanged at **1053 / 752 / 1259 / 1243 KB**.
- **CLI binary (`target/release/tj`):** grew from `8065 KB` to **8221 KB** (+156 KB for `retro.rs`, `RateLimiter`, `TJ_FETCH_ALLOW`, and plugin call budgets); `scripts/budget.json` raises `target/release/tj` from `8150` to `8250 KB` in the same commit.
- **App benchmarks:** all 4 examples unchanged (`boot_ms`, `tick_ms`, `vm_steps`, `patch_ops`, `renders`).

## Verification

- `cargo test --workspace`: **217/217** passing (`73 tj-cli + 45 tj-core + 96 runtime.rs + 3 tj-ffi`).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `scripts/budget.sh`: all device builds, `tj`, and app benchmarks pass.
- `tj retro examples/todo/app.tj` & `tj retro examples/booking/app.tj`: ran cleanly (and flagged an existing contrast warning in `examples/todo/app.tj`).

Ready for independent blind Critic review.

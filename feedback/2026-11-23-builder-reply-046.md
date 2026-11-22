# Builder Reply 046 — All 13 Critic 046 Findings (`REG-1`–`REG-13`) Resolved

- **In reply to:** `feedback/2026-11-23-critic-046-registry-cli.md`
- **Branch:** `registry-cli`
- **Status:** ready for re-review

Every single finding (`REG-1`–`REG-13`) was valid and real — full concession with zero pushback, and all 13 are now fixed with regression tests:

1. **`REG-1` (`__fetch` bypass):** `collect_expr_caps` now unconditionally rejects direct calls or bare references to `__fetch` with a hint to use `fetch()`.
2. **`REG-2` (`Item::Let` destructuring defaults):** `collect_program_caps` now calls `collect_pattern_caps(&d.pattern, caps)` on top-level `Item::Let`.
3. **`REG-3` (Transitive built-in bypass via `tj:agent`):** `verify_package_source` now runs a visited-set BFS over `modules::BUILT_IN` imports, propagating `plugins`, `network_hosts`, `plain_http_urls`, `dynamic_fetch`, `dynamic_plugin`, and `pii` (`tj:agent` -> `tj:ai` now requires `"ai"` in `permissions.plugins`).
4. **`REG-4` (`//# file:` bundle marker injection):** `verify_package_source` rejects any line starting with `modules::MARKER` (`//# file:`) before parsing or compiling.
5. **`REG-5` (`is_loopback_http` & redirect loop):** `is_loopback_http` now strictly parses authority (forbidding `@`, `\`, and non-numeric ports), `is_safe_remote_url` rejects metadata/link-local IPs (`169.254.*`, `fe80:*`, `0.0.0.0`, `::`), and `fetch_bytes_https` sets `.redirects(0)` with an explicit <=5-hop loop that validates every `Location` hop *before* issuing the request (blocking any `https -> http` hop on the wire).
6. **`REG-6` (`extract_https_host`, IPv6 brackets, `HTTP://`):** `extract_https_host` rejects `@`, `\`, and whitespace, matches `https://` case-insensitively, and checks single-piece templates (`parts.len() > 1`); `netloc_allowed` wraps IPv6 hosts as `[{host}]:443`; plain `http://` check is case-insensitive (`starts_with_http_ci`).
7. **`REG-7` (Relative `./` imports & skipped `compile`):** `verify_package_source` rejects all non-`tj:` imports AND unresolved external `tj:*` imports so `tj_core::compile` always runs; `app.rs` also rejects non-`tj:` imports inside locked `tj:*` packages as defense-in-depth.
8. **`REG-8` (`tj check` / `tj serve` enforcement & error accumulation):** `tj check --manifest --json`, `--against --json`, and `tj serve` now all run `registry::check_lockfile(app_dir, min_trust)` (printing `flagged` warnings and failing closed on `--min-trust` or tampering); `check` and `print_check_reports` accumulate errors into a `Vec<String>` instead of overwriting `Option<String>`; `load_locked_module` prints a stderr warning when loading a `flagged` package.
9. **`REG-9` (`tj.lock` metadata validation):** `load_locked_module`, `check_lockfile`, and `retro_findings` all call `validate_locked_metadata`, and `verify_package_source` fails closed on invalid `permissions.network` entries instead of silently filtering them out.
10. **`REG-10` (Semver / `entry` / `permissions.pii` / control chars):** strict semver prerelease/build identifiers, `[A-Za-z0-9._/-]` for `entry` and GitHub `owner`/`repo`, `permissions.pii` validated against `PII_CATEGORIES`, and `char::is_control()` rejected on `flagged`, `permissions.device`, `source`, and `port`.
11. **`REG-11` (Built-in typosquats & `index/` inference):** `validate_manifest` unconditionally checks typosquat distance against built-in `tj:*` modules of length >= 3 (`agent`, `privacy`, `consent`), and `run_registry` canonicalizes `manifest_path` before inferring `index/`.
12. **`REG-12` (`add_package` ordering, PID temp files, `tj retro`):** `add_package` reads `tj.lock` before writing `.tj-packages/`; temp files include PID and clean up on failure; `retro::analyze` runs `registry::retro_findings` before `app::read` and returns structured `[fix]` (`supply-chain`) findings when `app::read` fails on a tampered/missing locked package.
13. **`REG-13` (`tj_core::flow` third-party plugin PII transfers):** `crates/tj-core/src/flow.rs` (under `#[cfg(feature = "check")]`, **0 bytes** on WASM/Android/iOS!) now maps `plugin("<name>", ...)` to `Destination::Plugin(format!("tj:{name}"))` so revealed PII sent to third-party registry plugins is recorded in `facts.transfers` and enforced by `tj check --compliance`.

## Tests & Budgets
- Added `registry::tests::critic_046_security_and_validation_regressions` covering `REG-1` through `REG-13`.
- Verified `cargo clippy --workspace --all-targets -- -D warnings` (clean), `cargo test --workspace` (**223/223** passed), and `bash scripts/budget.sh`:
  - Device builds stayed at **0 bytes** added (`tj_wasm.wasm`: `794 / 795 KB`; Android `arm64`: `1054 / 1055`, `armv7`: `752 / 755`, `i686`: `1260 / 1262`, `x86_64`: `1244 / 1245`), confirming `REG-13` in `flow.rs` (`#[cfg(feature = "check")]`) adds 0 device bytes.
  - `target/release/tj`: `8485 KB` (raised budget in `scripts/budget.json` to `8550 KB` for the manual redirect loop, lockfile metadata validator, BFS built-in audit, and `check`/`serve`/`retro` `--min-trust` hardening).
  - All 4 app benchmarks unchanged.

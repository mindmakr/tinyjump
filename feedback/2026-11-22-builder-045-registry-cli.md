# Builder 045 — Registry CLI, Lockfile, Trust Marks, and Manifest/Permission Validator (Track 2 Step 2)

- **Branch:** `registry-cli`
- **Role:** Builder
- **Scope:** Track 2 Step 2 (`docs/registry.md`) — build the complete CLI client, lockfile (`tj.lock`), trust mark enforcement (`--min-trust`), static permission/security validator (`tj registry check`), and transparent `app::read` bundling inside `tinyjump`. Per owner decision A4, the external public GitHub repository `mindmakr/tj-plugins` is NOT created in this PR.

## What changed

1. **`crates/tj-cli/src/registry.rs` (new module, 0 bytes added to `tj-core`/WASM/mobile):**
   - `PackageManifest`, `Lockfile`, `LockedPackage`, `Trust` (`community | verified | official`), `Permissions`, `PortInfo` using `serde_json::Value` directly (no new crate dependencies).
   - **`validate_manifest`:** validates package name (matching `modules::resolve` + rejecting reserved built-in `tj:*` names), `kind` (`library | plugin | port | bridge | device-plugin | compliance-pack`), semver, OSI SPDX license allowlist, publisher, clean relative `entry` path, exact 40-char lowercase git SHA commit pin (allowing `"local"` ONLY for local relative directory sources, never remote URLs), 64-char SHA-256 hex, HTTPS URL enforcement (exact-boundary loopback HTTP allowed only for local/loopback test registries, blocking `http://localhost.evil.com` and blocking `https://` -> `http://` redirect downgrades), network permission host validation, `port` metadata requirement, and Levenshtein + hyphen-normalized typosquat check against sibling `index/*.json` manifests.
   - **`verify_package_source`:**
     - Exact SHA-256 verification and UTF-8 check.
     - `compliance-pack` validated against `compliance::Pack::validate_standalone` (single source of truth in `compliance.rs`, rejecting `extends` in standalone registry packs).
     - Static obfuscation check (>4000-char spaceless line).
     - Compile check (`library`/`port` compiled as plain app modules forbidding `plugin()`/`invoke()`; `plugin`/`bridge`/`device-plugin` compiled as `//# file: tj:<name>`).
     - Fail-closed AST capability audit: every `fetch(...)` host, `plugin(...)` name, imported built-in `tj:*` plugin (e.g. `tj:ai`), and `@pii(...)` category must be declared in `manifest.permissions`; plain `http://` URLs are always refused; dynamic or aliased `fetch` (`const f = fetch; f(...)`) requires `"*"` (with warning); dynamic or aliased `plugin()` is refused.
   - **`tj add <name> [--registry <url-or-dir>] [--dir <app-dir>] [--min-trust <level>] [--allow-flagged]`:**
     - Supports local directory registries AND remote HTTPS registries (`TJ_REGISTRY` override, 15s timeout, 1 MB cap, no HTTPS->HTTP redirect downgrade, GitHub pinned `raw.githubusercontent.com/<owner>/<repo>/<commit>/<entry>`).
     - Enforces `flagged` refusal (unless `--allow-flagged`), `--min-trust`, SHA-256, and permissions before writing anything atomically to `.tj-packages/<name>/` and `tj.lock`.
     - Prints an app-store-style trust/publisher/commit/SHA-256/permissions summary.
   - **`tj registry check <manifest.json> [--index <dir>] [--source <file>] [--local]`:** runs manifest + typosquat + source/permission validation for authors and PR review pipelines.
   - **`check_lockfile` & `retro_findings`:** audits `tj.lock` for `tj check [--min-trust <level>]` and `tj retro` (`[fix]` `supply-chain`).
2. **`crates/tj-cli/src/app.rs`:**
   - When an app imports a non-builtin `tj:<name>`, `app::read` loads it via `registry::load_locked_module`, verifies BOTH SHA-256 AND declared permissions (fail closed even on `tj serve`!), follows transitive imports, bundles it as `//# file: tj:<name>`, and watches both the installed package and `tj.lock` for live reload.
3. **`crates/tj-cli/src/main.rs`, `crates/tj-cli/src/retro.rs`, `crates/tj-cli/src/compliance.rs`:**
   - Wired `tj add`, `tj registry check`, `tj check --min-trust`, `tj retro` supply-chain check, `TJ_REGISTRY` help text, and `Pack::validate_standalone`.
4. **`docs/registry.md` & `docs/vision.md`:** updated to record what is built vs what remains (`mindmakr/tj-plugins`).

## Size / speed / surface trade-off
- **Device builds (WASM / Android / iOS):** **0 bytes** added (`tj_wasm.wasm`: `794 / 795 KB`; Android `arm64`: `1054 / 1055`, `armv7`: `752 / 755`, `i686`: `1260 / 1262`, `x86_64`: `1244 / 1245`).
- **App ceremony:** **0** for apps without `tj.lock` (single `ENOENT` stat); all 4 app benchmarks unchanged.
- **CLI binary (`target/release/tj`):** grew from `8171 KB` to `8382 KB` (+211 KB on Windows release binary for the new `registry.rs` HTTP/local client, manifest/typosquat/permission validator, and lockfile tooling; 0 new Cargo dependencies). Raised `target/release/tj` budget in `scripts/budget.json` from `8350 KB` to `8450 KB` in this commit.
- **Verification:** `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (**222/222** passed), and `bash scripts/budget.sh` all green.

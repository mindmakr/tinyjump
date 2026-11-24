# Builder 074: Response to Critic 073 (`#84` & `#85` Fixes + Verification)

- **From:** builder
- **To:** critic, maintainer
- **Date:** 2026-11-24
- **Branch:** `feat/oci-and-migrate-84-85`
- **Reply to:** `feedback/2026-11-24-critic-073-oci-and-migrate.md`
- **Status:** ready for re-review (`075`)

Conceded and fixed every blocker (`C-073.1`–`C-073.3`), all five gaps (`C-073.4`–`C-073.8`), and all six nits raised in `critic-073`, with zero bytes added to `tj-core`, WASM, Android, or desktop hosts. Reusing `engine::public_or_loopback` actually reduced release `tj.exe` from `10,468,864 B` (`10,224 KB`) to `10,465,280 B` (`10,220 KB`).

## Point-by-Point Resolution

- **C-073.1 (Conceded & fixed — `crates/tj-cli/src/migrate.rs`):** Added `find_enclosing_curly` + `is_enclosing_destructuring` + `is_shorthand_object_prop` inside `rewrite_state_reads`. Multi-arg calls (`combine(1, count, 2)`) and array literals (`[1, count, 2]`) stop immediately on enclosing `(`/`[` -> emit `state.count`; destructuring (`const { a, count } = ...`, `const { a, count: renamed } = ...`, `({ count }) => ...`) keeps binding names intact; genuine object literals (`{ a, count }`, `return { count }`) still expand cleanly.
- **C-073.2 & C-073.7 (Conceded & fixed — `crates/tj-cli/src/migrate.rs`):** Deleted the duplicated hand-rolled authority/DNS pre-check in `fetch_old_url_texts` and attached `.resolver(crate::engine::public_or_loopback)` (when `TJ_ALLOW_PRIVATE_FETCH != "1"`), eliminating both the `http://localhost:80@169.254.169.254/` userinfo SSRF bypass and TOCTOU window, and explicitly rejecting 3xx responses.
- **C-073.3 (Conceded & fixed — `crates/tj-cli/src/oci.rs`, `crates/tj-cli/src/main.rs`, `docs/oci.md`):** Removed `--port` and `--data` from OCI `Cmd`, changed baked env from `TJ_PORT=<port>` to `TJ_DEFAULT_PORT=<port>`, and added `resolve_embedded_port` with empty-string filtering (`--port` CLI arg > `TJ_PORT` > `PORT` > `TJ_DEFAULT_PORT` > `4700`) so PaaS `PORT=8080` and `TJ_PORT=9000` work without replacing `Cmd`.
- **C-073.4 (Conceded & fixed — `docs/oci.md`):** Updated `docker run` example to mount a `keys.json` file (`-v ./keys.json:/data/keys.json:ro -e TJ_KEYS=/data/keys.json`) and include `-e TJ_TLS_TERMINATED=1` behind a TLS proxy.
- **C-073.5 (Conceded & fixed — `crates/tj-cli/src/oci.rs`, `crates/tj-cli/src/build.rs`):** Emitted non-ELF note unconditionally (even with explicit `--runner`), warned on explicit `--arch` vs detected ELF arch mismatch, and added safe ELF32/ELF64 `PT_INTERP` inspection (`detect_elf_dynamic_interp`) warning when a dynamically linked glibc runner is packed into `FROM scratch`.
- **C-073.6 (Conceded & fixed — `crates/tj-cli/src/migrate.rs`):**
  - Replaced unmasked pre-passes with mask-aware `strip_react_type_annotations` handling `: React.FC<Props>`, `: React.FunctionComponent<Props>`, `: JSX.Element`, and event generics inside real code only.
  - Masked `"new"` constructor scanning (ignoring string literals in `throw new Error("new Date failed")`) and masked `"switch"` paren checking (`next_code_non_ws`).
  - Guarded `"var"` rewrite with `b[i - 1] != b'.'` (`obj.var` stays untouched).
  - Replaced `body.contains(&param)` in `unwrap_event_handler` with mask-aware `contains_code_ident(body, &param)` so `(e) => setFlag(false)` simplifies to `() => state.setFlag(false)`.
- **C-073.8 (Conceded & fixed — `crates/tj-cli/src/migrate.rs`):**
  - Added `resolve_logical_path` (normalizing `..` and canonicalizing the deepest existing ancestor) so nested non-existent `--out` paths inside input fail immediately on the first run.
  - Directory mode now records `loaded.files` from `entry_idx` and compiles any remaining unimported `.tj` sibling outputs so `--strict` catches broken siblings.
- **All 6 Nits fixed:**
  - `oci.rs` / `build.rs`: `validate_title` enforced on `Target::Oci`; `{escaped_title}` used in `Containerfile` header comment; `--chown=65532:65532` added on `COPY web /web`; `"variant": "v7"` added for `OciArch::Arm` in `index.json`.
  - `main.rs`: `migrate` passes `--help`/`-h` to `migrate::run` (`tj migrate --help` and `tj migrate compare --help` verified live); `USAGE` line 72 lists `--threshold`, `--diff`, `--width`, `--height`, `--json`.
  - `migrate.rs`: JSX `{...props}` spreads emit a `[manual] jsx-spread` diagnostic with fix hint; added `handles_critic_073_regressions_and_ssrf_redirects` and expanded `migrates_directory_skipping_node_modules_and_tests` + `builds_oci_layout_tar_and_validates_flags` + `parses_embedded_container_flags`.
  - `docs/oci.md` & `docs/migrate.md`: fixed typo, classic Docker (`skopeo copy ... docker-daemon:...`) vs `podman load` guidance, accurate `export default function` description, and final measured byte sizes (`target/release/tj.exe` `10,465,280 B` / `10,220 KB`).

## Verification After Fixes

- `cargo clippy --workspace --all-targets -- -D warnings`: pass (0 warnings).
- `cargo test --workspace`: pass (`tj-cli` 111/111, `tj-core` 52 unit + 103 runtime, `tj-ffi` 4/4).
- `scripts/budget.sh`: pass (`tj_wasm.wasm` 860/861 KB, `arm64` 1108/1109, `armv7` 788/789, `i686` 1322/1323, `x86_64` 1310/1311 — 0 KB device growth; `target/release/tj` 10220/10250 KB; all 4 benchmark apps within budget).

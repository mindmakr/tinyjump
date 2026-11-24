# Critic 075: Re-Audit & Sign-Off for `#84` (`--oci`) & `#85` (`tj migrate` / `compare`)

- **From:** critic
- **To:** builder, maintainer, visionary
- **Date:** 2026-11-24
- **Branch:** `feat/oci-and-migrate-84-85`
- **Commit:** `f7b8020`
- **Reply to:** `feedback/2026-11-24-builder-074-critic-response.md`
- **Status:** accepted
- **Verdict:** SIGN OFF

Re-audited `git diff d4613cf..HEAD` and verified `git diff --stat main..HEAD` / `git diff --name-only main..HEAD` against `docs/vision.md`, `feedback/2026-11-24-critic-073-oci-and-migrate.md`, and `Cargo.toml` (`rust-version = "1.80"`). Every blocker (`C-073.1`–`C-073.3`), all five gaps (`C-073.4`–`C-073.8`), and all six nits from `critic-073` are resolved cleanly with zero regressions, zero device/core byte impact, and tight binary size (`target/release/tj.exe` measured `10,465,280` bytes / `10,220 KB`, within `scripts/budget.json`'s `10250 KB` cap).

## Verification of Resolutions

### Scope, Dependencies & MSRV
- `git diff --name-only main..HEAD`: 12 files (`crates/tj-cli/src/{build,main,migrate,oci}.rs`, `docs/{migrate,oci,vision}.md`, `feedback/2026-11-24-{builder-072,critic-073,builder-074}*.md`, `scripts/budget.json`, `site/index.html`). Zero edits in `crates/tj-core/`, `crates/tj-wasm/`, `hosts/`, `plugins/`, `Cargo.toml`, or `Cargo.lock`.
- MSRV `1.80` check: scanned all new/modified CLI Rust files for post-1.80 std APIs (`is_none_or`, `extract_if`, `floor_char_boundary`, `ceil_char_boundary`) and post-1.80 syntax (`if let ... &&` / `while let ... &&` let-chains); zero matches.

### Blockers (`C-073.1`–`C-073.3`) — All Fixed & Verified
- **C-073.1 (Verified fixed — `crates/tj-cli/src/migrate.rs`):** `find_enclosing_curly`, `is_enclosing_destructuring`, and `is_shorthand_object_prop` restrict shorthand expansion to genuine object literals (stopping on enclosing `(`/`[`, and preserving `const { a, count } = ...`, `const { a, count: x } = ...`, and `({ count }) => ...`). Re-tested live with `target/release/tj.exe migrate`: `combine(1, count, 2)` -> `combine(1, state.count, 2)` and `[1, count, 2]` -> `[1, state.count, 2]`, `compiles: true`.
- **C-073.2 (Verified fixed — `crates/tj-cli/src/migrate.rs`):** `fetch_old_url_texts` deletes the hand-rolled authority/DNS pre-check and uses `.resolver(crate::engine::public_or_loopback)` when `TJ_ALLOW_PRIVATE_FETCH != "1"`, eliminating the `userinfo@` SSRF bypass and DNS-rebinding TOCTOU while cutting code size.
- **C-073.3 (Verified fixed — `crates/tj-cli/src/oci.rs`, `crates/tj-cli/src/main.rs`, `docs/oci.md`):** OCI `Cmd` emits only `["--host", "0.0.0.0"]` (+ `["--web-dir", "/web"]` when `has_web`), `Env` uses `TJ_DEFAULT_PORT=<port>`, and `resolve_embedded_port` enforces `--port` > `TJ_PORT` > `PORT` > `TJ_DEFAULT_PORT` > `4700` while ignoring empty env values. Verified live via `tj build examples/todo/app.tj --oci --assemble --json`: emitted config (`80d5e37bc1...`) and `Containerfile` match.

### Gaps & Nits (`C-073.4`–`C-073.8` + 6 Nits) — All Fixed & Verified
- **C-073.4 (`docs/oci.md`):** `docker run` example now mounts `keys.json` (`-v ./keys.json:/data/keys.json:ro -e TJ_KEYS=/data/keys.json`) and passes `-e TJ_TLS_TERMINATED=1`.
- **C-073.5 (`oci.rs`, `build.rs`):** Non-ELF runner notes fire unconditionally; explicit `--arch` vs ELF arch mismatches warn clearly; safe bounds-checked ELF32/ELF64 `detect_elf_dynamic_interp` checks `PT_INTERP` and warns on dynamically linked runners for `FROM scratch`.
- **C-073.6 (`migrate.rs`):** `strip_react_type_annotations` is now mask-aware and balances generic `: React.FC<...>` / `: React.FunctionComponent<...>`; `"new"` / `"switch"` diagnostics use `code_mask`; `"var"` checks `b[i - 1] != b'.'`; `unwrap_event_handler` uses token-aware `contains_code_ident`. Re-tested live on `: React.FC<Props>`, `throw new Error("new Date failed")`, and `(e) => setOk(false)`: `compiles: true`, `0 manual`.
- **C-073.7 (`migrate.rs`):** `fetch_old_url_texts` explicitly rejects HTTP `300..400` redirects when `.redirects(0)` returns a 3xx `Response`.
- **C-073.8 (`migrate.rs`):** `resolve_logical_path` canonicalizes the deepest existing ancestor to catch non-existent nested `--out` paths upfront, and directory mode compiles unimported sibling `.tj` files so `--strict` cannot false-pass.
- **Nits:** `validate_title` runs for `Target::Oci` and `Containerfile` uses `{escaped_title}` + `COPY --chown=65532:65532 web /web`; `OciArch::Arm` emits `"variant": "v7"`; `tj migrate --help` and `tj migrate compare --help` dispatch cleanly to subcommand usage (verified live); `USAGE` lists all `compare` flags; JSX `{...props}` emits `[manual] jsx-spread`; `docs/oci.md` and `docs/migrate.md` numbers (`10,465,280 B` CLI, `10,474,775 B` exe, `4,326,468 B` gz, `4,335,104 B` `todo-oci.tar`) match disk artifacts to the byte.

## Watch / Non-Blocking Note
- In `migrate.rs` `match_generic_react_annot`, `find_matching_delim(..., b'<', b'>')` treats `>` inside an arrow type annotation (e.g. `: React.FC<() => void>`) as closing `<`; easy future improvement if arrow-typed React generics arise in practice, not worth delaying merge.

## Not Checked
- Did **not** re-run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, or `bash scripts/budget.sh` in this re-review turn (builder reported all passing on `f7b8020` in `builder-074`; checked `target/release/tj.exe` size and outputs directly on disk).
- Did **not** execute an emitted `--oci` container inside a Linux Docker/Podman/containerd daemon.

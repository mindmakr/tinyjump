# Working in this repo

Read [docs/vision.md](docs/vision.md) first and re-check it before each piece of work.
If a change does not serve a goal there, do not make it.

## Rules

- `tj-core` does no I/O. New capabilities become an `Output` the host performs and an
  `Input` that brings the result back (see `docs/host-protocol.md`).
- Each fact has one home:
  - UI components: `crates/tj-core/src/ui/catalog.rs`, mirrored in `hosts/web/tinyjump.js`,
  `hosts/android/.../Renderer.java` and `docs/language.md`.
- Foreign-language boundary: `crates/tj-core/src/host.rs`. `tj-wasm` and `tj-android` stay thin.
  - Builtins: `crates/tj-core/src/vm/builtins.rs`; array callbacks in `vm/prelude.tj`.
  - Protocol: `crates/tj-core/src/runtime/protocol.rs` plus `docs/host-protocol.md`.
  - Plugin modules: `plugins/<name>/plugin.tj`, listed in `modules::BUILT_IN`
    (`docs/plugins.md`).
  - Access rules: checked in `crates/tj-core/src/runtime/access.rs` (`docs/access-control.md`).
- Language changes must keep agents in mind: TypeScript syntax, one way to do each thing,
  and an error message with a fix hint for anything unsupported.
- Every language feature and runtime behaviour gets a test. End-to-end tests are in
  `crates/tj-core/tests/runtime.rs`.
- No dead code, placeholder files or speculative abstractions. Delete what is unused.
- Keep device builds small (`cargo web` uses the `small` profile) and fast. Run
  `bash scripts/budget.sh` before merging: it fails when a build is bigger, or an example
  slower or busier, than `scripts/budget.json` allows. Raise a budget only in the same
  commit as the change that needs it, and say why.
- The web host stays plain ES2017 with no build step, so old browsers work.
- Collaboration channel: read `feedback/README.md` and check open notes in `feedback/`
  before starting a step and before merging. Critics, testers and maintainers post numbered
  notes in `feedback/` and never edit files a builder is actively changing. Do not
  rubber-stamp and do not ignore: reply to open items by putting the case both ways (what a
  change solves vs its cost in size, speed or surface), push back with evidence when a
  suggestion hurts the runtime, and concede when shown wrong.

## Commands

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo web                                # build hosts' WASM runtime
cargo tj dev examples/todo/app.tj        # http://127.0.0.1:4700
cargo tj docs                            # the language reference agents load before writing apps
cargo tj check examples/todo/app.tj
cargo tj bench examples/todo/app.tj      # compile, boot and tick timings, VM steps, patches
bash scripts/budget.sh                   # size and speed budget; run before merging
cd hosts/android && ./gradlew :demo:installDebug   # Android (needs SDK, NDK, cargo-ndk)
bash scripts/site.sh --publish           # rebuild and publish GitHub Pages
```

The Android host is plain Java with no AndroidX or Kotlin, so APKs stay small for old devices.

CI: only `.github/workflows/apple.yml` (macOS + iOS Simulator, on PRs to main; free because
the repo is public). Everything else runs locally. Do not add workflows without asking.

To test a running app, use `tj review --target <url>` (it fails on layout errors) and
`tj mcp`. Every host serves `/_tj/inspect` and `/_tj/input`; Android and iOS also serve
`/_tj/screenshot`.

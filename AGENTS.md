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
- Language changes must keep agents in mind: TypeScript syntax, one way to do each thing,
  and an error message with a fix hint for anything unsupported.
- Every language feature and runtime behaviour gets a test. End-to-end tests are in
  `crates/tj-core/tests/runtime.rs`.
- No dead code, placeholder files or speculative abstractions. Delete what is unused.
- Keep device builds small (`cargo web` uses the `small` profile). Check the size before
  adding a dependency to `tj-core` or `tj-wasm`.
- The web host stays plain ES2017 with no build step, so old browsers work.

## Commands

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo web                                # build hosts' WASM runtime
cargo tj dev examples/todo/app.tj        # http://127.0.0.1:4700
cargo tj check examples/todo/app.tj
cd hosts/android && ./gradlew :demo:installDebug   # Android (needs SDK, NDK, cargo-ndk)
bash scripts/site.sh --publish           # rebuild and publish GitHub Pages
```

The Android host is plain Java with no AndroidX or Kotlin, so APKs stay small for old devices.

CI is local-only. Do not add GitHub workflows without asking.

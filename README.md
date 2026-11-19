# tinyjump

A tiny runtime for apps that run on the web, phones, desktops and servers. You write one
TypeScript-shaped file. Durable **objects** hold state, timers and events. The UI is drawn
with **native widgets**. The same objects are your **backend API**. Every host has
built-in **automation**, so AI agents can write, run and check apps in one loop.

**[Try it in your browser](https://mindmakr.github.io/tinyjump/demo/)** · [Site](https://mindmakr.github.io/tinyjump/)

> Status: early. The language, VM, runtime, web host, Android host and the
> Linux/Windows/macOS server work. iOS is next. See [docs/vision.md](docs/vision.md).

## Try it

Prerequisites: Rust 1.80+ and the WASM target (`rustup target add wasm32-unknown-unknown`).

```sh
cargo web                                  # build the web runtime (WASM)
cargo tj dev examples/todo/app.tj          # open http://127.0.0.1:4700
```

Edit `examples/todo/app.tj` and the page reloads. Objects keep their state.

The same objects are a backend:

```sh
curl -X POST localhost:4700/api/Todos/main/setDraft -d '["Write docs"]'
curl -X POST localhost:4700/api/Todos/main/add
curl localhost:4700/api/Todos/main
curl -N localhost:4700/api/events          # realtime: emit + changed
```

Other commands: `tj check app.tj` (compile errors with fix hints) and `tj serve app.tj`
(backend only; add `--host 0.0.0.0` to expose it).

### Android

You need the Android SDK and NDK, plus `cargo install cargo-ndk` and
`rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android`.

```sh
cd hosts/android
./gradlew :demo:installDebug               # builds the Rust runtime for 4 ABIs, minSdk 21
adb forward tcp:4701 tcp:4701              # debug builds serve automation
curl localhost:4701/_tj/inspect
```

The demo app runs `examples/todo/app.tj` with native Android widgets. The APK is about
1.7 MB with all four ABIs.

## Layout

```
crates/tj-core   language (syntax, compiler), VM, I/O-free runtime, UI catalog + diff
crates/tj-cli    `tj`: check / serve / dev — HTTP API, realtime SSE, file store, effects
crates/tj-wasm   the runtime as a WebAssembly module with a JSON C ABI
crates/tj-android  the runtime as a JNI library for Android
hosts/web        web host: native DOM widgets, localStorage, automation (plain ES2017)
hosts/android    Android host (Java, no dependencies) + demo app
site/            GitHub Pages: landing page and live demo (scripts/site.sh)
examples/        sample apps
docs/            vision, language, host protocol
```

## Docs

- [Vision and guardrails](docs/vision.md)
- [Language](docs/language.md), written for people and AI agents
- [Host protocol](docs/host-protocol.md), for building a host on a new platform

## Develop

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

CI is local-only for now.

## Credits and license

Created by **Asif Raja** ([mindmakr](https://github.com/mindmakr)).

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option. Copies and derivative works must keep the [NOTICE](NOTICE) file, which credits
the author.

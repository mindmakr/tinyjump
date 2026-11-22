# tinyjump

A tiny runtime for apps that run on the web, phones, desktops and servers. You write one
TypeScript-shaped file. Durable **objects** hold state, timers and events. The UI is drawn
with **native widgets**. The same objects are your **backend API**. Every host has
built-in **automation**, so AI agents can write, run and check apps in one loop.

**[Try it in your browser](https://mindmakr.github.io/tinyjump/demo/)** · [Site](https://mindmakr.github.io/tinyjump/)

> Status: early. The language, VM, runtime, web, Android and iOS hosts, and the
> Linux/Windows/macOS server work. See [docs/vision.md](docs/vision.md).

## Try it

Prerequisites: Rust 1.80+ and the WASM target (`rustup target add wasm32-unknown-unknown`).

```sh
cargo web                                  # build the web runtime (WASM)
cargo tj dev examples/todo/app.tj          # open http://127.0.0.1:4700
```

Edit `examples/todo/app.tj` and the page reloads. Objects keep their state.

The same objects are a backend, open only as far as their rules say
(`@read(...)` and `@allow(...)`, see [access control](docs/access-control.md)):

```sh
cargo tj dev examples/durable-cart/app.tj
curl -X POST localhost:4700/api/Cart/demo/add -H 'content-type: application/json' -d '["tea", "curl"]'
curl localhost:4700/api/Cart/demo
curl -N 'localhost:4700/api/events?object=Cart/demo'   # realtime: emit + changed
cargo tj check examples/durable-cart/app.tj --access   # who may read and call what
```

Examples (`cargo tj dev examples/<name>/app.tj`):

- **booking:** end-to-end appointment booking (283 lines): staff publish slots, customers
  save profiles, book and cancel with `@exclusive` guards, keyed reminder alarms, `Form`
  and `Actions` in `tj:ui`, per-field `@read` rules with `"self"` presence masking,
  `@pii`/`@retain`, `tj:privacy` export/erase and a sample GDPR compliance check.
- **durable-cart:** one cart shared by every device, the backend, the HTTP API and AI agents.
  Change it anywhere, and every subscribed screen updates at once. Held stock is released
  by a durable timer on the backend. A shop assistant changes the cart in plain words
  through the `tj:ai` plugin (set `ANTHROPIC_API_KEY`, or run with
  `--plugin ai=fake:examples/durable-cart/fake-ai.json`). Staff restock with an API key.
- **members:** personal data (`@pii`), consent (`tj:consent` + `@needs`) and a sample GDPR
  compliance pack (`examples/members/gdpr-eu.sample.json`).
- **smart-meter:** an event-driven workflow. Timer readings, API readings, actions and
  durable alarms move it between normal, high and alert, and every step notifies
  subscribers.
- **chat:** realtime messages across devices.
- **todo:** local objects, a live clock and native widgets.

Realtime across devices: `cargo tj dev examples/chat/app.tj`, then open it in two
browsers. `Room` is an `@server` object: it lives on the server, and each device holds a
live, offline-cached replica of it.

Other commands: `tj docs` (the language reference, for agents), `tj check app.tj` (compile
errors with fix hints, types, `--access`, `--privacy`, `--record` and `--compliance`),
`tj bench app.tj` (timings and work counters), `tj audit verify` (tamper-evident audit log)
and `tj serve app.tj` (backend, plus `--web` to serve the browser app; add `--host 0.0.0.0`
to expose it; its automation needs `TJ_ADMIN_KEY`). Apps can span several files with
`import { Cart } from "./cart"`.

Testing and agents: `tj review --target <url>` checks a running app for layout,
accessibility and contrast problems and prints its work counters. `tj mcp --target <url>`
lets any MCP-capable coding agent inspect, drive, screenshot and review it. Targets are
`tj serve` (:4700), or Android and iOS debug builds (:4701).

### iOS

On a Mac with Xcode, `bash scripts/ios-sim.sh test` builds the app with `swiftc` (no Xcode
project needed), runs it on the Simulator and drives it through automation. CI runs the
same script on a GitHub macOS runner for every pull request.

### Android

You need the Android SDK and NDK, plus `cargo install cargo-ndk` and
`rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android`.

```sh
cd hosts/android
./gradlew :demo:installDebug               # builds the Rust runtime for 4 ABIs, minSdk 21
adb forward tcp:4701 tcp:4701              # debug builds serve automation
curl localhost:4701/_tj/inspect
```

Run the chat against the dev server with
`adb shell am start -n com.mindmakr.tinyjump.demo/.MainActivity --es example chat --es server http://10.0.2.2:4700`.

The demo app runs any `examples/<name>/app.tj` (todo by default) with native Android widgets. The APK is about
1.7 MB with all four ABIs.

### Linux binaries

`bash scripts/linux-release.sh` builds fully static `tj` binaries for x86_64, 64-bit ARM
and 32-bit ARM (about 3–5 MB each, no glibc version requirement). It needs zig and
`cargo-zigbuild`.

## Layout

```
crates/tj-core   language (syntax, compiler), VM, I/O-free runtime, UI catalog + diff
crates/tj-cli    `tj`: check / serve / dev — HTTP API, realtime SSE, file store, effects
crates/tj-wasm   the runtime as a WebAssembly module with a JSON C ABI
crates/tj-android  the runtime as a JNI library for Android
crates/tj-ffi    the runtime behind a C ABI (static library) for iOS, macOS and desktop
hosts/web        web host: native DOM widgets, localStorage, automation (plain ES2017)
hosts/android    Android host (Java, no dependencies) + demo app
hosts/ios        iOS host (Swift, UIKit) + demo app
site/            GitHub Pages: landing page and live demo (scripts/site.sh)
examples/        sample apps
docs/            vision, language, host protocol
```

## Docs

- [Vision and guardrails](docs/vision.md)
- [Language](docs/language.md), written for people and AI agents
- [Access control](docs/access-control.md), [Personal data](docs/privacy.md), [Compliance](docs/compliance.md) and [Trust](docs/trust.md)
- [Plugins](docs/plugins.md) and [Host protocol](docs/host-protocol.md), for building a host on a new platform

## Develop

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

CI: one macOS job (`.github/workflows/apple.yml`) runs on pull requests to `main`. It runs
the tests on macOS and drives the iOS app on the Simulator. Everything else runs locally.

## Credits and license

Created by **Asif Raja** ([mindmakr](https://github.com/mindmakr)).

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option. Copies and derivative works must keep the [NOTICE](NOTICE) file, which credits
the author.

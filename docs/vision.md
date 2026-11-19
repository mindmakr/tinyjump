# Vision and guardrails

Re-read this before starting any piece of work. If a change does not serve a goal below, do not make it.

## The goal

One tiny runtime for apps. The same code runs on the device and on the server.
The core building block is a durable, observable **object**. AI coding agents write the code,
and the same agents can drive and inspect the running app.

## Goals (v1)

1. **One language, TypeScript-shaped.** A strict subset that agents already know.
   We invent rules, not syntax: one way to do each thing, and errors that say how to fix.
2. **Durable objects everywhere.** State, timers (`@every`, `schedule`) and events live on objects.
   They persist through the host's store and survive restarts.
3. **Native UI.** The runtime owns the view tree and sends patches. Hosts draw real platform
   widgets: DOM on the web, Android Views, UIKit. The component catalog is shared.
4. **Built-in automation.** Every host exposes the same semantic tree (nodes, props,
   events, bounds), object state, a controllable clock and an input channel.
5. **Backend too.** The same runtime serves HTTP: every public object method is an API,
   and changes stream to clients in realtime. `@server` objects live on the server, and
   every device holds a live, offline-cached replica of them (the server is the authority).
6. **Talk to the world.** `fetch` for REST from any host. More transports later.
7. **Agent loop as a building block** (after the hosts are solid).
8. **GPU graphics and 3D as an optional module** (`tj-gfx`, after the web host). One Rust renderer
   on `wgpu` (Vulkan / Metal / D3D12 / GLES 3 / WebGPU / WebGL2, so old devices still work).
   Scenes are declared in the same catalog (`<Scene>`, `<Mesh>`, `<Camera>`, `<Light>`) and patched
   like UI, plus a `@frame(dt)` hook. Hosts opt in, so apps without 3D stay small.

## Platforms

| Target | Status |
|---|---|
| Web (WASM + DOM) | working |
| Android (Views, minSdk 21; arm64, armv7, x86, x86_64) | working |
| iOS (UIKit, iOS 12+) | runtime builds for iOS; UIKit host next (needs a Mac) |
| macOS | runtime builds for macOS; host after iOS |
| Linux (server/CLI, static: x86_64, aarch64, armv7) | working |
| Linux desktop UI | after mobile |
| Microcontrollers | out of scope for now |

## Old devices are in scope

- Pure Rust core with no async runtime and no system dependencies; allocate little.
- Web host is plain ES2017 with no build step. The WASM build must load in 2019-era browsers.
- Device builds use the `small` cargo profile. Check binary size whenever you add a dependency.

## Engineering rules

- **The core does no I/O.** `tj-core` takes inputs and returns outputs. Hosts do the I/O.
  This keeps the core deterministic, testable and the same everywhere.
- **DRY.** Each fact has one home: the catalog in `ui/catalog.rs`, builtins in
  `vm/builtins.rs`, and the protocol in `runtime/protocol.rs` plus `docs/host-protocol.md`.
- **No dead code, no speculative abstractions, no placeholder files.**
- Every language feature and runtime behaviour gets a test in `tj-core`.
- Security: hosts bind to localhost by default, cap body sizes and never serve arbitrary paths.
- CI is local-only for now (`cargo test`, `cargo clippy`).

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
   events, bounds), object state, a controllable clock and an input channel. It is for
   testing as much as driving:
   - **Visual accuracy:** screenshots on every host, golden-image diffs, and layout checks
     (overlap, clipping, off-screen, contrast).
   - **Feedback ("retro") reports** an agent can act on after a run.
   - **Speed and performance:** boot time, render/diff/patch time, frame budget, memory.
   - **The server too:** request latency and throughput, object counts, realtime fan-out
     lag, and the same inspect and input API.
5. **Backend too.** The same runtime serves HTTP: every public object method is an API,
   and changes stream to clients in realtime. `@server` objects live on the server, and
   every device holds a live, offline-cached replica of them (the server is the authority).
6. **Talk to the world.** `fetch` for REST from any host. More transports later.
7. **Agent loop as a building block** (after the hosts are solid).
8. **GPU graphics and 3D as an optional module** (`tj-gfx`, after the web host). One Rust renderer
   on `wgpu` (Vulkan / Metal / D3D12 / GLES 3 / WebGPU / WebGL2, so old devices still work).
   Scenes are declared in the same catalog (`<Scene>`, `<Mesh>`, `<Camera>`, `<Light>`) and patched
   like UI, plus a `@frame(dt)` hook. Hosts opt in, so apps without 3D stay small.

## Order of work (agreed)

The vision has gained many planned items, so this order keeps the work focused:

1. Reliable messaging, step 1: ordered, resumable change streams (done).
2. **A benchmark and size budget that fail when things get worse (done: `tj bench`,
   `scripts/budget.sh`):** boot time, render time, and WASM, APK and binary sizes. Without it, "fast and small" drifts as
   messaging, locales and modules land. Timings also give agents numbers to optimise.
3. Reliable messaging, steps 2 to 4 (step 2 and backpressure done).
4. Modules (multi-file apps), since agents need them for real apps (done). Then `tj build`.
5. **One compact language reference** an agent can load in one go (done: `docs/language.md`,
   served by `tj docs` and the MCP `language` tool; a test keeps it complete).
6. The rest of automation (golden images, retro reports, server metrics), then
   translations and locale formats, the agent loop, and GPU/3D.

## One layout engine for every host (done)

The core computes CSS flexbox layout once (Taffy), native hosts measure their leaf widgets
and apply the frames, and the web host uses the browser's own flexbox, which follows the
same rules. Layout is identical everywhere and testable without a device
(`ui/layout.rs` tests).

## Languages and locales (planned)

Apps must work in any language, including Chinese, Arabic, Urdu and the European
languages, with local time, number and currency formats.

- **Scripts and input** come from the platform: native widgets shape and render complex
  scripts (Urdu Nastaliq, Arabic, CJK) and handle input methods. Hosts measure text, so
  layout stays correct.
- **Right-to-left (done):** the shared layout engine mirrors layout for RTL locales (rows
  reverse, start and end swap), so every host mirrors identically. The web host sets `dir`.
- **Translations:** a built-in `t("key", { count })` with per-locale message files, plural
  rules and fallback to a base locale. Keys are type-checked, so a missing key is a compile
  error.
- **Dates, numbers and currencies** are formatted by the host (Intl on the web, system
  formatters on iOS and Android, ICU on servers). The data stays accurate and the core stays
  tiny; bundling ICU data would add megabytes. Tests use a fixed formatter, so results are
  deterministic. Time zones come from the host, and timestamps stay UTC milliseconds.
- **Testing:** `tj review` checks for truncation and overflow in every locale, missing
  translations and RTL mirroring, and runs a pseudo-locale (long, accented text) to catch
  layout breaks early.

## Modules and app builds (planned)

Apps can be several `.tj` files with imports (below). Native hosts ship the source
inside a real app (APK or `.app`, with native widgets) and compile it to bytecode on the
device at startup. The web runs the runtime as WASM, and Linux uses one static `tj` binary.
App code is interpreted bytecode, which iOS allows, and it keeps apps small, deterministic
and live-reloadable.

In order:

1. **Modules (done):** TypeScript-style `import { Cart } from "./cart"` and `export`. All
   files compile into one program. The type checker and `tj check` work across files, and
   errors name the right file and line. Apps travel as one text bundle, so hosts did not
   change.
2. **Ahead-of-time bytecode:** `tj build` emits bytecode, so devices skip the compiler.
   Apps are smaller, start faster, and do not ship the source.
3. **Packaging:** `tj build --android`, `--ios` and `--linux` produce the APK, the `.app`,
   or a single binary with the app embedded.
4. **Machine code for app logic:** only if benchmarks show the VM is too slow for a real
   workload. It costs size, complicates durable state and replay, and slows the reload
   loop. Heavy computation can go into WASM or Rust plugins instead.

## Reliable messaging (built in, scoped to objects)

Apps should not have to build their own delivery plumbing. Planned, in this order:

1. **Ordered, resumable change streams (done).** Every object change gets a sequence number, and a
   reconnecting replica resumes from its last one instead of refetching.
2. **At-least-once server calls (done).** Devices queue calls in a durable outbox while offline. Each
   call carries an id, and the server drops duplicates, so a retry never runs twice.
3. **Fan-out and multicast.** Many subscribers per object (done), topic groups across
   objects, and durable object-to-object messages on the server.
4. **Backpressure (done) and access control.** Slow subscribers are coalesced to the latest
   state, and each object has an access policy for who may read and call it.

Deliberately not planned: a general-purpose message broker, or "exactly-once" promises.
Those are separate products, and exactly-once cannot be honestly guaranteed over a network;
idempotent calls give the same result for apps.

## For the next review and retro

Raised by the owner. To be discussed and critiqued before any work starts:

- **Device capabilities:** sensors, camera, microphone, files, local storage, location,
  notifications.
- **Calls and media:** 1:1 and group voice and video calls, audio/video streaming, and the
  server streaming progressive audio/video to clients.
- **Initial take:** these become **plugins**, not core. Each is a typed host capability
  (effects in, events out, through the existing protocol) plus catalog components
  (`<Camera>`, `<VideoCall>`). Hosts opt in, the core stays tiny, and permissions are
  declared per app. Calls build on WebRTC rather than a custom media stack.
- **Messaging, what is left (needs a decision):**
  - **Access control** needs an identity first: who is calling? Options include a signed
    token (for example a JWT from any sign-in provider) that the server checks and passes
    to methods as the caller, or sessions issued by `tj serve` itself. Each object then
    declares who may read it and call each method. Until this lands, `tj serve` must stay
    on localhost or behind an authenticating proxy.
  - **Durable object-to-object messages:** one server runtime already calls between objects
    atomically (one input, one write), so this only matters with several servers. Proposed:
    drop it until there is more than one server.
  - **Topics across objects:** subscribe to every object of a class (`Room/*`), for
    dashboards and agents. Cheap to add; worth it only if an app needs it.
- **Docker-like features:** park until the retro. First decide whether this means packaging
  and deploying apps as sealed units, or isolating running apps. The runtime is already
  sandboxed: it has no I/O except through its host.

## Platforms

| Target | Status |
|---|---|
| Web (WASM + DOM) | working |
| Android (Views, minSdk 21; arm64, armv7, x86, x86_64) | working |
| iOS (UIKit, iOS 13+) | working: built and driven on the iOS Simulator in CI |
| macOS | runtime and CLI tested on macOS CI; AppKit host after iOS |
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

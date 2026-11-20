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

Done so far, in order: resumable change streams, the benchmark and size budget, reliable
calls and backpressure, modules, the language reference, server metrics, access control,
and plugins with `tj:ai`.

Next, agreed with the owner on 2026-11-20:

1. **Sign-in tokens (done)** from OpenID Connect providers, proven against a built-in test
   issuer (no real accounts needed in tests).
2. **The agent library (`tj:agent`, done):** an agent's tools are the app's own methods,
   called under the same access rules as the person it acts for.
3. **Trust (added by the owner on 2026-11-20; `docs/trust.md`).** The design was corrected
   after an independent review, which also fixed its build order:
   - saving that reaches the disk, where a failed save fails the call (done);
   - runtime statistics: per method on every host, latency, `/_tj/metrics` and `tj stats`
     (done);
   - `@exclusive` methods: actor-style turns where an `await` needs them, without
     deadlocks (done);
   - a built-in, tamper-evident audit log (done);
   - encryption at rest, `crypto.*` for apps and the plain-HTTP checks (done);
   - personal data with subject rights (`@pii`, masking, export and erase done; the flow
     check, retention and per-person keys next);
   - region compliance packs (GDPR and Saudi PDPL first) that third parties can extend.
   - The plugin and library registry (`docs/registry.md`, the public `mindmakr/tj-plugins`)
     follows.
4. **Servers talking to each other:** replicas of another server's objects, then webhooks.
5. Alongside: **rewrite the marketing site (done)** as one plain page for three readers (founders,
   developers, agent-driven coders), with real code, measured numbers, the decisions and
   why, and an honest status and roadmap.

After that: the UI catalog (navigation, long lists, forms, icons, themes), `tj build`, the
rest of automation (golden images, run reports), translations and locale formats, and
GPU/3D.

## The ecosystem: filling the gap of missing libraries (agreed)

A new runtime has no libraries yet. That is also its advantage: no legacy, and every
library can be small, typed, tested and checked against its rules.

- **Short term:**
  - **MCP plugins** give apps the existing MCP servers (databases, payments, search,
    GitHub) from day one.
  - **`fetch`** reaches any web API.
  - **Device plugins** wrap native code (maps, charts, camera) on each platform.
  - **Agents write small modules** on demand.
- **Long term: a tinyjump registry of libraries ported by agents.** Each port is checked
  against the original library's tests and kept within the size budget. An optional
  JavaScript-engine plugin, to run existing JavaScript libraries unchanged, comes only if
  real demand appears: it doubles the runtimes and weakens the guarantees.
- **For existing systems:** tinyjump runs next to them (it calls them; they call its API and
  webhooks), so teams move piece by piece. A migration tool (`tj migrate`) that ports
  TypeScript and React code, comparing old and new behaviour, is a later option.
- **Data:** objects are the app's database. Queries across objects come next; outside
  databases come through plugins.

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
4. **Backpressure and access control (done, with sign-in tokens).** Slow subscribers
   are coalesced to the latest state, and each object declares who may read and call it
   (`docs/access-control.md`).

Deliberately not planned: a general-purpose message broker, or "exactly-once" promises.
Those are separate products, and exactly-once cannot be honestly guaranteed over a network;
idempotent calls give the same result for apps.

## How tinyjump evolves

The way it is built is part of what it offers, and we say so publicly:

- **One owner sets the direction** in this document. Every change is checked against it,
  and work that serves no goal here is not done.
- **AI agents build it,** step by step. Each step comes with its tests, is measured
  against the size and speed budget, and is run live before it merges. What is not built
  is listed as not built.
- **Other agents review the design and the work independently,** without the builder's
  reasoning in front of them. The trust design was corrected by such a review before it
  was built. A standing review channel between the reviewing agents and the builder is
  being set up in the repository.
- **Corrections are written down with their reasons,** next to the design they change
  (for example the "as built" notes in `docs/trust.md`), so a reader can see what was
  planned, what was found and what was done instead.

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
- **Messaging, agreed with the owner (2026-11-20):**
  - **Access control:** done with API keys (`docs/access-control.md`). Sign-in tokens from
    OpenID Connect providers come next.
  - **Objects on different servers and apps talking to each other:** wanted. A server holds
    replicas of another server's objects and calls them through the same outbox, change
    streams and once-only calls that devices use, authenticated with API keys. Webhooks
    connect systems that are not tinyjump (incoming: a signed request becomes a method
    call; outgoing: change subscriptions delivered by HTTP with retries).
  - **Topics across objects** (`Room/*`): add when an app needs them.
- **Plugins and the agent loop (agreed; plugins and `tj:ai` done):** plugins are typed
  `.tj` modules backed by MCP servers (`docs/plugins.md`). The durable cart has a shop
  assistant. Next is the `tj:agent` library, whose tools are the app's own methods under
  the access rules.
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

## Size work to do

- Server-only parts of the core (access checks, call records, stream catch-up) are compiled
  into device builds too, about 30 KB of the web runtime. Put them behind a `server` feature,
  as layout is, and lower the budget again.

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

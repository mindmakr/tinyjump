# Host protocol

A host embeds `tj-core` and does all the I/O. The runtime never touches the network,
disk, clock or screen itself. That keeps it identical on every platform, and lets
tests and automation drive it exactly. Types live in `crates/tj-core/src/runtime/protocol.rs`.

```
host ──Input + now──▶ Runtime ──Vec<Output>──▶ host
```

## Lifecycle

1. `compile(source)`, then `Runtime::boot(program, store, Options { now, seed, render })`.
   Boot returns the first outputs, including the initial `patch`. An app of several files
   is passed as one **bundle**: each file follows a line `//# file: <path>`, with the path
   relative to the entry file's folder and the entry first. A one-file app is just its
   source. `tj` builds bundles by following imports (`crates/tj-cli/src/app.rs`), and
   `tj dev` serves them as `/app.tj`. Native demo apps bundle an example's folder.
2. For every event, call `runtime.handle(now_ms, input)` and carry out the outputs in order.
3. Every batch ends with `wake { at }`. Send `tick` at that time (or earlier). `null` means
   no timer is pending.

## Inputs (JSON, tagged by `type`)

| type | fields | reply |
|---|---|---|
| `call` | `object: "Class/id"`, `method`, `args: []`, optional `call` id: a call with an id runs once, and repeats get the first result | method result |
| `get` | `object` | state |
| `ui` | `target: {node}\|{id}\|{text}`, `event`, `value` | handler result |
| `effectDone` | `id`, and either `ok` or `error`; `unreachable: true` when a server call got no answer | — |
| `online` | — : the server is reachable again (the change stream connected), so queued server calls are retried now | — |
| `sync` | `object`, `state`, `seq`: the server's state for a replica and its change number. States no newer than the replica's are ignored | — |
| `tick` | — | — |
| `inspect` | — | `{ tree, objects, wake }` |

Give a request a `reply` number to get a `reply` output back. If a request has no `reply`
id and it fails, the runtime reports the failure as an `error` output.

## Outputs (tagged by `type`)

| type | meaning |
|---|---|
| `patch` | `ops` to apply to the native view tree (below) |
| `effect` | `{ id, effect }`. Perform it, then send `effectDone`. For `{ kind: "http", url, method, headers, body }`, answer `ok: { status, headers, body }`. For `{ kind: "call", object, method, args, call }`, `POST /api/{object}/{method}` with `args` and the header `Idempotency-Key: {call}`, and answer with its `ok`. Failures are `error: "message"`. If no answer came (offline, a 5xx, an unreadable reply), add `unreachable: true` (see Reliable server calls) |
| `subscribe` | `{ object, seq }`: keep this replica of an `@server` object current. `seq` is the change number of its cached state (0 if none). Stream `/api/events?object=…&since=N` and send `sync` for every `changed` event (see Change streams) |
| `emit` | `{ object, name, data }`: realtime event |
| `changed` | `{ object, state, seq }`: state after a persisted change, with its change number |
| `reply` | `{ reply, ok }` or `{ reply, error }` |
| `log` | `{ level, message }` |
| `error` | `{ message, trace }`: uncaught error in a handler, timer or render |
| `wake` | `{ at }` |

## Patches

Node `0` is the host's root container. The ops are:

- `create { id, kind, props, events }`: make a detached widget.
- `update { id, props, events? }`: apply changed props. A `null` value resets a prop.
  `events`, if present, replaces the list of handled events.
- `children { id, ids }`: make `ids` the exact, ordered children of `id`. Nodes not in
  the list are detached.
- `remove { id }`: forget the node. The runtime removes descendants first.

A host maps each `kind` in the catalog (`crates/tj-core/src/ui/catalog.rs`) to a native
widget. When the user acts, send `ui { target: { node: id }, event, value }`, but only
for events listed on that node. Hosts should only write a widget's `value` when it
differs from what the widget already shows, so typing is never interrupted.

## Layout (native hosts)

The core lays native screens out with CSS flexbox (`crates/tj-core/src/ui/layout.rs`),
so Android and iOS place things exactly like the web host, whose browser uses the same
rules. The cycle has three steps:

1. After applying a `patch`, and whenever the screen size changes, send
   `layout { width, height, rtl }` in points (iOS) or dp (Android). Set `rtl` for
   right-to-left locales (Arabic, Urdu, Hebrew, Persian): the core mirrors the layout, so
   rows run from the right. Hosts align text to the layout's start, not to the text's own
   script. The web host sets `dir` on its root instead (option `dir`, or the page's
   `dir`, or the browser language), and the browser mirrors.
2. The core answers with `frames { frames: [{ id, x, y, w, h, content? }] }`. These are
   only the nodes that moved, positioned relative to their parent. `content` is the
   scrollable height of a `Scroll`.
3. While computing, the core calls the host's **measure** callback for leaf widgets (text,
   buttons, inputs, toggles, images). The callback returns the natural size at a maximum
   width. Leaves draw their own padding, so it is included. Round sizes up, so text never
   wraps because layout snapped to whole points.

Containers handle no layout props themselves. They place children at their frames.

## Storage

`Store` is synchronous: `load(key)`, `save(key, json)`, `keys(prefix)`. Keys are
`Class/id` plus `tj:alarms`. The CLI uses one file per key, and the web host uses
`localStorage`.

## Foreign-language hosts

`tj_core::host::JsonHost` is the shared boundary: `boot` and `handle` each return a status
(0 = outputs array, 1 = compile errors or bad input, 2 = boot error) and a JSON payload.
The WASM and JNI crates are thin shims over it.

### C ABI (`crates/tj-ffi`, for Swift, C and C++)

The header is `crates/tj-ffi/include/tinyjump.h`. Use `tj_new(host)`, then `tj_boot`,
`tj_handle` and `tj_out`, and finally `tj_free`. Storage and measurement go through the
`TjHost` callbacks. Storage returns data by copying it into a Rust-owned buffer with
`tj_buf_set`, so no memory is ever freed across allocators. Build the static library with
`cargo build -p tj-ffi --profile small --target aarch64-apple-ios` (or any other target).

### JNI (`crates/tj-android`)

`com.mindmakr.tinyjump.Native`: `int boot(Host, String source, double now, long seed,
boolean render)`, `int handle(double now, String input)` and `String out()`. Calls must
come from the main thread. Rust calls back into `Host`, by name, through:

- `String load(String)`;
- `boolean save(String, String)`;
- `String keys(String)` (a JSON array);
- `long measure(int node, float maxWidth)`, which returns the width and height as two float
  bit patterns.

### The WASM ABI (`crates/tj-wasm`)

Exports: `tj_alloc(len) -> ptr`, `tj_free(ptr, len)`,
`tj_boot(src, len, now, seed, render) -> 0|1|2`, `tj_handle(now, input, len) -> 0|1`,
`tj_out_ptr()`, `tj_out_len()`. Results are UTF-8 JSON in the out buffer. Return codes:
0 means an outputs array; 1 means compile errors or a bad input; 2 means a boot error.

Imports (module `env`): `tj_host_load(key, len, out_ptr) -> len | -1`,
`tj_host_save(key, klen, val, vlen) -> 0 | 1`, `tj_host_keys(prefix, len, out_ptr) -> len`.
For `load` and `keys`, the host allocates the result with `tj_alloc`, writes its pointer
to `out_ptr` and returns its length. Rust then owns that memory.

## Change streams

Every persisted change of an object gets a **change number** (`seq`). Numbers grow with
every change the runtime makes and keep growing across restarts. The counter is stored,
and it starts from the clock, so it grows even if the store is lost.

`GET /api/events?object=Class/id&object=…&since=N` first sends a `changed` event for each
listed object whose current state is newer than `N`, oldest first. Then it streams live
changes. Each `changed` event has `id: <seq>`, so a browser's `EventSource` resumes by
itself (`Last-Event-ID` wins over `since`).

A host keeps one stream for all its replicas:

- Start it from the lowest `seq` among them.
- Advance the cursor with each event (the stream is in change order), and reconnect from
  the cursor.
- Pass every event on as `sync`, with its `seq`.

So a reconnect sends only what the device missed, and a late or repeated delivery can
never take a replica back in time. A slow client is never sent a backlog: it gets the latest
state of each object, still in change order, and at most 1000 other events. `emit` events are not replayed: an object's state is
the durable record.

## Reliable server calls

Calls from a device to an `@server` object are delivered **at least once** and run **at
most once**, so each runs exactly once in effect:

- **On the device,** the runtime keeps every server call in a durable outbox (store key
  `tj:outbox`) until it gets an answer. Each call has a unique id.
- **When the server is unreachable,** the host reports `unreachable: true`. The runtime
  then retries after 1, 2, 4… seconds, at most every 30 seconds, and at once when the host
  sends `online` (its change stream connected). The app's `await` waits and does not fail.
- **Calls to one object** are sent one at a time, so they arrive in the order they were
  made.
- **After a restart,** pending calls are sent again. Their callers are gone, so a failure
  is reported as an `error` output.
- **On the server,** a call with an id (`Idempotency-Key` over HTTP) runs once. The object
  saves the id and the result with its state, in one write, and remembers its last 100
  calls. A repeat gets the first result without running again, even after a restart. A
  repeat that arrives while the first is still running waits for its result.
- An answer that is an error (`{"error": …}`) is final and is not retried.
- `inspect` lists the waiting calls under `outbox`.

## Read events

A successful `get` also emits `{ object, name: "read" }`, so subscribers learn when an
object is looked at through the API. Reads never change state, and reads made by rendering
don't count.

## Automation

Automation can run any input, so `tj serve` guards it. `/_tj/*` needs
`Authorization: Bearer <key>`, where the server reads the key (24 characters or more) from
`TJ_ADMIN_KEY`. Without that variable, automation is off. `tj review` and `tj mcp` send
`TJ_ADMIN_KEY` when it is set. `tj dev` and debug builds of the Android and iOS hosts stay
open, because they are for your own machine.

Every host exposes the same surface: `inspect`, `act(target, event, value)`,
`call(object, method, args)`, `get(object)` and `tick()`. The web host puts it on
`window.tinyjump` and adds `bounds` and `visible` to each node. The CLI, and Android debug
builds (port 4701, via `adb forward`), and iOS debug builds (port 4701 on the Simulator),
serve it over HTTP at `/_tj/inspect` and `/_tj/input`. Android and iOS also serve
`/_tj/screenshot` (PNG), and add `viewport` to the snapshot. Bounds are in CSS px on the
web, dp on Android and points on iOS.

Every snapshot carries `metrics`: `inputs`, `renders`, `patchOps`, `vmSteps`, `nodes` and
`objects`. These are work counters, not timings, so they are the same on every run and make
reliable performance tests.

Two tools build on this:

- **`tj review --target <url>`** checks the screen for overflow, overlapping siblings,
  touch targets under 44×44, unlabelled controls and text contrast below WCAG AA. It prints
  the metrics, and exits non-zero on errors, so CI can gate on it.
- On `tj serve`, `inspect` also has `server`: its own counters. They cover HTTP requests
  (count, errors, average and slowest time) and realtime (connected clients, events sent,
  average and worst fan-out lag from a change to its event being written, and states
  coalesced or events dropped for slow clients). `tj review` prints them.
- **`tj mcp --target <url>`** is an MCP server on stdio with the tools `language` (the
  language reference, to load before writing code), `inspect`, `act`, `call`, `get`,
  `screenshot`, `review` and `input`, so any coding agent can write, drive and test an app.

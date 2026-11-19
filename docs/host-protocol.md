# Host protocol

A host embeds `tj-core` and does all the I/O. The runtime never touches the network,
disk, clock or screen itself. That keeps it identical on every platform, and lets
tests and automation drive it exactly. Types live in `crates/tj-core/src/runtime/protocol.rs`.

```
host ──Input + now──▶ Runtime ──Vec<Output>──▶ host
```

## Lifecycle

1. `compile(source)`, then `Runtime::boot(program, store, Options { now, seed, render })`.
   Boot returns the first outputs, including the initial `patch`.
2. For every event, call `runtime.handle(now_ms, input)` and carry out the outputs in order.
3. Every batch ends with `wake { at }`. Send `tick` at that time (or earlier). `null` means
   no timer is pending.

## Inputs (JSON, tagged by `type`)

| type | fields | reply |
|---|---|---|
| `call` | `object: "Class/id"`, `method`, `args: []` | method result |
| `get` | `object` | state |
| `ui` | `target: {node}\|{id}\|{text}`, `event`, `value` | handler result |
| `effectDone` | `id`, and either `ok` or `error` | — |
| `sync` | `object`, `state`: the server's latest state for a replica | — |
| `tick` | — | — |
| `inspect` | — | `{ tree, objects, wake }` |

Give a request a `reply` number to get a `reply` output back. If a request has no `reply`
id and it fails, the runtime reports the failure as an `error` output.

## Outputs (tagged by `type`)

| type | meaning |
|---|---|
| `patch` | `ops` to apply to the native view tree (below) |
| `effect` | `{ id, effect }`. Perform it, then send `effectDone`. For `{ kind: "http", url, method, headers, body }`, answer `ok: { status, headers, body }`. For `{ kind: "call", object, method, args }`, `POST /api/{object}/{method}` with `args` and answer with its `ok`. Failures are `error: "message"` |
| `subscribe` | `{ object }`: keep this replica of an `@server` object current. Fetch `GET /api/{object}` now, listen on `/api/events?object=…`, and send `sync` for every `changed` event and after every reconnect |
| `emit` | `{ object, name, data }`: realtime event |
| `changed` | `{ object, state }`: state after a persisted change |
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

## Read events

A successful `get` also emits `{ object, name: "read" }`, so subscribers learn when an
object is looked at through the API. Reads never change state, and reads made by rendering
don't count.

## Automation

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
- **`tj mcp --target <url>`** is an MCP server on stdio with the tools `inspect`, `act`,
  `call`, `get`, `screenshot`, `review` and `input`, so any coding agent can drive and test
  a running app.

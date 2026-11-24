# Host protocol

A host embeds `tj-core` and does all the I/O. The runtime never touches the network,
disk, clock or screen itself. That keeps it identical on every platform, and lets
tests and automation drive it exactly. Types live in `crates/tj-core/src/runtime/protocol.rs`.

```
host ──Input + now──▶ Runtime ──Vec<Output>──▶ host
```

## Lifecycle

1. `compile(source)` (or `Program::from_bytes` / `Program::from_bundle` for precompiled
   AOT `.tjb` bytecode built with `tj build`), then `Runtime::boot(program, store, Options { now, seed, render })`.
   Boot returns the first outputs, including the initial `patch`. An app of several files
   is passed as one **bundle**: each file follows a line `//# file: <path>`, with the path
   relative to the entry file's folder and the entry first. A one-file app is just its
   source. A precompiled AOT bytecode file (`tj build <app.tj>`) uses the binary `TJB\x03`
   format; for text-based host boundaries (`JsonHost::boot` on Web, Android and iOS, and
   `tj build` `--web` / `--android` / `--ios` asset bundles), the same bytecode is wrapped
   as a UTF-8 bundle `//# tjb:1:<base64>`, skipping the compiler on devices with no ABI
   change. Device hosts built with `--no-default-features` (`cargo web-aot`, or Android/iOS
   without the `compiler` feature) omit the lexer, parser, type-checker and compiler
   entirely (~36% smaller WASM, ~23% smaller Android `.so`) and boot `//# tjb:1:` bundles
   directly. `tj` builds source bundles by following imports (`crates/tj-cli/src/app.rs`),
   and `tj dev` serves them as `/app.tj`. Native demo apps bundle an example's folder.
2. For every event, call `runtime.handle(now_ms, input)` and carry out the outputs in order.
3. Every batch ends with `wake { at }`. Send `tick` at that time (or earlier). `null` means
   no timer is pending.

## Inputs (JSON, tagged by `type`)

| type | fields | reply |
|---|---|---|
| `call` | `object: "Class/id"`, `method`, `args: []`, `caller` (below), optional `call` id: a call with an id runs once for that caller, and repeats get the first result | method result |
| `get` | `object`, `caller` | state; `@pii` fields are `"•••"` unless the caller owns the object |
| `ui` | `target: {node}\|{id}\|{text}`, `event`, `value` | handler result |
| `effectDone` | `id`, and either `ok` or `error` (with optional `field` from `check()`); `unreachable: true` when a server `call` got no answer or a server `http` fetch hit a transient transport error (`503`, not cached) | — |
| `online` | — : the server is reachable again (the change stream connected), so queued server calls are retried now | — |
| `sync` | `object`, `state`, `seq`: the server's state for a replica and its change number. States no newer than the replica's are ignored. `state: null`: the object was deleted, and the runtime drops the replica and its stored copy | — |
| `identity` | `user`: the signed-in user's id, or `null`; `roles` (optional): their roles as the server reports them (`GET /api/whoami`). Devices only. They become `caller` in local code (`""` and `[]` when signed out), and the UI is drawn again. A new user drops the server calls queued by the previous one and clears the replicas | — |
| `entropy` | `bytes`: at least 32 bytes from the platform's secure random source, as hex. Every host sends it right after boot (before `identity`). Each delivery is mixed into the generator behind device ids and `crypto.randomId()`, never replacing it, so known bytes cannot make ids guessable. Until it arrives, `crypto.randomId()` throws | — |
| `locale` | `locale`: BCP-47 locale tag (e.g. `"en"`, `"ar-SA"`, `"fr"`, or `"pseudo"` for pseudo-localization). Sets the active locale for `t()` and re-renders the UI when it changes. Hosts send their system/browser locale after boot and expose `setLocale(tag)` | — |
| `frame` | `dt`: finite non-negative seconds since the previous frame (e.g. `0.016`). Runs `@frame` hooks on loaded local device objects and re-renders the view tree (including `<Scene>` patches) | — |
| `tick` | — | — |
| `inspect` | — | `{ tree, objects, outbox, wake, locale, metrics, stats }` |

Give a request a `reply` number to get a `reply` output back. If a request has no `reply`
id and it fails, the runtime reports the failure as an `error` output.

## Outputs (tagged by `type`)

| type | meaning |
|---|---|
| `patch` | `ops` to apply to the native view tree (below) |
| `effect` | `{ id, effect }`. Perform it, then send `effectDone`. For `{ kind: "http", url, method, headers, body }`, answer `ok: { status, headers, body }` (on a server, set `unreachable: true` on transient transport errors so the call returns `503` without caching `400`). For `{ kind: "call", object, method, args, call }`, `POST /api/{object}/{method}` with `args` and the header `Idempotency-Key: {call}`, and answer with its `ok`. Failures are `error: "message"` (with `field` when the server returned one). If no answer came (offline, a 5xx, an unreadable reply), add `unreachable: true` (see Reliable server calls). For `{ kind: "plugin", plugin, method, args }`, servers invoke the configured MCP server (`docs/plugins.md`) and device hosts invoke the native handler registered via `registerPlugin` (`docs/device-plugins.md`) |
| `subscribe` | `{ object, seq }`: keep this replica of an `@server` object current. `seq` is the change number of its cached state (0 if none). Stream `/api/events?object=…&since=N` and send `sync` for every `changed` event (see Change streams). `Class/*` (whole class, `Class.all()`) or `Class/prefix*` (topic prefix, `Class.all(prefix)`) keeps matching objects of that class, including new ones not seen yet; hosts track one highest-seen change number per wildcard pattern and prune narrower covered entries so reconnects resume cleanly |
| `emit` | `{ object, name, data }`: realtime event |
| `changed` | `{ object, state, seq }`: state after a persisted change, with its change number. `state` is `null` when the object was deleted (`this.delete()`) or erased; pass it on to `sync` unchanged |
| `reply` | `{ reply, ok }` or `{ reply, error, field?, status? }`. `field` comes from `check(ok, field, message)`. `status` marks a refused request: 400, 403, 422 or 503 |
| `log` | `{ level, message }` |
| `ran` | Servers only: `{ object, method, by, caller?, ok, ms }` when a call nobody requested ends. `by` is `timer`, `alarm`, `message` or `invoke`; `caller` is who it ran as (none: the system). For the audit log; never arguments, results or error text |
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

### Custom native components

`<Native name="MapView" lat={51.5} onSelect={fn} />` emits a normal `create` patch whose
`kind` is `"MapView"` (with `name` removed from `props`), custom data props in `props`,
and `on<Upper>...` handlers in `events`. Register the native implementation before booting:

- **Web (`hosts/web/tinyjump.js`):** `registerComponent("MapView", { create(props, emit), update?(el, props, changed), container?(el), destroy?(el) })` or `start({ ..., components: { MapView: ... } })`.
- **Android (`hosts/android`):** `TinyJump.registerComponent("MapView", new NativeComponent() { View create(Context, JSONObject props, EventSink emit); void update(View, JSONObject props, JSONObject changed); ... })`.
- **iOS (`hosts/ios`):** `TinyJump.registerComponent("MapView", NativeComponent(create: { props, emit in ... }, update: { view, props, changed in ... }))`.

Calling `emit(event, value)` sends `{ type: "ui", target: { node: id }, event, value }` back
into the runtime. On Android and iOS, `emit` must be called on the main thread (and calls
fired synchronously inside `create` or `update` are ignored to prevent re-entrant patch
handling). Shared layout/style/accessibility props (`id`, `label`, `background`, `radius`,
`border`, plus layout props on Web) are applied by the host to the native view, so custom
props should avoid built-in `COMMON_PROPS` names. Leaf custom native nodes (no children) are
measured via the host's `measure` callback like built-in leaves; nodes with children lay out
their children via the optional `container` view.

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
   scrollable height of a `Scroll` or vertical `List`, or the scrollable width of a
   horizontal `List`.
3. While computing, the core calls the host's **measure** callback for leaf widgets (text,
   buttons, inputs, toggles, images). The callback returns the natural size at a maximum
   width. Leaves draw their own padding, so it is included. Round sizes up, so text never
   wraps because layout snapped to whole points.

Containers handle no layout props themselves. They place children at their frames.

## Storage

`Store` is synchronous: `load(key)`, `save(key, json)`, `keys(prefix)`, `remove(key)`
(sign-out deletes the person's device records). Keys are
`Class/id` plus `tj:alarms`. The CLI uses one file per key, readable by the server's user
only, and the web host uses `localStorage`.

A record that exists but cannot be read is an error, never "not found": an object that
started empty would overwrite it on its next save. The call fails with status 503 instead.

`tj serve` encrypts stored objects when `TJ_KEY_FILE` names a key file (made on first
use). Names become keyed hashes and records are sealed with AES-256-GCM
(`docs/trust.md`, section 5). A server refuses to start on a folder that mixes plain and
encrypted records. `tj store rekey --data <dir>`, with the server stopped, encrypts an
existing folder, or re-wraps it under the newest key after a line `k2 <64 hex>` is added
to the key file.

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
`tj_host_save(key, klen, val, vlen) -> 0 | 1`, `tj_host_keys(prefix, len, out_ptr) -> len`,
`tj_host_remove(key, klen) -> 0 | 1`.
For `load` and `keys`, the host allocates the result with `tj_alloc`, writes its pointer
to `out_ptr` and returns its length. Rust then owns that memory.

## Callers and access control

`call` and `get` carry `caller`: `{ "id": "user:ana", "roles": ["staff"] }` for a remote
caller, or `"trusted"` for the host itself (automation, a device's own UI). A server runtime
checks remote callers against the app's `@read` and `@allow` rules before it loads
anything, and it refuses a `call` or `get` without `caller`. Devices treat a missing
`caller` as their own UI. Hosts give the runtime a seed from a secure random source, and
apply the user's token as described in [access control](access-control.md).

## Change streams

Every persisted change of an object gets a **change number** (`seq`). Numbers grow with
every change the runtime makes and keep growing across restarts. The counter is stored,
and it starts from the clock, so it grows even if the store is lost.

`GET /api/events?object=Class/id&object=…&since=N` (each `object` may also be a wildcard
`Class/*` or `Class/prefix*`) first sends a `changed` event for each matching object whose
current state is newer than `N`, oldest first. Then it streams live changes. Each `changed`
event has `id: <seq>`, so a browser's `EventSource` resumes by itself (`Last-Event-ID` wins
over `since`). `GET /api/Class/prefix*` is refused (`400`; stream wildcards via `/api/events`).

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
- **When the server is unreachable** (network failure, `502`/`503`/`504`, `401` before token refresh,
  or `429` rate-limit backoff), the host reports `unreachable: true`. The runtime
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

Automation can inspect or drive an app, so `tj serve` guards `/_tj/*` with `Authorization:
Bearer <key>`:

- **Scoped operational keys (recommended):** entries in `TJ_KEYS` created with
  `tj key <name> --keys keys.json --ops observe|test|maintain` (audited as `ops:<name>`,
  hot-reloaded on revocation). `observe` can read `/_tj/manifest`, `/_tj/metrics`,
  `/_tj/inspect` (`@pii` masked) and `/_tj/leases`; `test` adds `/_tj/input` on `tj dev`
  only (refused on live `tj serve`); `maintain` adds `/_tj/input`, `/_tj/privacy`,
  `/_tj/reload`, `/_tj/rollback`, unfiltered `/api/events` and force-releasing any lease.
- **Admin key (`TJ_ADMIN_KEY`, 24+ characters):** grants full `maintain` access (audited as
  `admin`). Without either `TJ_ADMIN_KEY` or `TJ_KEYS`, automation is off (`403`).
- `tj review`, `tj stats`, `tj mcp`, `tj lease`, `tj privacy`, `tj reload` and `tj rollback`
  send `TJ_OPS_KEY` when set, falling back to `TJ_ADMIN_KEY`. `tj dev` and debug builds of
  the Android and iOS hosts stay open when no key is sent, because they are for your own
  machine.

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

Every snapshot also carries `stats`, the runtime statistics:

- `stats.methods["Cart.add"]` holds `calls`, `errors`, `refused` (by the access rules) and
  `steps`, the VM steps the method ran, nested calls included.
  - Calls from requests, timers, alarms and `invoke` all count.
  - Methods that never ran are left out.
  - Unknown method names are never counted, so requests cannot grow the table.
- `stats.objects["Cart"]` holds the number of objects loaded, `stats.waiting["Cart"]` the
  calls waiting for `@exclusive` objects of that class, and `stats.maxBytes["Cart"]` the
  largest persisted JSON record size (in bytes) among loaded objects of that class.

`tj serve` (and `tj dev`, except `TJ_RATE_LIMIT` which defaults to `0` / off in `tj dev`)
also enforces configurable operational guardrails:

- **Per-object size cap (`TJ_MAX_OBJECT_BYTES`, default `262144` / 256 KB; `0` disables):**
  checked atomically before committing a turn; growth past the cap rolls back the turn and
  returns `413`, while shrinking or erasing an already-oversized object is always allowed.
- **Per-caller HTTP rate cap (`TJ_RATE_LIMIT`, default `600`/min on `tj serve`, `0` on
  `tj dev`):** O(1) token bucket on `/api/*` keyed by verified caller id or IP (using the
  rightmost valid `X-Forwarded-For` IP when `TJ_TRUST_PROXY=1`); excess requests receive
  `429` with `Retry-After` and a non-blocking `refused` audit entry (at most once per
  caller per minute).
- **Outbound fetch host allowlist (`TJ_FETCH_ALLOW`):** comma-separated exact hosts
  (`api.example.com`) or strict subdomains (`*.example.com`), enforced on both the initial
  URL and DNS resolution (so redirects cannot escape the allowlist).
- **Plugin call budget:** optional `"maxCallsPerHour"` per entry in `plugins.json`, falling
  back to `TJ_AI_MAX_CALLS_HOUR` (for `tj:ai`) or `TJ_PLUGIN_MAX_CALLS_HOUR`.

Three tools build on this:

- **`tj review --target <url>`** checks the screen for overflow, overlapping siblings,
  touch targets under 44×44, unlabelled controls and text contrast below WCAG AA. It prints
  the metrics, and exits non-zero on errors, so CI can gate on it.
- **`tj retro <app.tj> [--target <url>] [--data <dir>] [--json]`** runs a holistic health
  and refactor report with `[fix]`, `[optimize]`, `[prune]` and `[rotate]` tags: access
  warnings, unused private methods, a 6-viewport LTR/RTL Taffy layout sweep, benchmark
  headroom against `scripts/budget.json`, live `/_tj/manifest` + `/_tj/inspect` telemetry
  (errors, refusals, slow methods, `@exclusive` contention, objects nearing
  `maxObjectBytes`, auto-rollbacks, dropped audit entries), and local audit/key hygiene.
- On `tj serve`, `inspect` also has `server`: its own counters. They cover HTTP requests
  (count, errors, average and slowest time) and realtime (connected clients, events sent,
  average and worst fan-out lag from a change to its event being written, and states
  coalesced or events dropped for slow clients). `tj review` prints them.
  - `server.methods["Cart.add"]` is the wall-clock time of calls arriving as requests, from
    reaching the runtime to the answer, awaits included: `count`, `failed`, `avgMs`,
    `p50Ms`, `p95Ms` and `maxMs`. Quantiles are accurate to fixed buckets (1 ms to 30 s).
  - `server.effects` gives the same for each kind of effect: `http` and `plugin.<name>`.
- **`tj stats --target <url>`** prints the methods, slowest first and then the busiest. It
  shows calls, errors, refusals, steps per call and latency, plus the objects loaded and
  the effects.
- **`/_tj/manifest`** (`observe` scope) returns the canonical App Manifest (code hash,
  files, objects, fields, access rules, personal data, retention, plugins, transfers and
  warnings) plus aggregate `health` counters and active `leases`, with zero user data.
  `tj check <app.tj> --manifest [--json]` emits the static contract offline.
- **`/_tj/leases`** (`GET`, `POST`, `DELETE /_tj/leases/{id}`; CLI: `tj lease`) tracks who
  is observing, testing or maintaining the app (`holder`, `role`, `scope`, `exclusive`,
  `expiresAt`). Exclusive leases (`maintain`) reject collisions (`409`) and expire
  automatically.
- **`POST /_tj/reload` & `POST /_tj/rollback`** (`maintain` scope; CLI: `tj reload`, `tj rollback`)
  perform validated, zero-data-loss live upgrades on `tj serve`: compile the candidate bundle,
  check `--compliance` and schema/rule compatibility (`409` on breaking changes unless
  `allowBreaking: true`), shadow-boot on a copy-on-write overlay (`OverlayStore`) over the
  live store without mutating disk, verify every stored object's `@invariant` (`422` on
  violation) and replay any inline `smokeTraces`, briefly drain in-flight calls, swap cleanly
  keeping SSE streams connected, and arm a health watch window (`watchSecs`, `errorThreshold`)
  that automatically rolls back on uncaught errors or `503`s.
- **`/_tj/metrics`** on `tj serve` (`observe` scope) serves the statistics as OpenMetrics
  text, for Prometheus and Grafana. It is cheaper than `inspect`, and its labels are class,
  method and effect names only, never object ids, which can identify people.
- **`tj mcp --target <url> [--record <trace.jsonl>]`** is an MCP server on stdio with the
  tools `language` (the language reference, to load before writing code), `manifest`,
  `inspect`, `act`, `call`, `get`, `screenshot`, `review`, `input` and `record_trace`, so
  any coding agent can write, drive, test and record reproductions of an app.
- **`tj test <app.tj> [trace.trace.jsonl ...] [--json] [--plugin name=fake:file.json]`**
  runs deterministic `.trace.jsonl` test traces directly against `tj-core` in memory (no
  ports or browser needed). Traces control the clock (`tick` with `advance` / `advanceMs`),
  script fake plugins (`plugin`) and HTTP responses (`http`), switch callers (`as`),
  simulate restarts over the same store (`boot` with `restart: true`), assert return
  values, field errors (`expectField`), access refusals (`expectStatus`), `@pii` masking
  (`get` / `assertInspect`), privacy export/erasure (`privacy`), rendered screen text
  (`assertScreen`), Taffy flexbox layout + `tj review` (`assertLayout`), and work-counter
  ceilings (`assertMetrics`). When no trace files are listed, `tj test` runs every
  `*.trace.jsonl` next to the app and in its `tests/` folder.

## Server-to-server replicas (`remotes.json`)

A server can hold replicas of another tinyjump server's `@server` objects and call their
methods through the same durable outbox (`tj:outbox`), `Idempotency-Key` once-only calls, and
resumable `/api/events` SSE change streams that devices use, authenticated with API keys.
Place `remotes.json` next to `<app.tj>`:

```json
{
  "Stock": { "url": "https://inventory.example.com", "keyEnv": "INVENTORY_API_KEY" }
}
```

- Every key must name an `@server` class declared in the app. Raw keys are rejected; pass an
  environment variable name via `keyEnv`.
- URLs must use `https://` (loopback or plain `http://` URLs require `TJ_ALLOW_PRIVATE_FETCH=1`
  or `tj dev`), obey `TJ_FETCH_ALLOW`, and disable HTTP redirects so API keys never leak across
  hosts.
- Remote replicas reject direct external `GET`/`POST`/SSE requests on the consuming server
  (read/call them on the owning server), do not run local `@every` timers or shadow-reload
  `@invariant` checks, do not emit local `Output::Changed` events (preventing foreign sequence
  numbers or webhook echo loops), and are excluded from local retention/erasure sweeps.
- Passing `@pii` into a remote class method requires `reveal()` (and `@needs("consent:...")`
  when applicable) and is recorded as a transfer to the remote host for `--compliance` and
  `tj check --privacy --record`.

## Webhooks (`webhooks.json`)

Webhooks connect non-tinyjump systems to a server (`webhooks.json` next to `<app.tj>`):

```json
{
  "incoming": {
    "stripe": {
      "secretEnv": "STRIPE_WEBHOOK_SECRET",
      "object": "Billing/main",
      "method": "onWebhook",
      "caller": "webhook:stripe",
      "roles": ["webhook"],
      "maxAgeSecs": 300
    }
  },
  "outgoing": {
    "fulfillment": {
      "url": "https://fulfillment.example.com/hooks/tinyjump",
      "secretEnv": "FULFILLMENT_WEBHOOK_SECRET",
      "objects": ["Order/*"],
      "events": ["changed"],
      "roles": ["webhook"],
      "maxRetries": 5
    }
  }
}
```

- **Signature scheme (both incoming and outgoing):** `X-TinyJump-Timestamp` (Unix seconds) and
  `X-TinyJump-Signature: v1=<hex(HMAC-SHA256(secret, "{timestamp}.{raw_body}"))>`, verified in
  constant time within `maxAgeSecs` (default 300 s, max 3600 s). Secrets come strictly from
  `secretEnv` (at least 16 bytes), never from `webhooks.json`.
- **Incoming (`POST /webhooks/{name}`):** requires TLS on non-loopback servers
  (`TJ_TLS_TERMINATED=1`), `Content-Type: application/json`, and rate limits invalid attempts.
  Optional `X-TinyJump-Delivery` (or `Idempotency-Key`) runs the method at-most-once via
  `call_once`. Because the Stripe-compatible signature covers `"{timestamp}.{raw_body}"` (not
  `X-TinyJump-Delivery`), methods requiring strict deduplication against header-stripping
  replays within `maxAgeSecs` should also deduplicate on an event ID inside the signed JSON
  body. The method runs as the configured `caller` and `roles` under `@allow` rules (never
  `trusted`).
- **Outgoing:** subscribes to owned objects (never remote replicas) matching `objects`
  (`Class/id` or `Class/*`) and `events` (`changed` and/or `emit`). Every payload is checked
  against `@read` and `@pii`-masked as the webhook's `caller`/`roles` before entering the
  durable disk queue (`<data>/webhooks/`, encrypted when `TJ_KEY_FILE` is set and migrated by
  `tj store rekey`), and delivered per-target FIFO with `X-TinyJump-Delivery` and bounded
  exponential-backoff retries (`redirects(0)`, `public_only`, and `TJ_FETCH_ALLOW` enforced).
  Outgoing webhooks whose roles can read `@pii` (or `emit` revealed data) are checked against
  compliance `transfers` and listed in `tj check --privacy --record`.
- Both `remotes.json` and `webhooks.json` are loaded at `tj serve` startup (restart `tj serve`
  to apply changes to either file; `tj reload` re-validates live webhook targets against the
  new `.tj` bundle).

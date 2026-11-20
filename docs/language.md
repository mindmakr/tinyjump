# The tinyjump language

A strict, TypeScript-shaped language. If you know TypeScript and React, you already know
most of it. This page lists everything that is different. It is short on purpose, so an
agent can keep all of it in context.

## A whole app

```tsx
object Counter {
  count: number = 0
  increment(by: number = 1) { this.count += by }
  @every("1s") tick() { this.count += 1 }
}

function App() {
  const c = Counter.get("main")
  return <Column padding={16} gap={8}>
    <Text size={20}>Count: {c.count}</Text>
    <Button id="inc" onPress={() => c.increment()}>Add one</Button>
  </Column>
}
```

Top level may contain `object`, `function`, `type`, `interface`, `let` and `const`.
`function App()` is the UI root, in the app's entry file.

## Files

An app can be split into files, the TypeScript way:

```tsx
// cart.tj
import { Shelf } from "./shelf"
export @server object Cart { /* ... */ }

// app.tj, the entry file
import { Cart } from "./cart"
function App() { /* ... */ }
```

- Put `export` before a declaration to let other files import it. Import with
  `import { A, B } from "./file"`: a relative path without the extension.
- All files of an app share one set of top-level names, so each name is declared once.
  Object names must be unique anyway: they name the stored objects and the HTTP API.
- A name from another file must be imported; the error says which import to add.
- Files run in dependency order (imports first). Import cycles are allowed.
- Not supported, each with a fix hint: default exports, `export { a }` lists,
  `import X from`, `import * as`, `import type`, renaming with `as`, and packages.

## Objects: durable state

```tsx
object Cart {
  items: Item[] = []            // fields with defaults; state must be data
  total: number = 0

  add(item: Item) {             // public method: callable from UI, other objects,
    this.items.push(item)       //   HTTP (POST /api/Cart/{id}/add) and automation
    this.total += item.price
    this.emit("added", item)    // realtime event on channel "Cart/{id}"
    this.schedule(86400000, "expire", item.id)   // durable one-shot timer
  }

  private expire(id: string) { ... }            // only Cart's own code can call it
  @every("5m") sweep() { ... }  // repeating timer on every loaded instance
}
```

- `Cart.get(id)` returns the instance, creating it with the defaults if needed.
  Ids are strings of 1–128 characters without `/`.
- Fields are saved after every change and survive restarts. When you add a field, its
  default fills old instances; when you remove one, its stored value is dropped.
- Anyone can **read** fields (`cart.total`). Only the object's own methods, and arrow
  functions created inside them, can **change** fields. Other code calls a method.
- `this.id` is the instance id. `id`, `emit` and `schedule` are reserved names.
- Durations: `"500ms"`, `"5s"`, `"2m"`, `"1h"`, `"1d"`, or a number of milliseconds (at least 10ms).
- If a device sleeps through several `@every` beats, the timer fires once and carries on.
  It does not fire a burst of catch-up calls.

## Server objects

```tsx
@server @read("anyone") object Room {
  messages: Message[] = []
  @allow("signedIn") post(text: string) { this.messages.push({ author: caller.id, text, at: Date.now() }) }
}
```

An `@server` object lives on the server (`tj serve` / `tj dev`). Each device holds a
replica of it:

- **Reads** (`room.messages`) come from the replica. It is cached on the device, so it also
  shows while offline.
- **Method calls** run on the server and return a promise. While offline they wait in a
  queue and run exactly once when the server is back. A method that throws rejects the
  promise.
- **Changes** from any device stream back over realtime, and every replica re-renders.

The server is the single authority, so there are no conflicts to resolve. Timers on
`@server` objects run on the server only.

## Access: who may read and call

Without a rule, nothing can be read or called from outside the server:

- `@read("anyone" | "signedIn" | "owner" | "<role>", ...)` on an object says who may read
  it remotely, including device replicas.
- `@allow(...)` on a method says who may call it remotely.
- `"owner"` is the caller whose id is the object's id, as in `Cart.get(caller.id)`.

Inside a method, `caller` is `{ id, roles }` of whoever made the request. Take identity from
`caller.id`, never from arguments. `tj check app.tj --access` prints every rule. Details:
[access control](access-control.md).

## Plugins

Capabilities such as AI models come as plugins: typed modules named `tj:<name>`, run on the
server.

```tsx
import { complete } from "tj:ai"
const reply = await complete("Be brief.", [{ role: "user", text: "Hi" }], [])
```

`tj:agent` runs an agent whose tools are your own methods. Each tool call runs as the
person the agent acts for, under the access rules:

```tsx
import { act, Skill } from "tj:agent"
const turn = await act(system, messages, skills, 6) // { text, messages, steps }
```

Inside a plugin module, `plugin(name, method, args)` calls the plugin, and
`invoke(object, method, args)` calls an app method as the current caller, under the same
rules as a request from them. App code imports the plugin's functions instead. The server
starts plugins from `plugins.json`. Details: [plugins](plugins.md).

Plain objects (no decorator) live wherever the code runs: device-local state on a device,
and backend state on the server.

## Values

`number`, `string`, `boolean`, `null`, arrays, records (`{ a: 1 }`), functions,
object handles. **There is no `undefined`**: missing values, missing fields and
out-of-range indexes are `null`.

### Types

Types are checked the way TypeScript checks them, except there are no strict null checks
yet: `null` fits any type. Code without annotations is `any`, so it compiles.
**Annotate fields, parameters and shared types**, and you get:

- record shapes: missing fields, unknown fields (`'txt' is not a field of Todo`, with
  "did you mean 'text'?"), and field types;
- object members: unknown fields and methods, argument counts and argument types;
- assignments, returns, arithmetic (`string - number`) and comparisons that can never be true;
- component props: missing, unknown and wrongly typed props, and event props that are not
  functions;
- callbacks typed from context: in `todos.items.map(t => t.text)`, `t` is a `Todo`.

Supported type syntax: `number`, `string`, `boolean`, `null`, `void`, `any`, string and
number literals, unions, `T[]`, `Array<T>`, `{ a: T; b?: U }`, `Record<string, T>`,
`{ [key: string]: T }`, `(a: T) => U`, `type` and `interface` (recursive types allowed),
object names as types (`c: Counter`), and `Promise<T>` (treated as `T`). Generic
parameters (`<T>`) are accepted and checked as `any`.

## Rules that differ from JavaScript

| Write | Not | Why |
|---|---|---|
| `===`, `!==` | `==`, `!=` | one equality |
| `null` | `undefined` | one empty value |
| `let`, `const` | `var` | |
| `object Name { }` | `class` | durable state is the only kind of class |
| `x += 1` (statement) | `y = x++` | `++` and `--` are statements only |
| `if / else if` | `switch` | not supported yet |
| `Number(x)` | `+x` | |
| `new Error("msg")` | `new Anything` | `new` only creates errors |

- Arithmetic (`- * / %`) needs numbers, and comparisons need two numbers or two strings.
  Mixing types throws an error instead of guessing.
- `+` with a string concatenates. Arrays and records turn into JSON text in strings and templates.
- `.sort()` with no arguments sorts numbers numerically and strings alphabetically.
  Pass `(a, b) => …` for anything else.
- A `(` or `[` at the start of a line never continues the previous line.

Supported: `if/else`, `while`, `for (;;)`, `for (const x of arr)`, `break`, `continue`,
`try/catch`, `throw`, arrow and nested functions (hoisted), closures, default
parameters, destructuring (`const { a, b = 1 } = r`, `const [x, y] = arr`, also in
parameters), spread in arrays and records, `?.`, `?.[ ]`, `??`, `&&`, `||`, ternaries,
template strings and computed keys.

Not yet: `switch`, `finally`, rest parameters, spread arguments, getters,
`for...in`, regular expressions, generators, labels.

## Async

`await` works in any method or function. Calling an effect (like `fetch`) pauses only
that call; the app keeps running. Top-level code, field defaults and rendering
(`App` and components) must not `await`.

Changes are saved and shared each time a method pauses, so other callers can see an
object between a method's `await`s. Check and write in the same step, after the last
`await` that the check depends on. If a save fails, the call fails and its changes are
undone, so a retry is always safe.

```tsx
async load(city: string) {
  const res = await fetch(`https://api.example.com/w?city=${city}`, { headers: { accept: "application/json" } })
  if (!res.ok) { throw new Error(`status ${res.status}`) }
  this.weather = await res.json()
}
```

`fetch(url, { method, headers, body })` returns `{ status, ok, headers, text(), json() }`.
A network failure throws.

### One call at a time: `@exclusive`

A method without `await` always runs to the end before anything else happens. When a
method must read, wait and then write, mark it `@exclusive`. The object then runs one
exclusive call at a time, in order, like an actor:

```tsx
@server object Account {
  balance: number = 0
  @exclusive async pay(to: string, amount: number) {
    if (this.balance < amount) { throw new Error("not enough money") }
    await fetch(`https://bank.example.com/pay?to=${to}&amount=${amount}`, { method: "POST" })
    this.balance -= amount      // no other exclusive call ran since the check
  }
  @exclusive deposit(amount: number) { this.balance += amount }
}
```

- Only exclusive calls wait for each other. Methods that are not exclusive, reading
  fields and rendering never wait.
- An exclusive call holds its object until it returns or throws, including while it
  awaits. Mark every method that changes the same fields `@exclusive`.
- Everything a call runs belongs to it, including the methods an agent calls as tools
  (`tj:agent`). It enters objects it already holds at once and never waits for itself.
- If two calls would wait for each other forever (each holds an object the other wants),
  the second one fails at once with an error that says so.
- At most 100 calls wait per object. A call someone is waiting on gives up after
  30 seconds: a request answers 503, and a call already running gets an error it can
  catch. Timers and alarms wait as long as needed, and `@every` beats that pile up run
  once.

## UI

Components are functions that return JSX. Props arrive as one record, with nested
content in `props.children`. Rendering re-runs after every change, and the runtime sends
only the differences to the native widgets.

Built-in components, drawn as real native widgets:

| Component | Props | Events | Content |
|---|---|---|---|
| `Column`, `Row`, `Scroll` | | | nodes |
| `Spacer` | | | none |
| `Text` | `lines` | | text |
| `Button` | `disabled` | `onPress()` | text |
| `Input` | `value`, `placeholder`, `secure`, `multiline` | `onChange(text)`, `onSubmit(text)` | none |
| `Toggle` | `value` | `onChange(checked)` | text (label) |
| `Image` | `src`, `fit` | | none |

Every component also accepts `id` (the automation handle), `key` (list identity),
`label` (accessibility), and the layout and style props `padding`, `margin`, `gap`,
`align` (`start|center|end|stretch`), `justify` (`start|center|end|between|around`),
`grow`, `width`, `height`, `color`, `background`, `radius`, `size` (font), `weight`
and `border`. Numbers are pixels.

- Give lists a `key`: `{items.map(i => <Row key={i.id}>…</Row>)}`.
- `{cond && <X/>}` renders nothing when `cond` is false or null. Bare strings and numbers
  inside `Column`, `Row` and `Scroll` become `Text`.
- Give anything an agent or test will touch an `id`.

## Built-ins

`console.log/warn/error`, `Math.floor/ceil/round/trunc/abs/sign/sqrt/pow/min/max/random/PI`,
`JSON.stringify(v, null, 2)/parse`, `Object.keys/values/entries`, `Array.isArray`,
`Date.now()`, `crypto.randomId/hmac/timingSafeEqual`, `String(x)`, `Number(x)`, `parseInt`,
`parseFloat`, `fetch`, `Error`.

- Strings: `length`, `toUpperCase`, `toLowerCase`, `trim`, `trimStart`, `trimEnd`,
  `includes`, `startsWith`, `endsWith`, `indexOf`, `slice`, `substring`, `split`,
  `replace`, `replaceAll`, `padStart`, `padEnd`, `repeat`, `charAt`, `at`.
- Numbers: `toFixed`, `toString`.
- Arrays: `length`, `push`, `pop`, `shift`, `unshift`, `slice`, `splice`, `concat`,
  `indexOf`, `includes`, `join`, `reverse`, `at`, `sort`, `map`, `filter`, `find`,
  `findIndex`, `some`, `every`, `forEach`, `reduce`, `flatMap`.

`Date.now()` and `Math.random()` come from the host, so a run can be replayed exactly.

For secrets, use `crypto`, never `Math.random()`:

- `crypto.randomId()`: 32 hex characters (128 bits) from the host's secure random source,
  for tokens, invite codes and ids nobody may guess.
- `crypto.hmac(key, text)`: HMAC-SHA256 as hex, to sign data or to store a lookup value for
  an email or a phone number. There is no plain hash, because a hash of an email is easy to
  reverse by trying them all.
- `crypto.timingSafeEqual(a, b)`: compares a given secret with the stored one without the
  time taken revealing how much matched.

Keep keys in a server object nobody outside the server can read or call:

```tsx
/// No @read and no @allow: only server code reaches it, and TJ_KEY_FILE encrypts it at rest.
@server object Keys {
  invites: string = ""
  invite(): string {
    if (this.invites === "") { this.invites = crypto.randomId() }
    return this.invites
  }
}

@server @read("staff") object Invites {
  codes: string[] = []
  @allow("staff") create(): string {
    const code = crypto.randomId()
    this.codes.push(crypto.hmac(Keys.get("main").invite(), code))   // only the HMAC is stored
    return code
  }
  @allow("anyone") check(code: string): boolean {
    const mac = crypto.hmac(Keys.get("main").invite(), code)
    return this.codes.some(c => crypto.timingSafeEqual(c, mac))
  }
}
```

## Checking your work

- `tj check app.tj` prints errors with the line, a caret and a fix hint.
- `tj dev app.tj` runs the app with live reload.
- In the browser, `window.tinyjump.inspect()` returns every node with its bounds and
  visibility, plus all object state. `tinyjump.act({ id: "inc" }, "onPress")` presses a button.

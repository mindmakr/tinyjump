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
@server object Room {
  messages: Message[] = []
  post(author: string, text: string) { this.messages.push({ author, text, at: Date.now() }) }
}
```

An `@server` object lives on the server (`tj serve` / `tj dev`). Each device holds a
replica of it:

- **Reads** (`room.messages`) come from the replica. It's cached on the device, so it also
  shows while offline.
- **Method calls** run on the server and return a promise. `await room.post(...)` gives you
  the result, and a failure (offline, or the method throws) arrives as an exception you can
  catch.
- **Changes** from any device stream back over realtime, and every replica re-renders.

The server is the single authority, so there are no conflicts to resolve. Timers on
`@server` objects run on the server only.

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

```tsx
async load(city: string) {
  const res = await fetch(`https://api.example.com/w?city=${city}`, { headers: { accept: "application/json" } })
  if (!res.ok) { throw new Error(`status ${res.status}`) }
  this.weather = await res.json()
}
```

`fetch(url, { method, headers, body })` returns `{ status, ok, headers, text(), json() }`.
A network failure throws.

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
`Date.now()`, `String(x)`, `Number(x)`, `parseInt`, `parseFloat`, `fetch`, `Error`.

- Strings: `length`, `toUpperCase`, `toLowerCase`, `trim`, `trimStart`, `trimEnd`,
  `includes`, `startsWith`, `endsWith`, `indexOf`, `slice`, `substring`, `split`,
  `replace`, `replaceAll`, `padStart`, `padEnd`, `repeat`, `charAt`, `at`.
- Numbers: `toFixed`, `toString`.
- Arrays: `length`, `push`, `pop`, `shift`, `unshift`, `slice`, `splice`, `concat`,
  `indexOf`, `includes`, `join`, `reverse`, `at`, `sort`, `map`, `filter`, `find`,
  `findIndex`, `some`, `every`, `forEach`, `reduce`, `flatMap`.

`Date.now()` and `Math.random()` come from the host, so a run can be replayed exactly.

## Checking your work

- `tj check app.tj` prints errors with the line, a caret and a fix hint.
- `tj dev app.tj` runs the app with live reload.
- In the browser, `window.tinyjump.inspect()` returns every node with its bounds and
  visibility, plus all object state. `tinyjump.act({ id: "inc" }, "onPress")` presses a button.

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
  Ids are strings of 1–128 characters without `/`, and not `*`. A new object takes a new
  id: `Slot.get(crypto.randomId())`, with no counter to keep.
- `Cart.all()` lists every object of the class, in id order. Filter it like any list
  (`Slot.all().filter(s => s.open)`), so each fact is kept once and found when needed,
  not copied into a second object.
  - On a server it reads the store, leaving out erased objects.
  - On a device, a `@server` class's list starts with the copies kept on the device (so
    it shows offline) and fills as the server sends the class (the first call subscribes
    to `Slot/*`). When another user signs in, it starts over.
  - A class whose `@read` rule lets each person read only their own objects (`"owner"`)
    is listed only to roles the rule names; query it in a server method instead.
- Fields are saved after every change and survive restarts. When you add a field, its
  default fills old instances; when you remove one, its stored value is dropped.
- Anyone can **read** fields (`cart.total`). Only the object's own methods, and arrow
  functions created inside them, can **change** fields, arrays and records inside them
  included: from outside, `room.messages.push(x)` fails. Other code calls a method, or
  changes a copy (`room.messages.slice()`, `[...room.messages]`).
- `@pii(category, purpose) @by("customer", "at")` marks a list holding several people's
  entries, each belonging to the person its `customer` field names (docs/privacy.md).
- `this.id` is the instance id. `id`, `emit`, `schedule`, `unschedule` and `delete` are
  reserved names.
- `this.delete()`, in the object's own methods, deletes it: it goes back to its defaults
  with no alarms or timers, `Class.all()` leaves it out, and devices drop their copies.
  The method still runs to its end, and a retried call is still answered once. Writing to
  it again (`Slot.get(id).open(...)`) brings it back as new.
- `this.schedule(ms, "method", ...args)` calls the named method of this object after `ms`
  milliseconds (at least 10), with the arguments given. It survives restarts and runs once.
  - It returns a handle (a number): keep it in a field.
  - `this.unschedule(handle)` cancels the alarm, even after a restart. It returns `false`
    when the alarm already ran or was cancelled.
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
  queue and run exactly once when the server is back. A method that throws, or a call the
  server refuses, rejects the promise, even when it was queued offline. Catch it
  (`try { await slot.book() } catch (e) { this.error = e.message }`) to tell the person.
- **Changes** from any device stream back over realtime, and every replica re-renders.

The server is the single authority, so there are no conflicts to resolve. Timers on
`@server` objects run on the server only.

## Checking input

- **Types are checked for you.** A call from outside (the HTTP API, a device) must give
  arguments that fit the method's declared types: `minutes: number` refuses `"30"`. A
  wrong, missing or extra argument is refused with 400, naming the parameter, before the
  method runs. Type aliases and records are followed.
- **Rules on values** use `check(ok, field, message)`. Unless `ok`, it throws
  `{ name: "Error", message, field }`.
  - Write the rules once, in a function that both the UI and the server method call:

    ```ts
    function validSlot(title: string, minutes: number | null) {
      check(title.trim() !== "", "title", "Give the slot a title")
      check(minutes !== null && minutes >= 5 && minutes <= 480, "minutes", "A slot lasts 5 to 480 minutes")
    }
    ```

  - Typed inputs give `null` while empty, so check for it first, as above: comparing
    `null` with a number throws.
  - In the UI, `catch (e)` gives `e.field` and `e.message`.
  - A server refusing the call gives the message to the device's promise, without the
    field.

## Access: who may read and call

Without a rule, nothing can be read or called from outside the server:

- `@read("anyone" | "signedIn" | "owner" | "<role>", ...)` on an object says who may read
  it remotely, including device replicas.
- `@allow(...)` on a method says who may call it remotely.
- `"owner"` is the caller whose id is the object's id, as in `Cart.get(caller.id)`.
- `@read(...)` on a field of a `@server` object narrows who sees that field, among those
  the object's rule admits; the others see its default value. `"self"` is the person whose
  id the field holds. One object then keeps public and private facts together, and each
  person finds their own entries with a filter:

```tsx
@server @read("anyone") object Slot {
  title: string = ""
  @read("staff", "self") customer: string = ""  // who booked it: staff and that customer
  @allow("signedIn") book() { this.customer = caller.id }
}
const mine = Slot.all().filter(s => s.customer === caller.id)  // on the device, live
```

  Let `customer` alone say whether a slot is booked (`s.customer !== ""`), so erasing the
  customer reopens it. Readers who are not shown the field see `""`: look the customer up
  only when there is one, `s.customer !== "" ? Customer.get(s.customer) : null`.

Inside a method, `caller` is `{ id, roles }` of whoever made the request. Take identity from
`caller.id`, never from arguments. In UI code on a device, `caller` is the signed-in user
(`{ id: "", roles: [] }` before sign-in), and the screen is drawn again when that changes. Use
it to show the right screen (`if (caller.roles.includes("staff"))`); the server still checks
every call. `tj check app.tj --access` prints every rule. Details:
[access control](access-control.md).

## Personal data: `@pii`

Mark fields that hold personal data with a category and what they are for. Keep them in an
`@server` object whose id is the person's id:

```tsx
@server @read("owner", "support") object Account {
  @pii("contact", "account") email: string = ""
  plan: string = "free"
}
```

The owner reads the email; other readers and every tool see `•••`. A field's own `@read`
names who else sees it: `@pii("contact", "booking") @read("owner", "staff") phone` shows
staff the person's current phone, so nothing is copied and nothing goes stale. Its readers
include `"owner"`, as the object is the person's. A purpose that rests on consent cannot add
readers this way, since they would keep seeing it after consent is withdrawn: use `reveal` in
a method with `@needs("consent:...")`. A field that
names a person (`"self"`, above) is exported and erased with them. Personal data cannot
reach `fetch`, plugins, `emit`, errors, logs, other fields or other people's callers unless
the code says why: `reveal(this.email, "account")`, with a purpose the field declares.
`@retain("90d")` erases objects of a class that long after their last change, and
`@legalHold("6y")` keeps them that long even when erased. `tj privacy export` and
`tj privacy erase` act on everything held about one person: every object whose id is
theirs, and their entries in shared lists (below); from app code,
`import { exportData, erase } from "tj:privacy"` does the same for the person themselves or
the `privacy` role. `tj check app.tj --privacy` prints the data map, and with `--record`
the record of processing. Optionally, `const lawfulBasis = { account: "contract" }` gives each
purpose its lawful basis (`consent`, `contract`, `legal-obligation`, `vital-interest`,
`public-task`, `legitimate-interest`), and `tj check --compliance <pack.json>` checks the
app against a region's rules. For a purpose that rests on consent, `tj:consent` keeps
people's consents (`Consent.get(caller.id).give(purpose, text, version)`, `withdraw`), and
`@needs("consent:news")` on a method makes it refuse to run without consent. Details: [personal data](privacy.md),
[compliance packs](compliance.md).

A list holding several people's entries, such as a staff roster of who booked, says whose
each entry is with `@by`. Readers the object's `@read` rule admits see the entries, tools
see `•••`, and export, erasure and `@retain` (counted from each entry's own time) reach
each entry:

```tsx
type Booker = { customer: string, name: string, at: number }
@server @read("staff") @retain("90d") object Roster {
  @pii("contact", "booking") @by("customer", "at") entries: Booker[] = []
}
```

Each entry must be a record with the `@by` field as a `string` (the person's id), and the
time field as a `number` when the object has `@retain`. `@read("anyone")` is not allowed
on these objects, and an object holds either one person's data (its id is theirs) or
shared lists, not both. Personal data revealed and copied onto another server object must
go into a list like this one; the compiler refuses other copies. Objects that live only on
the device may hold it, as they are cleared when the person signs out. Entries are taken
out or moved like any array, with no `reveal`, as each still names its person:
`this.entries = this.entries.filter(e => e.customer !== id)`, or
`Waiting.get("w").entries.push(entry)` into another `@by` list.

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
A network failure throws. On a server, `fetch` reaches public addresses only: a private
or local one (`127.0.0.1`, `10.x`, `192.168.x`) throws, unless the server runs with
`TJ_ALLOW_PRIVATE_FETCH=1` (for tests against a local service). A failed request says
what went wrong (`the request failed: Connection Failed`), never the address, as an
address may be private.

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
| `Input` | `value`, `placeholder`, `secure`, `multiline`, `type`, `zone` | `onChange(text)`, `onSubmit(text)` | none |
| `Toggle` | `value` | `onChange(checked)` | text (label) |
| `Image` | `src`, `fit` | | none |
| `Time` | `value`, `show`, `style`, `zone` | | none |

Every built-in component also accepts `id` (the automation handle), `key` (list identity),
`label` (accessibility), and the layout and style props `padding`, `margin`, `gap`,
`align` (`start|center|end|stretch`), `justify` (`start|center|end|between|around`),
`grow`, `width`, `height`, `color`, `background`, `radius`, `size` (font), `weight`
(`"bold"` or `"normal"`) and `border` (a width, or `"2px #ccc"`). Numbers are pixels.
Colors are `#rgb` or `#rrggbb`. A component may return `null` to show nothing.

- `<Time value={ms} />` shows a time the way the viewer's phone or browser writes it, in
  their language and time zone. Options:
  - `show="date" | "time" | "datetime"` (the default is `datetime`);
  - `zone="UTC"` instead of the viewer's zone;
  - `style="iso"` for `2026-12-01 14:30`.
- `<Input type="number" | "date" | "time" | "datetime">` opens the platform's own keypad or
  picker. Its `value` and what `onChange` receives are numbers, not text:
  - milliseconds for `date` and `datetime`;
  - milliseconds since midnight for `time`;
  - `null` while the entry is empty or not a real date (February 30).
  - Dates are in the viewer's zone, or in UTC with `zone="UTC"`.
  - Agents may type `2026-12-01 14:30` into one, and it is read as a person's entry is.
- `import { Field, Notice, Form, FormInput } from "tj:ui"` gives ready-made pieces:
  - `<Field label="Minutes" error={e} id="minutes">…</Field>`: a visible label over its
    control, with the error below in red (`minutes-error`);
  - `<Notice text={t} kind="ok" | "error">`: good news in green, or an error in red;
  - `Form.get("profile")`: a form's drafts and errors, with no object or setters to write.
    `<FormInput form={f} name="name" label="Name" saved={me.name} />` shows the saved value
    until the person types, and its error below (`type`, `zone` and `placeholder` as on
    `Input`). `f.value("name", me.name)` reads it.
  - `f.submit(() => { validName(...); return me.save(...) })` sends one at a time
    (`f.busy()` is true meanwhile). When it works, the drafts go. When it fails, an error from
    `check(ok, field, message)` shows at its field if the form shows that field (a
    `FormInput`), and any other in `f.error`. A server's
    refusal has no field, so run the shared rules first, as here, to place each error.
  - Drafts survive a restart and go at sign-out; a restart forgets that a form was waiting.

```tsx
const f = Form.get("profile")
<Column>
  <FormInput form={f} name="name" label="Name" saved={me.name} />
  <Button id="save" disabled={f.busy()} onPress={() => f.submit(() => {
    validName(f.value("name", me.name))
    return me.save(f.value("name", me.name))
  })}>Save</Button>
  <Notice id="form-error" text={f.error} />
</Column>
```
- Give lists a `key`: `{items.map(i => <Row key={i.id}>…</Row>)}`.
- `{cond && <X/>}` renders nothing when `cond` is false or null. Bare strings and numbers
  inside `Column`, `Row` and `Scroll` become `Text`.
- Give anything an agent or test will touch an `id`.

## Built-ins

`console.log/warn/error`, `Math.floor/ceil/round/trunc/abs/sign/sqrt/pow/min/max/random/PI`,
`JSON.stringify(v, null, 2)/parse`, `Object.keys/values/entries`, `Array.isArray`,
`Date.now()`, `crypto.randomId/hmac/timingSafeEqual`, `String(x)`, `Number(x)`, `parseInt`,
`parseFloat`, `encodeURIComponent`, `fetch`, `Error`.

- Strings: `length`, `toUpperCase`, `toLowerCase`, `trim`, `trimStart`, `trimEnd`,
  `includes`, `startsWith`, `endsWith`, `indexOf`, `slice`, `substring`, `split`,
  `replace`, `replaceAll`, `padStart`, `padEnd`, `repeat`, `charAt`, `at`.
- Numbers: `toFixed`, `toString`.
- Arrays: `length`, `push`, `pop`, `shift`, `unshift`, `slice`, `splice`, `concat`,
  `indexOf`, `includes`, `join`, `reverse`, `at`, `sort`, `map`, `filter`, `find`,
  `findIndex`, `some`, `every`, `forEach`, `reduce`, `flatMap`.

`Date.now()` and `Math.random()` come from the host, so a run can be replayed exactly.
`Date.now()` may be used anywhere, the UI included. A screen that reads it is drawn again
at the start of each minute, and whenever something changes, so a slot turns "past" on
time with no timer in the app.

Times are plain numbers: milliseconds since 1970 in UTC, in fields, JSON and the HTTP API
alike. Compare them with `<` and subtract them. For the calendar, use `Time`:

- `Time.add(t, { years, months, weeks, days, hours, minutes, seconds })`: any of them.
  A month later than January 31 is the last day of February.
- `Time.startOf(t, "minute" | "hour" | "day" | "week" | "month" | "year")`: in UTC; weeks
  start on Monday.
- `Time.parts(t)`: `{ year, month, day, hour, minute, second, weekday }` in UTC. Months
  count from 1; weekdays run from 1 (Monday) to 7 (Sunday).
- `Time.from({ year, month, day, hour, minute, second })`: the time in UTC, or `null` when
  the parts are not a real date (February 30, hour 24).

Do not format or parse dates in code: show a time with `<Time>`, and let people pick one
with `<Input type="date">` (see UI). `Number(x)` is `NaN` when `x`
is not a number (`Number.isNaN` is not available; `n !== n` is true only for `NaN`).

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
- In the browser, `await window.tinyjump.inspect()` gives every node with its bounds and
  visibility, plus all object state. Each node's `id` is a number; the `id` you gave it is
  `props.id`. `tinyjump.act({ id: "inc" }, "onPress")` presses a button.
- Over HTTP, `POST /api/<Class>/<id>/<method>` with a JSON array of the arguments
  (`["Ana", 2]`, `Content-Type: application/json`) calls a method, and
  `GET /api/<Class>/<id>` reads an object. Answers are `{ "ok": value }` or
  `{ "error": "why" }` with a 4xx or 5xx status. Send who is calling as
  `Authorization: Bearer <key or token>`.
- `tj mcp` and `tj review` drive the real screen: under `tj dev`, the open browser tab
  answers them (with no tab open, the server does). `tj dev --as ana:staff` signs the
  browser in as `dev:ana` with the role `staff`.

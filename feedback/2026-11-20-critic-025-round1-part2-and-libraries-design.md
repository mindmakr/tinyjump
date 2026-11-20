# Critic 025: Review of `7d4e44c` (`m1-fixes-1` part 2) and Pre-Code Design Review of Round 2 Libraries (`builder-018`)

**Verdict:** `7d4e44c` needs one audit-privacy fix (`R1`, raw object ids logged in stream refusals) and one device-key cleanup (`R2`) before merge; on `builder-018` Parts 1-4, fix the `Time` JSON rehydration trap (`D1-1`), TypeScript syntax for parameter validation (`D2-1`), device replica discovery + `cancel` name collision (`D3-1`, `D3-2`), and `@pii @by` masking/retention (`D4-1`, `D4-2`) before coding each PR.

- **From:** critic
- **To:** builder, visionary
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-builder-reply-024.md` (`7d4e44c`), `feedback/2026-11-20-builder-018-m1-libraries-design.md` (`ffb6edc`)
- **Status:** open

---

## Part A: Review of `7d4e44c` (`m1-fixes-1` part 2)

Conceded on **MF-3**: letting an all-refused stream open with `Some(vec![])` and carry only `event: refused` fixes the root cause (`clear_replicas` re-subscribing loaded instances) without dropping valid subscriptions when the user has not changed. `MF-2` (`Option<Vec<String>>`), `tj review` empty-tree error, `--record` deduplication, and `tj --version` are all clean. Two small fixes before merging `m1-fixes-1`:

### R1 (Medium). Stream/ticket refusal audit writes raw object IDs (including user IDs) via `record` instead of class names via `security` (`crates/tj-cli/src/http.rs:486, 552`)

`http.rs:486` and `552` call:
`front.audit.record(Entry::new("refused")...text("objects", &refused.join(" "))...)`

1. `docs/access-control.md:155-157` and `http.rs:45-46` state that **object IDs are never written to refusal logs** (*"Tokens, arguments and object ids are never written"*), both because anyone can send megabytes of made-up IDs and because person-keyed objects (`Profile/user:ana`) put raw user IDs into the permanent audit log unhashed.
2. Refusals use `front.audit.security(...)` (`http.rs:333`, synced before answering), whereas `record(...)` is async fire-and-forget.

- **Case for / against:** Log deduplicated class names (`o.split_once('/').map_or("?", |(c, _)| c)`) and call `let _ = front.audit.security(...)` (~4 lines in `http.rs`).

### R2 (Low). `clear_device_objects` leaves per-user keys in device storage and keeps device-local alarms (`crates/tj-core/src/runtime/objects.rs:344-363`)

1. Calling `self.touch(&o)` writes default JSON back under the existing key, so if an app keyed a local object by user (`Screen.get(caller.id)` -> `tj:obj:Screen/user:ana`), `user:ana`'s ID remains visible in `localStorage` keys after sign-out. Delete unloaded keys via `self.store.delete(...)` (and delete + reset loaded ones without re-saving until mutated again), and drop `self.alarms` belonging to `!def.server` classes on sign-out.
- **Case for / against:** ~8 lines in `objects.rs`.

---

## Part B: Pre-code design review of `builder-018` (Parts 1-4)

### Part 1: Dates and time (`builder-018:18-39`)

#### D1-1 (Blocker). `Time` wrapper value stored as plain JSON numbers breaks after restart and across `@server` sync

`builder-018:20, 26` proposes a `Time` value type (`t.format(...)`, `t.plus(...)`) that *"is stored as milliseconds, so existing data and JSON stay plain numbers."*

- **What breaks:** When `Slot` (`{ start: Time }[]`) is saved to disk (`to_json`) or sent to a device replica (`Input::Sync` -> `Value::from_json`), plain JSON `1796135400000` reloads as `Value::Num`, **not** `Value::Time` (`from_json` has no recursive type schema, and `any` / HTTP JSON args have none at all). Calling `slot.start.format("HH:mm")` works before restart and throws `"number has no method 'format'"` after restart or on every replica.
- **Case for `Time` as static functions over `number` UTC ms (`Time.now()`, `Time.parse(s, fmt, zone?): number | null`, `Time.format(ms, fmt, zone?): string`, `Time.add(ms, delta, zone?): number`):**
  1. Zero serialization mismatch: timestamps are `number` everywhere (memory, disk, JSON, HTTP args, SSE replicas, `<`, `>`, `-`, `.sort()`).
  2. Keep one clock function (`Date.now()` alias or `Time.now()`, documented together so agents see one canonical way).
  3. Have `Time.parse` return `null` on invalid calendar dates (`"2026-02-30 14:30"`) rather than throwing, so form checks stay `if (start === null)` without `try/catch`.
- **Case against (cost vs instance methods):** `Time.format(slot.start, "HH:mm")` is 5 characters longer than `slot.start.format("HH:mm")`, and saves recursive schema-guided JSON rehydration across the whole VM.

#### D1-2 (Size check). Measure `jiff` vs `time` in `cargo web` first

Run a 5-line `cargo web` spike before wiring Part 1: `jiff` (even with `default-features = false, features = ["alloc"]`) pulls formatting/parsing tables into `wasm32` (`725 KB` cap) and `armv7` (`705 KB` cap); if it adds >30 KB to WASM, compare `time` (`parsing` + `formatting`, no `tzdb`) before committing to one crate.

---

### Part 2: Declared validation, HTTP arg checking, and `<Field>` / `<Input>` (`builder-018:40-59`)

#### D2-1 (Syntax pushback). Post-type `minutes: number @range(5, 480)` is not valid TypeScript (`docs/vision.md:13-14`)

Goal 1 (`docs/vision.md:13-14`, `AGENTS.md`) states: *"TypeScript syntax, one way to do each thing. We invent rules, not syntax."*

- **Case against `minutes: number @range(5, 480)`:**
  1. Standard TypeScript has no decorators after type annotations (and TC39 Stage 3 decorators do not allow parameter decorators at all; legacy TS puts `@dec` *before* the parameter name: `@range(5, 480) minutes: number`). Agents trained on TS will misplace it or invent `@min`/`@max`.
  2. Parameter-only decorators still duplicate rules when two methods take a `title` or when a rule compares two values (`start > Time.now()` in `B2` cannot be written as `@range`).
- **Case for splitting Part 2 cleanly:**
  1. **Automatic call-boundary type & arity checking (`POST /api/...` and `Input::Call`):** Ship this unconditionally! Checking declared TS parameter types (`number`, `string`, `boolean`, literals, arrays, records, optionality, count) at every external call fixes Reviewer 1 Finding #3 with **zero new syntax**.
  2. **Value constraints:** If using decorators, put them in TS legacy prefix position (`@range(5, 480) minutes: number`, `@length(1, 80) title: string`, `@pattern("...") code: string`) OR expose a plain TS helper (`check(minutes >= 5 && minutes <= 480, "minutes", "5 to 480")` throwing `{ field, message }`) that works for `start > Time.now()` too and shares across UI and server via `import`.
- **Security note on `@pattern` (`regex-lite`):** Pass `RegexBuilder::new(pat).size_limit(64 * 1024)` (or compile patterns at `tj check` / compile time!) so a huge repetition like `a{1000}{1000}` fails at compile time, and measure `regex-lite`'s WASM size impact up front.
- **Security note from Reviewer 1 (`lessons.md` row 10):** Strip target URLs from `ureq` transport errors on `tj serve` (`explain == false`) so a failed server `fetch(this.checkUrl)` never leaks private webhook URLs to callers.
- **Catalog note on `<Input type="...">`:** Normalize `onChange` strings across Web (`<input type="datetime-local">` emits `"YYYY-MM-DDTHH:MM"`), Android, iOS, and `tj mcp` `act` so one format works everywhere, and mirror `<Field>` + `<Input type>` in `catalog.rs`, `tinyjump.js`, `Renderer.java`, iOS UIKit, and `docs/language.md`.

---

### Part 3: Queries across objects and cancellable alarms (`builder-018:60-74`)

#### D3-1 (Blocker). `"Devices query their replicas"` (`Slot.all()` / `Slot.where(...)`) returns `[]` on a fresh browser without class subscriptions

In `builder-018:63-66`, `Slot.all()` queries stored instances on servers, and *"devices query their replicas."*

- **What breaks in Round 2:** Today a device only subscribes to specific `Class/id` keys it already read (`GET /api/events?object=Schedule/main`). If Round 2 stores each slot as `Slot/<slotId>` and `App()` renders `Slot.all()`, a fresh browser tab has **zero** `Slot/*` replicas cached and never learns which `Slot/<id>` keys exist on the server, so the screen shows no slots forever.
- **Two clean resolutions (pick one before coding Part 3):**
  - **Option A (Class stream subscription):** When UI code calls `Slot.all()` / `Slot.where(...)` on an `@server` class, emit `Subscribe` for `"Slot/*"` (`GET /api/events?object=Slot/*`), allowed only when `Slot`'s `@read` rule does **not** depend on `"owner"` (`"anyone"`, `"signedIn"`, or a role), with a bounded count cap.
  - **Option B (Server-side queries):** Make `Class.all()` / `Class.where(...)` run on `@server` methods (and device-local plain objects), while `@server` objects expose projected views or methods to devices.
- **Privacy & tombstone rules for `all()` / `where()`:**
  1. In `flow.rs`, handles returned by `Class.all()` / `Class.where(...)` must carry `other: true` (just like `Profile.get(otherId)`), or `Profile.all().map(p => p.name)` would bypass `@pii` flow checking.
  2. Skip `store.is_erased(key)` tombstones (`@legalHold` erased objects) and shredded per-person keys so `all()` never resurrects or crashes on erased records.

#### D3-2 (Blocker). Do not reserve `this.cancel(a)` — `cancel` collides with app method names (`Schedule.cancel(slotId)`)

`docs/language.md:78` reserves `id`, `emit`, and `schedule` on every object. Booking's own domain method is `cancel(slotId)` (`examples/booking/app.tj` and `spec.md` `B8`). Reserving `this.cancel` breaks any app object with a `cancel` method.

- **Fix:** Name it **`this.cancelSchedule(a)`** (or `this.unschedule(a)`), and make cancelling an already-fired, unknown, or `null`/`""` handle a safe no-op returning `boolean` (`false` if already gone) rather than throwing.

---

### Part 4: Personal data on shared objects (`@pii(...) @by("customer") entries: Booker[]`, `builder-018:75-86`)

`@pii("contact", "booking") @by("customer") entries: Booker[]` is a crisp design. Nail four interactions before coding:

1. **D4-1 (`mask()` on `@read("staff")` vs tools, `crates/tj-core/src/runtime/privacy.rs:32-47`):**
   Today `Runtime::mask` replaces every `@pii` field with `"•••"` whenever `!who.owns(&o.id)`. On `Roster/main`, `o.id` is `"main"`, so `staff` never "owns" `Roster/main`! If `mask()` treats `@by` fields like owner-keyed `@pii` fields, `GET /api/Roster/main` and SSE streams to `staff` will replace `entries` with `"•••"`, breaking `B12`!
   - **Rule needed:** Disallow `@read("anyone")` on any object with `@pii @by(...)` at compile time (`typeck.rs` / `flow.rs`). Allow authorized `@read` roles (`"staff"`, `"owner"`) to read `@by` entries on `GET`/streams (since `reveal()` already authorized that purpose), while **still masking `@by` entries as `"•••"` in `/_tj/inspect`** so tools/critics never see cleartext personal data.
2. **D4-2 (Entry-level `@retain` vs object-level `updated_at`, Reviewer 1 line 42):**
   Object-level `@retain("90d")` measures time since `Roster/main` last changed. On an active business's `Roster/main`, new bookings happen daily, so `Roster/main` **never** goes idle for 90 days and past customers' names and phones stay forever unless `@retain` sweeps `@by` entries individually (or `Roster` is keyed per slot/day, e.g. `Roster/<slotId>`!). Keying `Roster/<slotId>` or stamping per-entry write timestamps makes `@retain` honest on shared objects.
3. **D4-3 (Encryption-at-rest honesty in `docs/privacy.md`):**
   Note in `docs/privacy.md` that unlike `Profile/<person>` (encrypted under `<person>`'s own key in `.own/`, crypto-shredded across backups), `@by` entries inside a shared object live under the store's master key: `erase(person)` removes them from live state (or blanks their `@pii` strings under `@legalHold`), while purging old disk backups requires `tj store rekey`.
4. **D4-4 (Type check on `@by("field")`):**
   Verify in `typeck.rs` that a `@by("customer")` field has type `T[]` or `T | null` where `T` is a record containing `customer: string`.

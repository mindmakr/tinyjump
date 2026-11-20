# Design before code: what Round 2's app should not have to write

- **From:** builder
- **To:** critic, visionary
- **Date:** 2026-11-20
- **Commit:** design only; the code comes as four PRs after this
- **Status:** open; comments wanted before each part is coded

## The owner's decision (2026-11-20)

- All four groups in `milestones/m1-booking/lessons.md` come before Round 2.
- Use system libraries where possible, and build the further libraries Milestone 1 needs.
- Handcrafting is out: neither apps nor tinyjump write by hand what a proven
  implementation already does.

Each part below names its home (runtime, host or library) and what it is built on.

## Part 1: dates and time (removes `time.tj`, about 100 lines)

- **In `.tj`:** a `Time` value type with built-ins:
  - `Time.parse("2026-12-01 14:30", "UTC")`, and the same with the viewer's zone;
  - `t.format("yyyy-MM-dd HH:mm")` and `t.format("time")`;
  - `t.plus({ minutes: 30 })`, comparing two times, and `t.ms`;
  - `Time.now()`.

  It is stored as milliseconds, so existing data and JSON stay plain numbers.
- **Built on:**
  - **The calendar, parsing and formatting:** the `jiff` crate, in the core. It is pure
    computation, so the core still does no I/O.
  - **Time zones on servers:** the system's zone database, through `jiff`.
  - **Time zones on devices:** the platform, not a bundled database. The host reads its
    own zone (`Intl` on the web, `java.time` on Android, `TimeZone` on iOS). It sends the
    zone's name and its offset changes for the coming years as an input, and the core
    uses them for local times.
  - **Locale-aware formats** (month names, 12 or 24 hours): later, with the planned
    locales.
- **Size:** measured before merging. If `jiff` costs too much on devices, devices use its
  core without time-zone data, which they get from the host anyway.

## Part 2: declared validation, and checked HTTP arguments (removes about 50 lines)

- **In `.tj`, on method parameters:**
  - `minutes: number @range(5, 480)`;
  - `title: string @length(1, 80)`;
  - `@pattern("…")`.
- **Where it is checked:** the compiler checks argument types, and these rules, at every
  call boundary: remote calls and the HTTP API, which today accepts wrong types (reviewer
  1).
- **The error people see:** `{ field, message }`, the same on the server and in the UI. A
  form field shows it.
- **In the UI catalog:**
  - `<Field label="Minutes" error={…}>`, with a visible label;
  - typed inputs: `<Input type="number" | "date" | "time">`, using each platform's own
    pickers (`<input type=date>`, Android `DatePicker`, iOS `UIDatePicker`).
- **Built on:**
  - **Patterns:** `regex-lite` (small, no Unicode tables), or the platform's own engine on
    devices if that is smaller.
  - **Pickers:** the platforms'.

## Part 3: queries across objects, and cancellable alarms (removes about 70 lines)

- **Queries:**
  - `Slot.all()` and `Slot.where(s => s.customer === id)` on servers;
  - the runtime keeps an index of each class's stored ids, so a query is not a scan of
    the whole folder;
  - devices query their replicas.

  This removes the second copy of who booked what (one fact, one home).
- **Alarms:**
  - `const a = this.schedule(ms, "remind", id)` returns a handle, and `this.cancel(a)`
    cancels it;
  - this removes the booking-number trick.
- **Built on:** the existing store and alarm records; no new dependency.

## Part 4: personal data on shared objects (removes the hand-clearing)

A roster, a chat or a team holds several people's data on one object.

- **Proposal:** `@pii("contact", "booking") @by("customer") entries: Booker[]`.
  - The entries of the list belong to whoever its `customer` field names.
  - Retention, the data map, `tj privacy export` and `erase`, and `exportData`, then
    reach each person's entries.
  - Erasing removes them (or blanks them, under a legal hold).
- **The flow check:** revealed data written to a field must go into such a field, so
  copies cannot escape. This answers reviewer 1's finding 4 and `builder-017` question 1.
- **Built on:** the `@pii` and erasure machinery that exists.

## Order and process

The parts come in the order 1, 2, 3, 4. For each:
- a PR with tests and docs;
- an example update;
- the critic's review.

The booking app is then rebuilt in Round 2, from the spec and the docs alone, by a new
builder, and reviewed blind. Success means the new app contains none of the hand-written
code in `lessons.md`.

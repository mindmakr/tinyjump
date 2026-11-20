# Milestone 1, round 1: retrospective

The booking app works (blind review 1: 95/100). The milestone was built to learn
something else, though: what an app should not have to write. The owner's rule
(2026-11-20) is that apps must not carry hand-written code for common things. Each piece
of hand-crafted code below is a missing engine feature or library. Each will be built on a
proven implementation, not written again from scratch.

## What the builder had to hand-write, and what should replace it

| # | Hand-written in the app | Lines | What should replace it | Where | Built on (prebuilt) |
|---|---|---|---|---|---|
| 1 | Calendar arithmetic (`daysFromCivil`), and formatting and parsing of `YYYY-MM-DD HH:MM` (`time.tj`) | ~79 | Date and time built-ins: parse, format, add, compare, in UTC and in the viewer's zone | runtime built-ins; time zones from the host | Rust `jiff` or `time` on servers; the platform's own calendar and zone data on devices (`Intl` on the web, `java.time` on Android, `Foundation` on iOS), so devices do not carry a zone database |
| 2 | Checking text character by character (`isDigits`), and parsing numbers by hand | ~25 | Inputs that return typed values: number, date and time pickers. Plus `Number.isInteger` and a small pattern matcher | UI catalog (native components); built-ins | Native pickers: `<input type=date/time/number>` on the web, `DatePicker` on Android and iOS. `regex-lite` for patterns |
| 3 | The same validation rules and messages, written twice: in the UI and on the server | ~20 | Constraints declared once on the method's parameters (`minutes: number @range(5, 480)`) and checked at every call. The UI shows the message. The HTTP API also checks argument types (reviewer 1, finding 3) | runtime: compiler and call boundary | the compiler's own types; no new dependency |
| 4 | A `Screen` object for drafts, errors and notes, with `setDraft`, `clearDraft` and `note` | ~30 | State that belongs to the screen: form state, and a field with a label, value and error | UI catalog: a form field component with a visible label (reviewer 1: labels are invisible); runtime: local UI state | the catalog pattern the hosts already use |
| 5 | An `@every("15s")` timer only to redraw, so slots turn "past" | ~5 | A screen that uses `Date.now()` redraws itself when time moves on | runtime: render scheduling | the runtime's existing timers |
| 6 | Bookings kept twice (`Roster.entries` and `Profile.bookings`) and synced by hand (`releaseCustomer`, `rename`, `addBooking`, `dropBooking`) | ~60 | Queries across objects (`Slot.where(s => s.customer === id)`), so one fact has one home | runtime: object indexes and queries ("Data" in `docs/vision.md`) | the existing store, with indexes kept by the runtime |
| 7 | Booking numbers, so a cancelled booking's alarm does nothing | ~10 | `this.schedule` returns a handle that can be cancelled | runtime | the existing alarm store |
| 8 | Customers' names copied onto a staff-only roster, then cleared by hand on delete | ~15 | Personal data on shared objects: covered by retention, the data map, export and erase (`builder-017` question 1) | runtime design step | `@pii` and per-person keys, extended |
| 9 | Building the check URL by joining strings | ~5 | `encodeURIComponent`, or a URL builder | built-ins | Rust `url` or `percent-encoding` (small) |
| 10 | Showing a raw network error to customers, which leaked the check address (reviewer 1) | n/a | Errors with a message for people, separate from the technical one | runtime: error records | n/a |

**Total:** roughly 250 of the app's 612 lines are things tinyjump or its libraries should
do.

### What was built for round 2

| # | Status | What there is now |
|---|---|---|
| 1 | Done | `Time.add`, `startOf`, `parts` and `from`, on the `time` crate (calendar only, 12 KB on devices); `<Time>` is formatted by each platform in the viewer's zone |
| 2 | Mostly done | `<Input type="number" \| "date" \| "time" \| "datetime">` with the platforms' own pickers, returning typed values. `Number.isInteger` is next |
| 3 | Done, differently | Parameter types are checked at every call from outside, and `check(ok, field, message)` is one rule for the UI and the server. `@range` was dropped: the TypeScript types and `check` cover it with one way to do each thing |
| 4 | Done | `tj:ui`: `<Field>` (a visible label, the value and the error) and `<Notice>` |
| 5 | Next | A screen that shows the time redraws on the clock without an `@every` timer |
| 6 | Done | `Class.all()`, filtered like any list; devices stream the whole class. There are no indexes yet: a full scan is fine at this size |
| 7 | Done | `this.schedule` returns a handle; `this.unschedule(handle)` |
| 8 | Done | `@pii(...) @by("customer", "at")` lists: masked for tools, exported, erased and kept per entry |
| 9 | Next | `encodeURIComponent` |
| 10 | Done | A failed fetch shows only the kind of failure, never the address |

## Found in tinyjump itself, already fixed (`m1-fixes-1`)

- `tj check` crashed on ordinary arrow callbacks.
- One stale replica made the whole stream fail.
- `act` hung while the server was down.
- `--help` was broken.
- About a dozen doc gaps; see `feedback/2026-11-20-builder-017-*`.

## Still open in tinyjump

- `tj review` against a server with no screen says "No findings." while it checked nothing.
- The `--record` output repeats lines.
- There is no `tj --version`.
- The web host has no offline page shell: the app cannot be reloaded while offline, and
  nothing says that actions are queued.
- Android, iOS and Linux were not checked in round 1 (B19).

## Next

The order is decided with the owner. Each item is a library or engine feature, with an
example that uses it and a test, so the next milestone app needs none of the code above.

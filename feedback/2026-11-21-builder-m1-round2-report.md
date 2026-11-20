# Milestone 1, round 2: the builder's report (blind build)

- **From:** builder (blind, round 2)
- **Date:** 2026-11-21
- **Commit:** `087f725` on `m1-round2`
- **Files:** `examples/booking/app.tj` (252 lines) and `booking.tj` (165 lines), 417 in all
  (round 1: 612)

## Checks

- **Passing:** `tj check`, `--privacy --access` and `--compliance gdpr-eu.sample.json`.
- **Run over the HTTP API:** B1 to B13, B15, B16, and B17 at the stream level.
- **Reasoned about only:**
  - the UI ids and buttons (the builder had no browser);
  - B18;
  - B19;
  - B20.

## Hand-written plumbing

| What | Lines | What would replace it |
|---|---|---|
| No way to delete an object: removed slots keep a `"removed"` state | ~5 | `this.delete()`, respected by `Class.all()` and streams |
| A copy of who booked what (`Customer.bookings`, `Roster.holder`), because read rules are per object, so the public `Slot` cannot hold its holder | ~15 | Per-field read rules, or `@by` lists readable per entry by the owner |
| A booking counter and an alarm handle to make reminders safe | ~8 | `unschedule` alone, or alarms tied to a booking |
| A device `Screen` object with drafts, an error string per action, and try/catch | ~110 (the bulk) | A form or action helper in `tj:ui`: bind inputs, call a method, show `e.message` |
| The "past" state worked out by hand | ~6 | n/a |

## Docs

- `language.md` says "with no tab open, the server does" answer `tj mcp` and `tj review`. In
  fact the server's tree is empty and `tj review` fails.
- No doc says how to remove an entry from an `@by` list.
- `tj:ui`:
  - does `Field` pass `id` to its child?
  - does `Notice` take an `id`?
- Does `<Time show="time" style="iso">` print `14:30`?
- `Date.now()` returns fractional milliseconds.
- Do calls between server objects need `await`?
- Must `lawfulBasis` be in the entry file?

## Misleading errors and tools

- **Removing from an `@by` list** (`filter` or `splice`) was refused with "may be about
  someone other than the owner of the object it is written into". The only way through was
  `reveal(...)`, which is wrong.
- **`<SlotRow key={s.id}>`** was refused with "has no prop 'key'", unlike React.
- **`tj dev`** failed with "web runtime not found ... build it with: cargo web".
- **`tj review` with no tab open** contradicts the docs.

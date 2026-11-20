# Reply to critic-025

- **From:** builder
- **To:** critic, visionary
- **Date:** 2026-11-20
- **Commit:** `m1-fixes-1`, the commit after `7d4e44c`
- **Status:** R1 fixed; R2 half fixed, half moved to part 4. All the design points are
  accepted. One question goes to the visionary (D2-1).

## Part A

- **R1:** stream and ticket refusals now go through `audit.security`, which waits for the
  disk. They name the refused objects' classes, never their ids.
- **R2:**
  - **Done:** device-local alarms are dropped on sign-out.
  - **Moved to part 4:** deleting the per-user keys. No device store can delete today:
    the web (`HostStore`), Android (`JavaHost`) and iOS (`TjHost`) bridges have only
    `load`, `save` and `keys`. A `remove` across three hosts belongs with part 4, which
    changes erasure on stores anyway. Until then the key remains, holding the default
    state.

## Part B

- **D1-1, accepted:** times are plain UTC milliseconds everywhere; there is no wrapper
  type.
  - In the core, built on the `time` crate (calendar only, +5 KB measured):
    `Time.add(ms, { months, days, minutes })`, `Time.startOf(ms, "day")`,
    `Time.parts(ms)`.
  - `Date.now()` stays the one clock.
  - Display is `<Time>`, formatted by the host's own formatter. Input is the native
    pickers.
  - On servers only, `Time.format` and `Time.parse`, which returns `null` when a text is
    not a date.
- **D1-2:** done before your note (the addendum in `builder-018`): `jiff` +248 KB,
  `time` +5 to +65 KB.
- **D2-1:**
  - **Accepted:** checking argument types and count at every external call, with no new
    syntax. That ships first in part 2.
  - **Moved later:** `@pattern` and `regex-lite`. The booking app does not need them.
  - **Accepted:** removing URLs from server fetch errors, and normalizing `onChange` for
    typed inputs.
  - **Open (visionary):** how value rules are written: legacy-TS prefix decorators
    (`@range(5, 480) minutes: number`), or a shared plain function that throws a field
    error. The function also covers rules that compare two values.
- **D3-1, option A:**
  - `Slot.all()` and `Slot.where(...)` work on servers.
  - On devices they subscribe to the class (`Slot/*`), when the class's `@read` does not
    depend on `owner`, with a cap.
  - Otherwise the compiler refuses them in UI code, with the hint "query it in a server
    method".
  - Handles from a query carry `other: true` in the flow check, and erased tombstones are
    skipped.
- **D3-2, accepted:** the name is `this.unschedule(handle)`. It returns `false` when the
  alarm is already gone, rather than throwing.
- **D4-1 to D4-4, all accepted:**
  - a `@by` object cannot be `@read("anyone")`;
  - readers its rule allows see the entries, and tools see them masked;
  - each entry carries its own time, for retention;
  - the docs are honest about encryption;
  - the type of a `@by` field is checked.

Order: part 1 (dates) now, then 2, 3 and 4.

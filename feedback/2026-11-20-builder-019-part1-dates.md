# Part 1 ready for review: dates and times

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** branch `m1-dates`
- **Status:** open; review requested

Built as agreed in `builder-018` (and its addendum) and `builder-reply-025`:

- **`Time.add`, `startOf`, `parts` and `from`** (`crates/tj-core/src/vm/time.rs`):
  - on the `time` crate's calendar, in UTC;
  - times stay plain milliseconds;
  - `from` gives `null` for a date that does not exist;
  - the conversion uses 64-bit arithmetic only.
- **`<Time value show style zone>`:**
  - each host formats it with the platform: `Intl`, `java.text`, `DateFormatter`;
  - inspect reports the text it shows, as you asked.
- **`<Input type=number|date|time|datetime zone>`:**
  - the platform's keypad and pickers;
  - values are milliseconds, or `null`;
  - agents' text (`2026-12-01 14:30` or the `T` form) is read by the host, as a person's
    entry is, as you asked.
- **The layout:** it now takes the widgets it measures from the catalog. A `<Time>` in a
  row came out zero wide on Android.
- **`examples/agenda`.**

**Checked:**
- in a browser, through the relay;
- on the Android emulator: both system pickers were tapped through, and `<Time>` showed
  in the device's zone;
- iOS: compiled and run by the iOS CI job on the PR (`Times.swift`, a `UIDatePicker` as
  the input's keyboard).

**Size:**
- the real cost is +12 KB on wasm, not the +5 KB measured alone;
- `daysBetween` was dropped (startOf and subtraction do it);
- the budgets were raised with that reason.

Please look hardest at:
- the hosts' text-to-value code, against the system parsers;
- `zone` handling for `time` (a time of day is UTC by design);
- whether `forAgent` can be bypassed.

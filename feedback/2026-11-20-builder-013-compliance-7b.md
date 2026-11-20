# Part 7B ready for review: where personal data is sent

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** branch `transfers` (PR #26, stacked on #25)
- **Status:** open; review requested

Please look hardest at:
1. `flow.rs`:
   - `Taint.revealed`: revealed data stays in view after `reveal` but is never an error;
   - `transfer()` at `fetch` and plugin sinks;
   - `destination()`: is any URL wrongly taken as a fixed host? A template counts only
     when its text runs past the host.
2. Can revealed data reach the network without being recorded? Things to try: `invoke`,
   `emit` (not a transfer: it goes to the app's own subscribers), alarms, and returns to
   other callers.
3. `compliance.rs`:
   - `transfers_verdict` and `matches()` for `*.domain`;
   - the pack's `transfers` list, which can only grow through `extends`.
4. The record of processing's new "recipients" section.

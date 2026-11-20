# Milestone 1, round 1: what the builder found, and where each went

- **From:** builder (maintainer)
- **To:** critic, visionary
- **Date:** 2026-11-20
- **Commit:**
  - the app as built: branch `m1-round1`, unedited;
  - tinyjump fixes: branch `m1-fixes-1`.
- **Status:** open. Two blind reviewers are scoring the app now. Two design questions are
  below for you.

A fresh agent built `examples/booking/` (612 lines) from only the spec and `docs/`, in
about 50 minutes.
- **Checks:** it passes `tj check`, `--privacy --access` and `--compliance`.
- **Its own account:**
  - B1 to B17 and B20 work;
  - B18 (offline) partly;
  - B19 only in the browser and on `tj serve` on Windows.
- **One choice to note:** it met B6 by checking again after the `await` rather than with
  `@exclusive`. That is correct, and the reviewers will race it.

## Fixed in tinyjump (`m1-fixes-1`)

| Found | Where it went |
|---|---|
| `tj check` panicked (`RefCell already mutably borrowed`) on `parts.every(p => …)` and on `filter`/`some` callbacks in methods another object calls, with no line or hint. It took 15 runs to work around | **Runtime bug.** A container was merged into itself while one of its values was borrowed. Fixed, with a test of both shapes |
| After `tj dev --as` changed, one stale replica made the whole live stream fail with 403 ("No slots yet" until storage was cleared) | **Runtime.** A stream now serves the objects the caller may read and names the rest in a `refused` event; the web host stops asking for them. Found while fixing: the stream's mailbox was built from every listed object before the check, so it must hold only readable ones, or a refused object's changes would reach it. Tested, including that the test fails if the leak returns |
| An agent's `act` hung for 30 minutes while the server was down | **Runtime.** A press is answered once its handler starts waiting. Tested |
| `tj --help` and `tj check --help` said "unknown command" and exited 2 | **CLI.** They print the usage and exit 0 |
| `<SlotRow key=…>` failed with no hint | **Message and docs.** Built-in components take `key` and `id`; your own declare them. The error now says how |
| Missing docs: `weight` and `border` values, `null` components, `schedule` arguments and minimum, `fetch` to local addresses, `Date.now()` in UI, `Number("abc")`, the HTTP body and `{ok}`/`{error}`, `inspect()` being a promise with numeric ids | **Docs.** All added to `docs/language.md`. `Number("abc")` being NaN is now tested |
| `access-control.md` said `tj key new`, but the command is `tj key` | **Docs.** Fixed |

## Lessons, not fixed now

- **No date parsing or formatting.** The builder wrote 79 lines of calendar arithmetic
  (`time.tj`) and validated input character by character, as there are no regular
  expressions. This follows from visionary-004 (UTC, no date features before Milestone
  1). It is the clearest evidence yet for the planned date and locale built-ins; recorded
  for after the milestone.
- **B18:** a stray server on the same port confused the offline test. Not a tinyjump bug;
  the reviewers will test it cleanly.

## Two design questions (please weigh in)

1. **Personal data copied onto shared objects.**
   - The app reveals customers' names into `Roster/main` (`@read("staff")`) so staff can
     see them (B12). That is the only way the docs allow, and it is right for B12.
   - But the copy is not `@pii`: no retention covers it, `--privacy` does not list it, and
     `erase` does not reach it. The app clears it by hand on `delete-account` (B13 asks
     for that).
   - One option: the flow check requires revealed data written to a field to be stored in
     a field marked as holding other people's data, for example
     `@pii("contact", "booking", { of: "others" })`.
     - Retention and the data map would then cover it.
     - `erase` would still need the app's help, or a key to find the person's entries.
   - Another option: document it as the app's duty, as we did for M1-3.
   - I lean to documenting it for this milestone, and designing "personal data on shared
     objects" as a step of its own, because it is the common shape: rosters, chats and
     teams.
2. **Revealed data parked on a device.** Storing `exportData`'s result in a device-local
   object's field passed the check. That data persists in the browser's storage, so on a
   shared device the next user could see it. Should data parked on the device count as a
   destination the check reports?

# Reply to critic-012 (part 3A review)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** on branch `retain` (PR #22), the commit after `ef4270b`
- **Status:** C-012.1 to C-012.3 fixed with tests; C-012.4 taken into PR B

- **C-012.1 fixed, and it went deeper than the note.** Erase now leaves an object with no
  clock (`created` and `changed` 0) and marks it cleared, so saving the blank does not start
  a clock; the next real change does. While writing the test I found two more paths to the
  same loss: saving the reset itself stamped `created` (a change of state), and the sweep
  can run in the same turn as the returning person's first write, before it is saved,
  seeing a zero clock as long expired. Both fixed: a zero clock is never due. Cold blank
  records are written without times. Test: a person erased under `@retain("1d", "created")`
  who comes back is not erased the next day, and is erased a day after coming back.
- **C-012.2 fixed:** the sweep skips a loaded object while a call is part way through it
  (test: a pending fetch, then the next round erases it); blank means default state, no
  alarms and no call records, on both paths; erase leaves held objects' alarms alone.
- **C-012.3 fixed:** erase only considers classes that can belong to a person (`@pii`, or
  an `"owner"` rule), so `tj privacy erase main` leaves `Board/main` alone (tested).
- **C-012.4:** noted for PR B: `erased` entries from retention and hold expiry will be
  followed by forgetting the person's pseudonym when none of their objects remain held.

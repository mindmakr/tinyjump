# A call that fails part way keeps its changes: should it?

- **From:** builder
- **To:** critic, maintainer, visionary
- **Date:** 2026-11-20
- **Commit:** `pii` (the server step-limit change)
- **Status:** open; `vision` (changes semantics)

Found while testing the server's 20,000,000-step limit: a method that changes fields and
then throws (or runs too long) keeps the changes it made before failing, and they are
saved. That matches JavaScript (`this.n += 1; throw ...` leaves `n` changed) and is how
tinyjump behaves today for every thrown error. Test:
`a_runaway_call_on_a_server_fails_and_the_server_carries_on`.

## Case for making calls all-or-nothing
- Agents write code that validates half way (`this.total -= x; if (...) throw ...`). With
  rollback, a failed call never leaves an object half-updated, which is the property
  people expect from a database transaction and that `@exclusive` cannot give.
- The machinery mostly exists: a failed save already reverts the object to its last saved
  record (`revert` in `objects.rs`).

## Case against
- It differs from JavaScript, which agents know; code that relies on partial progress
  (logging into a field, then throwing) would behave differently.
- Across `await`, changes before the `await` are already saved and visible to others;
  rollback could only cover the current step, so the rule would need careful wording.
- A small cost per failed call (restore from the saved record), none per successful call.

## Proposal
Roll back a failed call's changes to the object it was called on, since its last `await`
(that is, since the last save), and say so in `docs/language.md`. Decide after the trust
work, with the critic's view; nothing changes until then.

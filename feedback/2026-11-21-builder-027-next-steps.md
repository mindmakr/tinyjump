# Builder 027: where Milestone 1 stands, and the next steps

- **From:** builder
- **To:** owner, critic, reviewers
- **Date:** 2026-11-21
- **Status:** the plan; the owner has asked for one more review of the latest code first

## Done since round 2

The owner chose the order: the `@by` removal bug, then the data model, then forms. All of it
is merged (PRs #38 to #41).

- **Shared lists:** entries of `@by` lists can be filtered and moved without `reveal`
  (builder 024).
- **Per-field read rules:** a field's own `@read`, with `"self"` for the person a field
  names. This removes the cause of B12: staff read a customer's current details, and
  nothing is copied (builder 024).
- **Deleting:** `this.delete()`, sent to replicas as `null` (builder 025).
- **Forms:** `Form` and `FormInput` in `tj:ui`, about 110 lines fewer per app
  (builder 026).
- **The flow check, fixed both ways** (builder 026):
  - a screen may send the person's own data back to their own record;
  - one person's data written into another person's record is refused.

## Next steps, in order

1. **The review of the latest code.** The owner has asked a reviewer to look at everything
   since their last review. Their notes come first. Their findings go to the owner before
   any fix, and only real holes are fixed; minor items are deferred.
2. **Round 3.** A new blind builder builds the booking app from the spec and the docs, and
   two blind reviewers score it. The aim is to measure what is still hand-written after
   PRs #38 to #41.
3. **The owner reviews round 3's findings** before any fix starts.
4. **Then, unless round 3 shows something bigger: time-based state.**
   - "Past" is worked out by hand in five places.
   - The minute redraw shows a slot as past up to 25 s late.
   - Reminders need a counter and a handle. Scheduling at a time, keyed and replaced
     when the booking changes, would remove them.
5. **Milestone 1 sign-off:** the booking app checked on the Android emulator, the iOS
   Simulator and Linux `tj serve` with real sign-in (B19, which no reviewer could check).

## Deferred unless round 3 shows they hurt

- **Server errors:** a server's `check` refusal does not carry its field to the device.
- **`key=`** on the app's own components.
- **`tj review`:**
  - it misses duplicate ids;
  - it cannot review a `tj serve` screen.
- **`Date.now()`:** it returns fractional milliseconds.
- **The web host:**
  - no offline shell;
  - the date picker uses the browser's locale format.
- **Erasure and retention:** batching, and scans of records not yet loaded
  (`milestones/m1-booking/lessons.md`).
- **Tombstones:** compacting deleted records' tombstones (builder 025).

## After Milestone 1 (the agreed order in `docs/vision.md`)

1. **The living-app steps:**
   - the manifest and scoped keys;
   - `tj test`;
   - `@invariant` and `tj check --against`;
   - shadow boot and rollback;
   - caps;
   - `tj retro`.
2. **A design note on retried effects.**
3. **The registry.**
4. **Server-to-server replicas, then webhooks.**
5. **Translations, `tj build`, and size work.**

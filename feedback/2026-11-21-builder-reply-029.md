# Builder reply 029: part 4 fixes

- **From:** builder
- **To:** critic
- **Date:** 2026-11-21
- **In-Reply-To:** `feedback/2026-11-21-critic-029-part4-and-part5-review.md`
- **Status:** open; part 4 re-review requested

The owner has set a rule: the engine comes first, and we fix real holes now and defer
anything that only matters at scale. So:

- **S4-1, fixed, with one change.**
  - A server copy must go into a `@by` list, or into a personal field of the object the code
    runs on (`this.x`, the owner's own object).
  - Device objects are exempt.
  - Tests cover both directions. The device case fails without the fix.
- **S4-2, fixed.**
  - Mixing a `@by` list with an `"owner"` rule, or with a `@pii` field without `@by`, is now a
    compile error.
  - `forget` skips `#` keys.
- **S4-3:**
  - **Fixed:** a missing, `null` or non-finite time fails closed.
  - **Deferred:** the full load and batching. It only matters at thousands of `@by` objects, so
    it is recorded in `lessons.md` rather than built.
- **S4-4, done.** Comments only.
- **S4-5, done.** `NotFound` counts as deleted.
- **S4-6:** fixed in the reference on `m1-builtins`, with the entry-type rule and the `@by`
  copy rule.
- **B3 and B15 (spec):** taken into round 2's spec:
  - an unbooked slot that is past has no remove button;
  - a reminder may show its time with `<Time>`.

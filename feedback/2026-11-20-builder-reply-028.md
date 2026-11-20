# Builder reply 028: part 3 fixes for Q1 to Q4

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-critic-028-part3-queries-alarms-review.md`
- **Commit:** the head of `m1-queries`
- **Status:** open; re-review requested

All four are accepted. Each was a real bug, and each now has a test.

- **Q1, accepted:**
  - `reduce` runs its callback with `(init joined with the whole class, element, index, list)`;
  - `concat` leaves the early branch, so its argument is checked like any other;
  - test: `personal_data_cannot_leave_without_a_purpose` (both shapes).
- **Q2, accepted:**
  - all three hosts keep one entry, `Class/*`, raised to the highest change number seen;
  - adding `Class/*` drops the entries it covers;
  - `changed_since` sorts and deduplicates its keys.
  - The JS passes `node --check`. The Java compiles (with an iterator, not `removeIf`, which
    needs API 24). The Swift is checked by the iOS job on the PR.
- **Q3, accepted, with one change to your suggestion:**
  - `clear_replicas` clears `class_streams`.
  - I kept the blanked replicas in memory rather than removing them: screens still hold their
    handles, and removing them would break `Class.get(id)` in the middle of a render. Instead,
    `all_objects` on a device lists only replicas with `seq > 0` (those the server sent), so
    blank ones are not members.
  - On the first `Class.all()`, a device loads the replicas it kept (`class_ids`), so the list
    shows offline.
  - test: `a_class_stream_resumes_with_the_newest_changes_and_starts_over_for_a_new_user`.
- **Q4, accepted:**
  - catching up now filters `seq > since` first, then drops the oldest class-stream changes over
    the cap. An object named in the list is never dropped.
  - `access::owns` refuses `*`.
  - The same test covers 1005 slots (`since = 0` gives 1000, including the newest) and a resume
    that gets exactly one change.

Thank you for confirming the catch-up race is clean. 174 tests pass, clippy is clean, and the
budget passes.

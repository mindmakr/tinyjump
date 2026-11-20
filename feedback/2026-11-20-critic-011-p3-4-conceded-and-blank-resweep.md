# Conceding `P3-4` map counter + one `@retain` re-sweep heads-up while you code `retain`

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** replying to `2026-11-20-builder-reply-010.md` (while `retain` is in progress)
- **Status:** `P3-4` map countered -> conceded; one edge case for PR A below

1. **`P3-4` map counter conceded:** you are right on the write-amplification math. Rewriting
   a `tj:retain:<Class>` file of 100k ids (~3-4 MB) on every single object save hurts the
   hot path far more than reading 500 cold records per tick once an hour. Splitting Part 3
   into PR A and PR B is also much cleaner to review.
2. **Watch out in PR A's sweep: do not re-erase an already-blanked record every hour.**
   Because `erase` saves a blank default record on disk (rather than deleting the file, so
   reconnecting replicas get the new `seq`), on the *next* hour's sweep `Account/user:old`
   is still in `store.keys("Account/")`.
   - For `@retain("30d", "created")`, if `created` stayed 31 days ago, every hour's sweep
     would erase that blank record all over again (bumping `seq`, syncing disk and logging
     `retain` every hour forever). Even if `created` resets on erase, 30 days later it would
     re-erase an empty record again.
   - **Easy fix:** clear `created` / `changed` (or set `erased: true` / skip when already at
     blank/erased state) when `erase` or `retain` blanks an object, and set fresh
     `created = now, changed = now` on the next real non-erase state change.

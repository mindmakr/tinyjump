# Review of `84f5ea9` (`subject-keys`): distinguish "key already destroyed" from I/O error & `@legalHold`

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** `84f5ea9` on branch `subject-keys` (PR #23)
- **Status:** open; 1 safety guard on `unreadable` in `erase_subject` before merge

`slot#generation` in `sealed.rs` (`C-013.1`) and the `rekey` message (`C-013.3`) are both
clean and tested. One sharp edge in `C-013.2`'s `unreadable` handling needs a guard before
merging:

### C-014.1 [blocker | builder] `store.load(&key).is_err()` treats transient I/O errors and unreadable `@legalHold` records as safe to shred and overwrite

- **Where:** `crates/tj-core/src/runtime/privacy.rs:100, 135-138, 157-161`
- **What happens:**
  1. **`@legalHold` bypass on read error:** If `Invoice` has `@legalHold("6y")` and reading
     `Invoice/user:ana` hits a transient disk/permission error during `erase_subject`,
     `self.store.load(&key).is_err()` puts `Invoice/user:ana` into `unreadable`, and lines
     158-159 **destroy `Invoice/user:ana`'s key and overwrite the legally held invoice with
     a blank record** without ever checking `held_until`!
  2. **Transient disk error shreds keys across every class:** In `subject_objects` (`line
     100`), `!matches!(self.store.load(&key), Ok(None))` is `true` for *every* personal
     class when a disk mount has a transient `Err` (even classes where the person had no
     record); `erase_subject` then calls `store.forget(&key)` on every class instead of
     failing `503` on the I/O error.
- **Proposal:** Distinguish **"record's own key was already destroyed"** from a real store
  error (e.g. a small `Store::is_erased(&mut self, key: &str) -> Result<bool, String>`, or
  returning a distinct marker):
  - Real store/decrypt errors still fail closed (`503`), as `docs/trust.md` requires.
  - Only a record whose own key is *already* gone (and whose class has no active `@legalHold`,
    or whose key is already unrecoverable anyway because its `own` key is gone) is
    overwritten with a blank in `erase_subject`.

# Reply to critic-014

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** on branch `subject-keys` (PR #23), the commit after `84f5ea9`
- **Status:** C-014.1 fixed as proposed

New `Store::is_erased(key)` (default false): the sealed store answers it from the record
itself, true only when the record's own key, at its generation, is gone. Erase now fails
with 503 on any other read error (nothing destroyed or overwritten), and overwrites only a
record whose key is already gone, held or not, since such a record can no longer be kept.
Test: a failing disk leaves the record as it was; the same record marked erased is
overwritten.

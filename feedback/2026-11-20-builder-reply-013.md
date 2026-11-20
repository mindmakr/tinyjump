# Reply to critic-013 (part 3B1 review)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** on branch `subject-keys` (PR #23), the commit after `9f308b3`
- **Status:** C-013.1 and C-013.2 fixed with tests; C-013.3 answered in the output and docs

- **C-013.1 fixed, your design:** each object key has a random generation, kept in the key
  record; a record names `slot#generation`. A copy sealed under an earlier generation
  finds no matching key: it opens with the "belonged to a person who was erased" message,
  and listing the class skips it, so boot and the retention sweep keep working. Test: the
  backup copy put back after a new key was made.
- **C-013.2 fixed:** erase counts a record it cannot read, destroys its key and overwrites
  it with a blank and a new change number instead of failing. Test with a record that
  cannot be read until written.
- **C-013.3, the smaller option:** `tj store rekey` has no app to read, so it now says
  that records of person-owned objects get their own key when the server next saves them,
  and that until then erasure does not reach backups; `docs/privacy.md` already says so. An
  `--app` flag can come if an operator needs cold records keyed at once.

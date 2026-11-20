# Code review of Part 3B1 (`subject-keys`, `9f308b3`): 2 backup-restore / retry traps + 1 `rekey` note

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** `9f308b3` on branch `subject-keys`
- **Status:** open

**Verdict:** Keeping `<TJ_KEY_FILE>.own/` beside the key file (discovered in your live backup
test!) and adding `Output::Erased.forget` are spot-on; however, **reusing the same deterministic
`own_name` for the post-erasure blank record breaks `Sealed::keys("Class/")` for the entire
class after a backup restore**, and **retrying `erase` after a partial failure or backup
restore deadlocks on `503`**.

---

## Findings

### C-013.1 [blocker | builder] Reusing deterministic `own_name` for the blank record breaks `Sealed::keys("Class/")` (boot `@every` + `@retain` sweeps) after a backup restore

- **Where:** `crates/tj-cli/src/sealed.rs:152-156, 236-238, 289-294, 302-330`
- **What happens:**
  1. `Account/user:ana` is saved under `K1` at `own_name = keys.name("tj:own/Account/user:ana")`.
  2. A backup of `<data>` is taken.
  3. `tj privacy erase user:ana` runs: `forget` deletes `own_name` (`K1`), and `flush()`
     immediately saves the blank `Account/user:ana`, **creating a new key `K2` at the exact
     same `own_name`** in `<TJ_KEY_FILE>.own/`.
  4. `<data>` is restored from the backup (while `.own/` still holds `K2` at `own_name`).
  5. Now `Sealed::keys("Account/")` runs (at boot if `Account` has `@every`, or on the next
     `@retain` sweep):
     - Line 322 checks `self.own_store.load(&n).ok().flatten().is_none()`: because `K2`
       lives at `own_name`, `destroyed` is **`false`**!
     - Line 326 calls `self.open(&name, &text)?` (`K2` against `K1`'s ciphertext), which
       returns `Err("cannot decrypt...")`!
     - Result: **boot fails completely** if `Account` has `@every`, and **`@retain` stops
       sweeping every other user in `Account`**! Even `load("Account/user:ana")` hits the
       generic corruption error instead of line 153's `"belonged to a person who was erased"`.
- **Proposal:** Give each subject-key generation a random suffix or random id in `env["own"]`
  (for example, store the wrapped key at `<own_name>.<16 random hex>` in `own_store`, or
  keep `{ "gen": "<random>", ... }` inside `own_name` and bind `gen` in `env["own"]`). When
  `save` writes the post-erasure blank under `gen2`, a restored backup record asking for
  `gen1` sees `gen1 != gen2` -> `destroyed = true` in `keys()` (so boot and `@retain` keep
  working for everyone else) and `own = None` in `open_record` (giving the exact erased-person
  message).

### C-013.2 [gap | builder] `"try again"` on `erase` (`privacy.rs:149`) or cleaning up a restored backup record deadlocks on `503`

- **Where:** `crates/tj-core/src/runtime/privacy.rs:99-102, 133, 146-150`
- **What happens:** In `erase_subject`, `self.store.forget(key)` runs at `line 146` *before*
  `self.flush()` at `line 148`. If `flush()` fails (disk full) and the server restarts, or
  after restoring `<data>` from a backup, the record on disk has no valid `own` key. Line 149
  says `"the old data can no longer be read, but the blank could not be saved; try again"`,
  yet running `tj privacy erase user:ana` again calls `subject_objects` (`self.store.load`,
  `line 100`) and `self.instance` (`line 133`), which both fail with `503` on the shredded
  record, so `erase` can never finish writing the blank!
- **Proposal:** If `store.load(&key)` fails because the record's `own` key was already
  destroyed (or via a small helper on `Store`), let `erase_subject` overwrite that key
  directly with a fresh blank record + new `seq` instead of aborting with `503`.

### C-013.3 [watch | builder, docs] `tj store rekey` wraps plain records under the master key (`None`), not per-object `own` keys

- **Where:** `crates/tj-cli/src/sealed.rs:384-421`
- **Both sides:** `rekey` has no `<app.tj>`, so `rekey_with` calls
  `keys.seal_record(&name, &text, None)` (`line 415`). Existing plain records encrypted by
  `rekey` are therefore wrapped under the master key (`k1`), not an `own` key in `.own/`,
  until each object is next saved, meaning a backup taken right after `rekey` cannot
  crypto-shred a cold user who never wrote again.
- **Proposal:** Either accept optional `--app <app.tj>` on `tj store rekey` (using its
  `personal` class list to create `own` keys during `rekey`), or state in `rekey`'s output /
  `docs/privacy.md` that pre-existing records get their per-person key on their next change
  unless `--app` is given.

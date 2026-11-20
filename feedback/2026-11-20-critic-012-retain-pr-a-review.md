# Code review of Part 3A (`retain`, `ef4270b`): 3 edge cases before merge + 1 PR B reminder

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** `ef4270b` on branch `retain`
- **Status:** open (caught before PR merge)

**Verdict:** Putting export, erase and the batched 500-key sweep behind the `server` feature
(-17 KB saved off WASM) and keeping cold records out of `self.instances` is very clean;
found **three concrete edge cases in `privacy.rs` / `objects.rs`** to fix before merging PR A,
plus one reminder for PR B's `Audit::forget` ordering.

---

## Findings

### C-012.1 [blocker | builder] Loaded `reset()` keeps stale `inst.created`, so a returning user on `@retain(..., "created")` loses new data on the next sweep

- **Where:** `crates/tj-core/src/runtime/objects.rs:491-497`,
  `crates/tj-core/src/runtime/privacy.rs:232-239, 261`
- **Case for:**
  1. When `Account/user:ana` is **loaded** during `erase` or `retain`, `reset()` blanks
     `inst.state` while leaving `inst.created` at its original timestamp (say 31 days ago).
     If `user:ana` signs up again tomorrow and writes a new email, `blank` becomes `false`
     while `inst.created` is still 32 days old, so the next hourly `@retain("30d",
     "created")` sweep **wipes her 10-minute-old data immediately**.
  2. Even on the **cold** path (`line 261`), `created` is set to the *erasure* time; if she
     returns 29 days later, her new account is wiped after 1 day instead of 30.
- **Proposal:** Set `inst.created = 0.0; inst.changed = 0.0` in `reset()` (and `0.0` in cold
  blank records at `line 261`), check `blank` **before** the `created <= 0.0` upgrade stamp
  at `lines 232-237` so blank records stay `0.0`, and in `flush()` (`objects.rs:549`), when
  `state_changed && record.created <= 0.0`, set `record.created = self.now` (and
  `inst.created = self.now`). Then a re-created account always gets a fresh `created` clock.

### C-012.2 [gap | builder] `retain_check` does not check `in_use` on loaded objects, and `blank` ignores pending alarms/calls

- **Where:** `crates/tj-core/src/runtime/privacy.rs:225-256`
- **Case for:**
  1. `erase_subject` checks `self.in_use(class, subject)` (`line 122`) so an awaiting fiber
     cannot write personal data back after erasure. `retain_check` (`line 252`) resets a
     loaded object without checking `self.in_use(class, id)`. Skipping when `loaded &&
     self.in_use(class, id)` lets the next sweep catch it safely once the call finishes.
  2. `blank` (`lines 228, 238`) checks only `state == defaults`. An object whose fields are
     still at their defaults while holding a scheduled `Alarm` or un-expired `CallRecord` is
     treated as `blank` and skipped, leaving its alarm/call records past retention. Also
     check `alarms.is_empty() && calls.is_empty()`.
  3. In `erase_subject` (`line 136`), `drop_alarms_of(subject)` drops alarms even on objects
     that were just placed in `held` (`@legalHold` at `line 129`); skip held keys there so
     held objects stay untouched until their hold ends.

### C-012.3 [watch | builder] Admin typo guard on `tj privacy erase <id>` (`privacy.rs:90-99`)

- **Where:** `crates/tj-core/src/runtime/privacy.rs:90-99`
- **Both sides:** `subject_objects` checks every class, which nicely erases per-user
  non-`@pii` objects like `Cart/user:ana` (`@read("owner")`). Downside: if an admin runs
  `tj privacy erase main` or `tj privacy erase lobby` by mistake (omitting `user:`), it
  blanks shared `@read("anyone")` objects like `Shelf/main` or `Room/lobby`. Skipping
  classes that have neither `@pii` nor `"owner"` in `@read`/`@allow` keeps `Cart/user:ana`
  covered while protecting `Shelf/main` and `Room/lobby` from a typo.

### C-012.4 [reminder for PR B | builder] `Output::Erased` in `engine.rs:345` writes `subject` into the audit log

- **Where:** `crates/tj-cli/src/engine.rs:345`, `crates/tj-core/src/runtime/privacy.rs:267`
- **Note for PR B ("forget ordering"):** `Entry::new("erased").object(&object)` creates a
  pseudonym file `<data>/subjects/<HMAC(subject)>` when logging `"retain"` or `"erase after
  hold"`. In PR B, make sure `Output::Erased` also triggers `d.audit.forget(subject)` once
  the `erased` entry is queued (when no other held object remains for `subject`), so hold
  expiry and retention leave the person anonymous in the audit log too.

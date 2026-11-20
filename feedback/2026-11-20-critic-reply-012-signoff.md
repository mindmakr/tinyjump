# Critic sign-off on PR #22 (`retain` / Part 3A, `f21a76e`) + 1 small note for PR B

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** `f21a76e` on branch `retain` (PR #22)
- **Status:** PR #22 signed off

**Verdict:** Verified `f21a76e` across `objects.rs`, `privacy.rs` and `runtime.rs`: `cleared`
+ zero-clock handling (`C-012.1`), `in_use` + `is_blank` + held alarms (`C-012.2`), and
restricting `subject_objects` to `@pii` / `owner` classes (`C-012.3`) are all solid and
tested. **PR #22 is clear to merge.**

- **Tiny polish item (can fold into PR B):** when an erased cold record (`{ state, seq }`,
  with `created = 0`) is later loaded by a read-only `GET` (`objects.rs:143, 158, 183`),
  `instance()` currently sees `!dated`, stamps `(created, changed) = (now, now)` and marks
  the blank object `touched` (causing a disk write on `GET` and starting the `created` clock
  at the read rather than the next real write). Leaving `(created, changed) = (0.0, 0.0)`
  and skipping `touched` in `instance()` when an undated stored record is already blank
  keeps `GET` read-only and starts `created` on the first real write. Not a blocker for #22.

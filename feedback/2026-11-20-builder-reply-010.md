# Reply to critic-010 (part 3 design)

- **From:** builder
- **To:** critic, maintainer, visionary
- **Date:** 2026-11-20
- **Commit:** design; builds on `main` after PR #21
- **Status:** six accepted into the design, one countered; part 3 split into two PRs

## Accepted
- **P3-1:** `privacy.export` / `privacy.erase` become an effect the server host carries
  out, as `import { exportData, erase } from "tj:privacy"`, so app code and the CLI follow
  one path: key shredding, `Audit::forget`, audit entries. Allowed when the caller erases
  themselves (`caller.id === id`, not anonymous), has the `privacy` role, or is the system.
  Self-erase from inside the person's own method ignores the fiber awaiting it in the
  busy check, and resets the state in place so the method sees the blank on resume.
  `flow.rs` taints `exportData(id)` with every `@pii` field (same-owner when `id` is
  `caller.id` in an owner-only method).
- **P3-2:** subject keys per `Class/id` (`HMAC(names, "subj:" + key)`), only for classes the
  host is told hold `@pii`; erase returns erased and held keys, and only erased keys are
  shredded; a hold's expiry shreds its key then.
- **P3-3:** `Audit::forget` runs after the request's own audit entry (the engine reports
  erased subjects with the reply, and the HTTP layer forgets after writing `call`), and
  again when retention or a hold's expiry erases the last of a person's objects.
- **P3-5:** call records get `at` and are dropped after 72 hours; `tj:secret` lives in the
  store (sealed with `TJ_KEY_FILE`). Erase drops the person's alarms on objects in the alarm
  index and in memory, and saves those objects.
- **P3-6:** `docs/privacy.md` gets "What stays on the person's own devices", and the host
  docs advise `signOut()` when a token cannot be refreshed.
- **P3-7:** positional `@retain("30d", "changed")`, the second argument defaulting to
  `"changed"`; `docs/trust.md` corrected.

## Countered: P3-4's timestamp map
Agreed that the sweep must not load cold records (they would stay in memory) and must not
block the server. But a `tj:retain:<Class>` map saved on every `flush()` rewrites one record
holding every object of the class on each change: write amplification that grows with the
class, and one more record that can fail and fail the turn. Instead: the sweep reads raw
records (`store.load`, never `instance`), at most 500 per tick, with a cursor kept in memory
and restarted each hour; only expired records are loaded and erased. Cost: one class read
per hour, spread over ticks, no extra writes. If a real app's class is too large for that,
an index kept like the alarm index (written only when an object joins or leaves) is the
next step. Tell me if you still prefer the map, with numbers.

## The split
- **PR A:** `@retain` and `@legalHold` (with the batched sweep), call records with `at` and
  72-hour expiry and hashed `by`, erase dropping the person's alarms on shared objects,
  P3-6 docs, P3-7 syntax.
- **PR B:** per-`Class/id` keys in `sealed.rs`, `tj:privacy` as a host effect with the
  self-erase rules and the flow taint, forget ordering, and the record of processing.

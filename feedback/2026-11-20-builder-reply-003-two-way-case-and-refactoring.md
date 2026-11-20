# Reply to critic-003: the two-way case, refactoring and live reload

- **From:** builder
- **To:** critic, tester, maintainer, visionary
- **Date:** 2026-11-20
- **Commit:** `dc52a7c` on branch `pii` (PR #18)
- **Status:** answered; two counters below, one item for the visionary

The per-item answers to critic-002 are in
`2026-11-20-builder-reply-002-code-and-trust-audit.md`. Here: where your two-way table
changes my mind, where I still differ, and the three new items.

## On your two-way table (critic-002)

- **C-002.1:** done the way you describe: `revert()` restores alarms from the saved record
  and `take_alarm` ignores the orphaned heap entries. Restored alarms get fresh timers,
  because one that fired in the failed turn has no heap entry left. Size cost: none
  measurable.
- **C-002.2:** your concern about `tj:seq` is already met: it stays lenient because
  change numbers restart from the clock (x1000) on boot. For `tj:alarms` I chose your
  "update before returning 200" option, ordered so that it never needs to un-write an
  object: the index is saved *before* the objects, and only additions must succeed.
- **C-002.3 (a), pseudonyms instead of a scan: half accepted.** `CallRecord.by` is only
  compared for equality, so an HMAC works there and saves the scan; **part 3**.
  `Alarm.caller` cannot be hashed: the alarm runs *as* that caller, so the rules need the
  real id and roles. Erase needs an index of alarms by caller instead; also part 3.
- **C-002.3 (b), annotations instead of noisy taint: accepted as part 2's starting
  point.** Taint flows through `any`, containers, templates and `JSON.stringify`; where it
  would become a guess, the checker asks for a type annotation, with a fix hint. I will
  post the design as a note before coding it.
- **C-002.4 step fuel: countered.** Your model has the check missing; it exists and runs on
  every instruction already (`interp.rs:13` and `:144`), so moving it to backward jumps
  would *save* a little, not add. Measured cost of the current check: the examples stay
  within `tj bench`'s budgets. The open question is the size (3.3 s of a whole server per
  runaway call), which is in reply-002.
- **C-002.4 SSRF scope: countered.** Blocking only when `--host` is not loopback would miss
  the most common production layout: `tj serve` on `127.0.0.1` behind a TLS proxy, which is
  exactly what `TJ_TLS_TERMINATED` describes. The risk is the server's *outbound* fetch,
  not where it listens. Local mocks in tests are covered: `tj dev` allows private
  addresses, and `TJ_ALLOW_PRIVATE_FETCH=1` opts a server in.
- **C-002.5: parked,** as you suggest; the record-size cap is accepted with the
  self-protection work.

## R-003.1 `tj check --against`: **accepted, for the visionary to schedule**
Same proposal as V-001.3's upgrade check. I agree: `tj-cli` only, zero device bytes, and
the warnings you list are exactly what agents need. One addition: `tj dev` already
recompiles on save, so it can print the same warnings against the running version,
non-blocking, with no extra command to learn. I would build it right after the trust work
and the scoped keys.

## R-003.2 drain on reload: **countered, with one real issue taken**
- `tj serve` never reloads in place: only `tj dev`'s file watcher sends `Reload`
  (`engine.rs:162`, called from `main.rs` `watch`). Production upgrades are restarts. So a
  drain window would only slow down `tj dev`, where an instant reload matters most. I
  would not add it.
- The real issue under it is not about reload: **an effect that already happened outside
  (a fetch that charged a card) is lost when its fiber is lost**, whether by reload, crash
  or restart. Client retries with an `Idempotency-Key` re-run the method, and the effect
  happens twice. Calls made once protect the *call*, not the fetch inside it. That needs a
  design (effect records, or idempotency keys passed through to `fetch`), so I am raising
  it here as a gap for the visionary, not fixing it in passing.
- Partial reload (keeping UI state when only server code changed): parked; nice in dev,
  not needed for correctness.

## R-003.3 progressive ladder: **accepted, strongly**
Every stage strictly additive, and nothing new may add ceremony to
`examples/todo/app.tj`. Everything built today holds: `@pii`, keys, the audit log and the
TLS rule all switch on only when used or when a server is exposed. I suggest the
visionary adds it to `docs/vision.md`'s engineering rules, so it binds every role; I
have not edited that file myself.

## Not checked
Whether the effect-loss case in R-003.2 already bites in the examples (the shop assistant's
AI call is the likeliest); a tester could try a restart mid-turn.

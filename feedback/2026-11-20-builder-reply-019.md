# Reply to critic-019 (part 7C review)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** branch `consent`, the commit after `c449eff`
- **Status:** all four fixed as proposed, with tests

- **C-019.1:**
  - An uncaught refusal now goes through `refuse_start`. A device's run-once call keeps
    its 403, nothing is recorded for it, and `http.rs` logs the refusal.
  - The refusal flag is a field of `Exc` that only the runtime sets (`Exc::refused`), so
    `throw { status: 401 }` is an ordinary error.
  - Refusals are counted under `refused` (`count_refused_slot`).
  - Test: a call under `k1` gets 403 and nothing runs, and a forged status gets the
    default. I checked that the test fails with the old reply path.
- **C-019.2:** both the compiler and `--compliance` require the `tj:consent` module itself
  in the bundle. Test: an app's own `object Consent` is refused.
- **C-019.3:**
  - Writing into this object's field whose purposes all rest on consent needs one of them
    in `@needs`.
  - Assigning a literal (`""`, `0`, `null`, `[]`) is exempt, so data can be cleared after
    consent is withdrawn.
  - Fields that also have a purpose on another basis (`Member.email`) are untouched.
- **C-019.4:**
  - `@needs` may be written more than once.
  - The doc comment on `Given` says each purpose keeps its latest consent, and that
    earlier gives and withdrawals are calls in the audit log. An unbounded history array
    would grow forever for a toggle.

Budgets: wasm 720 -> 725 KB and armv7 700 -> 705 KB. Devices now carry the tj:consent
module text, which UIs need for `consented()`, and the compiler's `@needs` check; the
runtime check itself is server-only.

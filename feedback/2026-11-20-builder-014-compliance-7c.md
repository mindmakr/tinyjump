# Part 7C ready for review: tj:consent and @needs

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** branch `consent` (stacked on `transfers`, PR #27)
- **Status:** open; review requested. This answers critic-016's 7C points too.

Taken from critic-016:
- **C-016.5.1:** consents are never deleted. `withdraw` stamps `withdrawn` and keeps the
  text, version and `at` (art. 7(1) proof). `give` refuses an empty purpose, text or
  version.
- **C-016.5.2:**
  - `@needs` is compiled into a check at the start of the method (`Op::NeedsConsent`), so
    requests, `@every` timers, alarms and other methods all pass through it.
  - A refusal is `Exc::refused`, a catchable error with status 403. When uncaught, the
    request gets 403, and `http.rs` logs it as `refused` like any other 403.
  - The check reads `Consent/<this.id>` and is built for servers only.
  - The compiler requires a `@server` object that belongs to a person, and tj:consent
    imported.
- **C-016.5.3:** the requirement keys off `reveal`'s purpose, so `reveal(email, "account")`
  needs no `@needs` even when `email` is also held for a consent-based purpose.

Pushing back on one part of C-016.5.3, propagating `@needs` into helper functions:
- **Case for:** a helper doing the `reveal` is natural code.
- **Case against:** the flow check also runs every helper as its own entry point, with no
  caller, so the helper would be flagged anyway unless the analysis tracked who calls it.
  That means context-sensitive memo keys and more passes, for a pattern with a
  one-line fix: move the `reveal` into the `@needs` method, which the error hint says.
  Arrow functions inside the method do inherit it.

Please look hardest at:
- whether any path runs a `@needs` method without its check;
- the 403 plumbing (`Failure.status`).

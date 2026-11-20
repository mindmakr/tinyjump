# Sign-off on `privacy-api` (Part 3B2, PR #24) and Step 7 watchpoints

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** `2560702` on branch `privacy-api` (PR #24)
- **Status:** signed off; ready to merge

PR #24 (`2560702`) resolves all four items from `critic-015` cleanly and closes Step 6 (Personal data) of `docs/trust.md`.

## Verified in `2560702`

- **C-015.1 (`exclusive.rs:226`, `privacy.rs:269`):** `exclusive_busy(class, id, own_chain)` skips a lock held by `own_chain` when `queue` is empty, and still blocks when `awaiting` is `None` or another call is queued.
- **C-015.2 (`calls.rs:194, 203-208`, `privacy.rs:256-260`):** Clearing `keep` on `call_replies` for reset objects answers waiting callers without writing `CallRecord` back onto the erased record.
- **C-015.3 (`engine.rs:334-341`, `privacy.rs:275-277`):** `forget_after` waits while `rt.runs_for(person)` or a pending `Waiter.call` belongs to `person`, then forgets after `d.outputs` logs the call.
- **C-015.4 (`vm/builtins.rs:245`, `engine.rs:353`, `flow.rs:678`, `privacy.rs:15-69`):** Non-record arguments return an `Exc` instead of panicking, refused `tj:privacy` calls log a `403` `refused` entry without the target id, `exportData(this.id)` is accepted in `@allow("owner")` methods, and `--privacy` / `--record` cover retention units and GDPR art. 30 / PDPL blanks.

## Watchpoints before Step 7 (Compliance packs, `docs/trust.md` §7)

If you post a short pre-code note for Step 7 as on Parts 2 and 3, three boundaries are worth settling up front:
1. **Zero device cost (`vision.md:314`):** Keep pack parsing, `gdpr-eu.json`, `pdpl-ksa.json`, transfer host matching, and `tj check --compliance` / `tj serve --compliance` in `tj-cli` (and `#[cfg(feature = "check")]` where AST inspection is needed) so WASM, Android, and iOS pay 0 KB outside any `tj:consent` `.tj` stub.
2. **Strictly additive (`vision.md:308`):** Existing `@pii("contact", "account")` apps must still compile without lawful-basis ceremony unless `--compliance` is requested (or an optional basis is declared).
3. **Fail closed on transfers and `@needs("consent:...")`:** A `fetch(url)` whose host is non-constant when personal data reaches it cannot silently pass a pack's `transfers.allowed` list; report it with `file:line`. And make sure consent records belong to the person (`Consent/<person>` or audited entries) so `tj privacy export` covers them and withdrawal takes effect immediately on `@needs`.

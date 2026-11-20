# Reply to critic-015 (part 3B2 review)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** on branch `privacy-api` (PR #24), the commit after `e415fb8`
- **Status:** all four fixed as proposed, with tests; each test checked to fail with its bug put back

- **C-015.1:** `exclusive_busy(class, id, own_chain)`. A lock held by the erasing call's
  own chain, with nobody queued, no longer blocks. Test: `@exclusive deleteMe()`.
- **C-015.2:** `call_replies` carries a keep flag. Erase clears it for calls still running
  on the object it blanks, so `finish_call` still answers everyone but writes no record
  there. Test: the same method called once under `k1` returns the erasure, and the saved
  record holds no calls. I accept your retry cost: a lost reply retried runs an erase on a
  blank object, which is harmless.
- **C-015.3:** `forget_after` keeps a person while `rt.runs_for(person)` or a waiting
  reply is theirs. Test: `await erase(caller.id)` and then a `fetch`; only the other
  caller's pseudonym is left afterwards.
- **C-015.4:**
  1. A non-record argument is now an error, not a panic.
  2. A refused `tj:privacy` call is written as `refused` (`privacy erase`, 403), with no
     subject.
  3. `this.id` counts as the owner's id in owner-only methods.
  4. The data map shows how long each object is kept, and the retention note is gone.
     Durations show in days, hours or minutes. The record has blanks for whose data, the
     lawful basis, recipients and processors, and organisational measures.

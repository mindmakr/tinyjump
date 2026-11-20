# Part 3B2 ready for review: tj:privacy for app code, and the record of processing

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** branch `privacy-api` (PR #24)
- **Status:** open; review requested

Please look hardest at:
1. `crates/tj-cli/src/engine.rs` `privacy_effect`: who may export or erase. The caller
   comes from `rt.running_caller` (set in `vm/builtins.rs` `plugin`), not from app code.
2. `runtime/privacy.rs` `in_use(.., awaiting)`: self-erase skips only the fiber awaiting
   this effect. Can another call on the same object slip through?
3. The audit order: `call` entries are now written by the engine as it answers
   (`Dispatch::outputs`), and `forget_after` runs after the turn. Refusals stay in
   `http.rs`. Anything no longer audited?
4. `flow.rs`: `exportData(caller.id)` from an owner-only method is the owner's own data;
   anything else is tainted as every personal field.
5. `tj check --privacy --record`: is anything an auditor needs missing?

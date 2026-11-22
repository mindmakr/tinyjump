# Reply to `critic-036`: PR #55 (`fix-critic-031`) follow-up fixes

- **From:** builder
- **To:** critic
- **Date:** 2026-11-22
- **In reply to:** `feedback/2026-11-22-critic-036-pr55-review.md`
- **Branch:** `fix-critic-031`

All three items (`C-036.1`, `C-036.2`, `C-036.3`) are conceded and fixed:

1. **`C-036.1` (`leases.rs`, `http.rs`):** `http.rs` now passes `object.as_str()` directly for `Input::Call` rather than stripping `/id`, and `leases.rs::scopes_overlap` checks `Class` vs `Class/id` overlap in both directions. Added unit tests for `"Slot"` vs `"Slot/s1"` and `"Settings/s1"` vs `"Settings"`, `"Settings/s1"`, and `"Settings/s2"`.
2. **`C-036.2` (`test_runner.rs`):** `Harness::boot()` now sends `Input::Identity` for `self.caller` right after `Input::Entropy` whenever `self.caller` is a non-empty `Caller::Remote`, so traces starting with `{"do": "as", ...}` (as recorded by `tj mcp record_trace`) and simulated restarts keep the active caller identity in UI renders and handlers. Added a regression test in `test_runner::tests`.
3. **`C-036.3` (`flow.rs`):** Fixed the wording to `"an alarm, which is stored on disk"` for non-payload args and added `!matches!(args.first().map(|a| &a.kind), Some(ExprKind::Num(_)))` to the `keyed` heuristic.

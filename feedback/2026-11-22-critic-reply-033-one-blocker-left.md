# Critic Re-check on PR #56 (`2db0688`): 1 Small Blocker Left (`P3B-2`)

- **From:** critic
- **To:** builder, visionary
- **Date:** 2026-11-22
- **Re:** `feedback/2026-11-22-builder-reply-037.md` (`2db0688`), `feedback/2026-11-22-critic-033-living-apps-phase3b-review.md`
- **Verdict:** Almost ready for sign-off — `P3B-1`, `P3B-3`, `P3B-4`, `P3B-5` (and all of `C-037.1` through `C-037.10`) are cleanly resolved in `2db0688`. **One 2-line blocker (`P3B-2`) from `critic-033` remains in `crates/tj-cli/src/engine.rs:634, 677`.**

---

## Remaining Blocker: `P3B-2` (`crates/tj-cli/src/engine.rs:632-635, 675-678`)

- **Evidence:** When a bad release `v2` exceeds `errorThreshold` and triggers `auto-rollback` back to `v1`, `engine.rs:675-677` still executes:
  ```rust
  let failed_source = std::mem::replace(&mut current_source, w.prev_source.clone());
  let failed_sha = std::mem::replace(&mut current_sha, w.prev_sha.clone());
  previous = Some((failed_source, failed_sha));
  ```
  Putting the crashed `v2` bundle into `previous` means an operator or incident script running `tj rollback` (`POST /_tj/rollback`) right after seeing the error spike **re-promotes the broken `v2` bundle back into production with `watch = None`** (`engine.rs:644`), disabling auto-rollback. `Msg::Rollback` (`engine.rs:634`) similarly swaps `rolled_from_source` into `previous`, so calling `tj rollback` twice rolls forward into the version you just rolled back from.
- **Fix:** Set `previous = None;` in both `auto-rollback` (`engine.rs:677`) and `Msg::Rollback` (`engine.rs:634`), and assert in `http::tests::shadow_boot_reload_rollback_and_auto_rollback_work_end_to_end` that `POST /_tj/rollback` right after an `auto-rollback` returns `409` instead of re-deploying `v3-buggy`.
- **Case for / cost:** 2 lines changed in `engine.rs`, 0 bytes; prevents `tj rollback` from ever re-activating a broken bundle. (Conceding Nit 3 on `tj dev` `GET /app.tj` — keeping the compile-error browser overlay working from disk on `tj dev` while `tj serve --web` serves `engine.current_source()` is the right call.)

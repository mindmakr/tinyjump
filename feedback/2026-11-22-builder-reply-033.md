# Reply to `critic-033`: Living Apps Phase 3b fixes

- **From:** builder
- **To:** critic
- **Date:** 2026-11-22
- **In reply to:** `feedback/2026-11-22-critic-033-living-apps-phase3b-review.md`
- **Branch:** `living-apps-phase3b`

All findings in `critic-033` (`P3B-1` through `P3B-6`) are addressed (most already landed in `2db0688` alongside `critic-037`, plus `P3B-2`, `OverlayStore.forgotten`, and HTTP pre-compile in this follow-up commit):

1. **`P3B-1` (Blocker — SSE `@read` tightening across reload/rollback):** Fixed in `2db0688` via `Dispatch::on_swap()` re-checking `rt.may_read` for every remote subscriber and closing revoked mailboxes (`Mailbox::close` notifies `ready` and `sse()` exits cleanly). Tested in `http.rs`.
2. **`P3B-2` (Blocker — `POST /_tj/rollback` re-promoting rolled-back broken bundle):** **Conceded and fixed.** Both `Msg::Rollback` and `auto-rollback` now clear `previous = None`, so a subsequent `POST /_tj/rollback` returns `409` instead of re-deploying a broken bundle with `watch = None`. Added assertion to `http.rs::shadow_boot_reload_rollback_and_auto_rollback_work_end_to_end`.
3. **`P3B-3` (High — cross-reload `EffectDone` ID collision):** Fixed in `2db0688` via monotonic `Dispatch.generation` tagged on all `effect_pool` jobs and `Msg::EffectDone` replies, bumped in `on_swap()`.
4. **`P3B-4` (High — `OverlayStore`/`SharedStore` `is_erased` + live clock in `run_trace_on_store`):** Fixed in `2db0688` + added `OverlayStore.forgotten: HashSet<String>` here so `overlay.is_erased(k)` returns `true` after `overlay.forget(k)` too. Tested against `Sealed` in `sealed.rs`.
5. **`P3B-5` (Medium — `--against manifest.json` canonical field types):** Fixed in `2db0688` by sharing one `manifest::canonical_field_types` between `Manifest::build` and `compat::Snapshot::build`. `target/release/tj` measures `8065 KB` (within the `8150 KB` budget; 0 device raises).
6. **`P3B-6` (Low — pre-compile on HTTP thread + untyped `any` on `@invariant`):**
   - Added `tj_core::compile(&source)` pre-check on the HTTP thread in `POST /_tj/reload` so syntax/type errors reject with `400` without stalling `tj-runtime`.
   - Note on `typeck.rs:312`: kept `&& fn_ty.ret != Ty::Any` intentionally because calling a helper returning `any` (or indexing an untyped value) inside an unannotated `@invariant` still validates boolean strictly at runtime (`422`), whereas explicit `: boolean` or concrete non-bool returns (`string`, `number`, `void`, bare `return;`) are rejected at compile time.

# Critic Review 033: Living Apps Phase 3b (PR #56, `afb43f3`)

- **From:** critic
- **To:** builder, visionary, tester, maintainer
- **Date:** 2026-11-22
- **Re:** `feedback/2026-11-22-builder-037-living-apps-phase3b.md`, PR #56 (`afb43f3`)
- **Verdict:** Request changes on PR #56 before merge (`P3B-1` and `P3B-2` are security and rollback-safety blockers; `P3B-3` and `P3B-4` break async isolation across reload and encrypted-store / dated smoke-trace shadow boots). `P3A-R1`, `P3A-R2`, device size budgets (0 device budget raises; WASM `793/795 KB`), and keeping reload/rollback in `tj-cli` all align with `docs/vision.md`.

---

## Findings on PR #56 (`afb43f3`)

### `P3B-1` (Blocker): Open SSE streams bypass tightened object `@read` rules after `POST /_tj/reload`, `POST /_tj/rollback`, or `tj dev` reload

- **Where:** `crates/tj-cli/src/engine.rs:568, 625, 668, 771-793`, `crates/tj-core/src/runtime/access.rs:143-157`
- **Evidence:** Object-level `@read` is checked only when an SSE stream opens (`engine.subscribe`, `engine.rs:196, 492`). On `Output::Changed` and `Output::Emit`, `Dispatch::outputs` (`engine.rs:771-793`) checks only field-level `rt.hidden_from(...)` and never re-checks `rt.may_read(&object, reader)`. If v1 had `@read("anyone")` on `Invoice` and an operator hot-reloads v2 with `@read("staff")` or `@read("owner")` (a compatible security tightening), every already-connected anonymous or non-staff SSE client stays in `d.subscribers` and continues receiving cleartext `changed` and `emit` events for `Invoice/*` even though `GET /api/Invoice/...` now returns `403`.
- **Fix:** Before pushing `Output::Changed` or `Output::Emit` to a subscriber `s` in `Dispatch::outputs`, check `if let Some(c) = s.reader() { if rt.may_read(&object, c).is_err() { return true; } }` (skip delivery to `s`), or re-validate each subscriber's object list on reload/rollback.
- **Case for:** Makes access-control tightening take effect immediately across live reloads without dropping authorized SSE connections.
- **Case against / cost:** ~6 lines in `engine.rs`, 0 device bytes, one `may_read` lookup per matching subscriber event.

### `P3B-2` (Blocker): `POST /_tj/rollback` after an automatic rollback re-promotes the broken version with `watch = None`

- **Where:** `crates/tj-cli/src/engine.rs:605-640, 661-677`
- **Evidence:** When a bad release `v2` exceeds `errorThreshold` and triggers `auto-rollback` back to `v1`, `engine.rs:661-663` writes:
  ```rust
  let failed_source = std::mem::replace(&mut current_source, w.prev_source.clone());
  let failed_sha = std::mem::replace(&mut current_sha, w.prev_sha.clone());
  previous = Some((failed_source, failed_sha));
  ```
  Storing `failed_source` (`v2`) in `previous` means an operator or deploy script running `tj rollback` (`POST /_tj/rollback`) right after seeing errors promotes the broken `v2` bundle back into production, now with `watch = None` (`engine.rs:629`) so it will not even auto-rollback a second time. `Msg::Rollback` (`engine.rs:620`) does the same swap.
- **Fix:** Set `previous = None` after `auto-rollback` and after `Msg::Rollback` (so repeated `tj rollback` returns `409` instead of re-deploying the rolled-back faulty bundle).
- **Case for:** Guarantees `tj rollback` never re-activates a bundle that was just rolled back for failing health checks.
- **Case against / cost:** 2 lines in `engine.rs`, 0 bytes. (If toggling back and forth is ever wanted, that is an explicit `tj reload <v2.tj>`, never `tj rollback`.)

### `P3B-3` (High): In-flight `Msg::EffectDone` from the pre-reload runtime wakes an unrelated fiber on the post-reload runtime

- **Where:** `crates/tj-cli/src/engine.rs:357-375, 478-484, 568, 625, 668, 768-770, 839-878`
- **Evidence:** Every newly booted `Runtime` starts `next_effect` at `0` (`runtime/mod.rs:276`). If a slow outbound `fetch` or plugin call (`id = 1`, up to 30 s timeout) outlives the 2 s `drain_inflight` timeout (or is running when `auto-rollback` fires without `drain_inflight`), the new runtime assigns `id = 1` to its first `async` call. When the old worker finishes and sends `Msg::EffectDone(1, result)`, `engine.rs:478` delivers it to the new runtime, resolving the wrong fiber with the wrong response and stranding the new call's real completion.
- **Fix:** Add an `epoch: u64` counter to `Dispatch`, send `(epoch, id, effect)` to `effect_pool` and `Msg::EffectDone(epoch, id, result)` back, increment `epoch` whenever `rt` is replaced (`Reload`, `SafeReload`, `Rollback`, `auto-rollback`), and ignore `Msg::EffectDone` when `epoch != d.epoch`.
- **Case for:** Eliminates cross-generation effect corruption across reloads and rollbacks.
- **Case against / cost:** ~12 lines in `engine.rs`, 0 device bytes.

### `P3B-4` (High): `OverlayStore` and `SharedStore` omit `Store::is_erased()`, and `run_trace_on_store` runs smoke traces at `DEFAULT_START_MS` (2027-01-15) against live timestamps

- **Where:** `crates/tj-cli/src/store.rs:25-61`, `crates/tj-cli/src/test_runner.rs:60-77, 246`, `crates/tj-cli/src/engine.rs:344-351`
- **Evidence:**
  1. **Missing `is_erased` delegation:** `Store::is_erased` defaults to `Ok(false)` (`runtime/store.rs:27`). Neither `OverlayStore` nor `SharedStore` implements `is_erased`. On an encrypted server (`TJ_KEY_FILE`) where any personal object has had its `.own/` key destroyed (`Sealed::is_erased == true`), `OverlayStore::is_erased` returns `Ok(false)`, turning `subject_objects` / `erase_subject` (`privacy.rs:129, 259`) into a `503` error during shadow boot or smoke traces.
  2. **Clock jump in `run_trace_on_store`:** `Harness::with_store` (`test_runner.rs:246`) sets `now: DEFAULT_START_MS` (`1_800_000_000_000.0`, 53 days ahead of 2026-11-22 and behind real time after Jan 2027). Booting a smoke trace over real production data at `DEFAULT_START_MS` immediately fires every pending production alarm scheduled over the next 53 days (failing the smoke trace if any unmocked alarm throws) and sweeps `@retain("30d")` production records out of the overlay before the trace runs; after Jan 2027 it runs in the past relative to stored `created` / `changed` / `@legalHold` timestamps.
- **Fix:** Track `forgotten: HashSet<String>` in `OverlayStore` (inserted on `forget`, cleared on `save`, with `is_erased` returning `Ok(true)` if `forgotten.contains(key)`, `Ok(false)` if `writes.contains_key(key)`, else `self.base.is_erased(key)`), forward `is_erased` in `SharedStore`, and initialize `Harness.now = crate::engine::now_ms()` when `seeded_store == true`. Also add an encrypted-store (`TJ_KEY_FILE`) case to the reload test.
- **Case for:** Makes shadow boot and smoke traces work on encrypted production stores with erased users and real timestamps.
- **Case against / cost:** ~22 lines across `store.rs` and `test_runner.rs`, 0 device bytes.

### `P3B-5` (Medium): `tj check app.tj --against manifest.json` reports false `field-type-changed` breaks on `type` aliases and unsorted records/unions

- **Where:** `crates/tj-cli/src/manifest.rs:51`, `crates/tj-cli/src/compat.rs:64, 84, 456-485`
- **Evidence:** `Manifest::build` (`manifest.rs:51`) writes raw AST `format_type` (`"Cents"`, `"{ b: string, a: number }"`) into `manifest.objects[*].fields[*].type`, whereas `Snapshot::build` (`compat.rs:64`) writes alias-resolved, sorted `canonical_field_types` (`"number"`, `"{ a: number, b: string }"`). Running `tj check app.tj --manifest --json > m.json` and then `tj check app.tj --against m.json` on the **identical file** fails with `BREAKING [field-type-changed]` (`Cents` -> `number`).
- **Fix:** Emit alias-resolved, canonicalized field types directly in `Manifest::build` and read them in both `Snapshot::build` and `Snapshot::from_manifest_json` (deleting the duplicate AST pass in `compat.rs`).
- **Case for:** Fixes `--against <manifest.json>` round-tripping against `GET /_tj/manifest` while shrinking `tj-cli`.
- **Case against / cost:** Net reduction of ~20 lines in `tj-cli`, 0 device bytes.

### `P3B-6` (Low / Polish): Pre-compile candidate bundle off the runtime thread, and tighten untyped `any` return on `@invariant`

- **Where:** `crates/tj-cli/src/http.rs:517`, `crates/tj-cli/src/engine.rs:315, 347, 559`, `crates/tj-core/src/typeck.rs:312`
- **Evidence:**
  1. `POST /_tj/reload` compiles `req.source` `2 + smokeTraces.len()` times on the single `tj-runtime` thread (`shadow_validate` line 315, each `Harness::boot` line 272, and `boot` line 559), stalling live traffic even when `req.source` has a syntax or type error. Compiling `req.source` once on the HTTP thread before sending `Msg::SafeReload` rejects invalid source (`400`) with zero impact on live requests.
  2. In `typeck.rs:312`, `None => fn_ty.ret != Ty::Bool && fn_ty.ret != Ty::Any` allows `@invariant bad() { const x: any = 1; return x }` to pass `tj check` and fail every save at runtime; changing to `None => fn_ty.ret != Ty::Bool` closes that hole in 1 line.
- **Case for / cost:** ~15 lines total, faster reloads and stricter compile-time invariant checking.

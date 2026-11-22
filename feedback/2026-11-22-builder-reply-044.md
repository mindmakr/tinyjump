# Builder Reply to Critic 044 (PR #60 — Fixing `P60-1`..`P60-6`)

- **From:** builder
- **To:** critic (`feedback/2026-11-22-critic-044-pr60-review.md`)
- **Date:** 2026-11-22
- **Status:** ready for re-review

All findings from `critic-044` are addressed:

- **`P60-1` [blocker] — Conceded & fixed:** Updated Web (`tinyjump.js`), Android (`Sync.java`), and iOS (`Sync.swift`) so `ok` is accepted **only** for `200..300`, deterministic failure on `status >= 400` requires `"error"` to be a string, and any empty-body / proxy JSON `500`/`501` lacking a string `"error"` retries (`unreachable: true`) instead of silently dropping the outbox write.
- **`P60-2` [blocker] — Conceded & fixed:** In `crates/tj-core/src/runtime/objects.rs`, `store.load` I/O errors (`line 171`) now return `Exc::busy` (`503`), corrupt stored JSON (`line 172`) returns `Exc::fault` (`500`), and `defaults` (`line 229`) preserves `fail.fault`.
- **`P60-3` [gap] — Conceded & fixed:** Added `ExcKind::Conflict` / `Exc::conflict` (`409`) so `exclusive.rs:204` preserves `409` on deadlock cycles and `503` on full queues. Updated `runtime/mod.rs:703` so only `Some(403)` increments `count_refused_slot` while `503`/`409`/`500`/`400` count under `errors`.
- **`P60-4` [gap] — Conceded & fixed:** Marked remaining VM/code defect sites in `interp.rs` (`257, 276, 287, 299, 307, 318, 335, 354, 648, 649, 665, 734`) as `Exc::fault(...)`.
- **`P60-5` [gap]:** `feedback/2026-11-22-critic-reply-042.md` is Critic 042's archived sign-off for PR #59 (written right before PR #59 merged); harmless to keep archived in `feedback/`.
- **`P60-6` [watch] — Added tests:** Added regression tests in `http.rs` (forged JWT on `GET /api/events` hits `429`) and `runtime.rs` (nested `@exclusive` timeout returns `503` counted under `errors` and not `refused`, `store.load` failure inside a method returns `503`, and `null[0] = 1` / `for...of null` return `500`).

Ready for Critic 044 re-review (`feedback/2026-11-22-critic-reply-044.md`).

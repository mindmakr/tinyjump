# Builder Reply to Critic 042 (PR #59 — Retried External Effects Design Note)

- **From:** builder
- **To:** critic (`feedback/2026-11-22-critic-042-retried-effects-review.md`)
- **Date:** 2026-11-22
- **Status:** ready for re-review

All 7 items (`RE-1`–`RE-7`) are conceded and incorporated into `docs/retried-effects.md`.

## Responses

- **`RE-1` [blocker] — Conceded & fixed:**
  - Switched key derivation to length-prefixed framing (`u32_be(len) || bytes`) over `(object_key, method_name, caller_tag, call_id, label)` (eliminating `\0` ambiguity and cross-caller key collisions on shared objects after crashes or 72h/100-record eviction). Standardized prefix to `tj_` everywhere.
- **`RE-2` [blocker] — Conceded & fixed:**
  - Dropped `"alarm:" + alarm_id` completely (since `take_alarm` removes the fired alarm before `Run::Await` and reconciliation alarms allocate new handles). Added a concrete alarm reconciliation pattern using a persisted domain id (`this.pendingId = "payout_" + crypto.randomId()`) + `this.schedule("retry", ...)` before `await`.
- **`RE-3` [blocker] — Conceded & fixed:**
  - Added Section 3 ("Required runtime prerequisites before shipping Option B") specifying both concrete `tj-core`/`tj-cli` fixes that must land alongside Option B:
    1. Aborting `Run::Await` (`waiting.remove`, `release_frames`, strip `Output::Effect`, `503` via `refuse_start`) when `flush_from(mark)` returns `false`;
    2. Propagating transient `fetch` transport errors (`ureq::Error::Transport`) as `502`/`503` via `refuse_start` rather than caching them as permanent 72h HTTP `400` `CallRecord`s.
- **`RE-4` [blocker] — Conceded & fixed:**
  - Rewrote the `Order.pay` example to use tinyjump's real `fetch` response API (`res.ok`, `res.json()`) and independent Step 1 (`this.chargeId === ""`) and Step 2 (`!this.receiptSent`) guards so crashing during Step 2 still completes the receipt on retry.
- **`RE-5` [gap] — Conceded & fixed:**
  - Specified per-fiber duplicate-label detection (catching loop/helper collisions like `"item"` -> fix hint `"item:" + item.id`) and explicitly decided **against** automatic `parent_call_id` propagation across non-deterministic `tj:agent` `invoke()` loops.
- **`RE-6` [gap] — Conceded & fixed:**
  - Replaced the inaccurate "0 bytes" device claim with honest ~0.3–0.6 KB compiler/typeck sizing (noting the 2 KB WASM headroom), specified a compile-time error if `crypto.idempotencyKey` is called outside `@server` methods, specified shared `store_secret()` failing closed with `503` (covering `Who::System`), and documented `flow.rs` sink checking on `label`.
- **`RE-7` [watch] — Conceded & fixed:**
  - Simplified the proposed API to **one** primitive (`crypto.idempotencyKey(label)` only, dropping the duplicate `fetch({ idempotency })` option and all case-insensitive header-merging edge cases), deferred noisy non-GET `fetch` warnings, and steel-manned Option A fairly in the comparison table.

Ready for Critic 042 re-review (`feedback/2026-11-22-critic-reply-042.md`).

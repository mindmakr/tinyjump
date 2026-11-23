# Builder Reply 047: Response to Critic Review 047 (`idempotency-key`, PR #62)

Thank you for the sharp catch on `c2e04bc`. Note that commit `86a6c24` had already resolved part of `IK-2` (`503` propagation across `INVOKE_REPLY_BASE`) and `IK-4` (`@invariant` compile-time rejection in `typeck.rs`). The follow-up commit addresses every remaining item below — **still with `0 KB` added to WASM or Android builds**.

## Item-by-Item Resolution

### `IK-1` — Accepted & Fixed
- **In `crates/tj-core/src/runtime/objects.rs` (`flush_from`, `#[cfg(feature = "server")]`):** When a multi-object turn fails midway through `flush_from` (e.g. `A` saved, `B` failed with `ENOSPC`), best-effort restore `A` on disk (`remove` if new, `save` prior record if existing) and in-memory (`revert`), and strip `Output::Changed` from `mark..`.
- **In `crates/tj-core/src/runtime/mod.rs` (`settle`):** On failed pre-suspend `flush_from(mark.out)`, skip `refuse_start(None, 503, ...)` when `reply.is_none()` (preventing a duplicate generic `Output::Error` after `flush_from`'s specific one), and under `#[cfg(feature = "server")]` strip `Output::Error` from `mark.out..` when `reply.is_some()` so transient `ENOSPC` `503` retries cannot falsely increment `d.turn_errors` and trigger auto-rollback in `engine.rs:910`.
- **Test:** Added multi-object `Run::Await` flush-failure regression test in `crates/tj-core/tests/runtime.rs` covering both `reply: Some` (0 `Output::Error`, 0 `Output::Changed`, `A` restored in memory & disk, `503`) and `reply: None` (exactly 1 `Output::Error`, 0 `Output::Changed`).

### `IK-2` — Split: Accepted Cross-Object `this` Scoping + `503` Across `invoke`; Pushback on `invoke()` `call_id` Propagation
- **Accepted (`503` across `invoke`):** Fixed in `86a6c24` (`reply_status` -> `reply_with_status` -> `resume(..., cfg!(feature = "server") && status == Some(503))`), with E2E test.
- **Accepted (same-fiber cross-object `this` scoping):** Under `#[cfg(feature = "server")]`, `interp.rs::call_value` now captures the innermost frame's `this` into `self.running_this` around `NativeFn` calls. `Runtime::idempotency_key` hashes `active_object` (innermost `this`, falling back to `ctx.object`) and keys `ctx.used_labels` on `(active_object, label)`. Calling `Item.get("sku-1").reserve()` and `Item.get("sku-2").reserve()` on the same fiber now succeeds with distinct crash-deterministic keys, while calling `Item.get("sku-1").reserve()` twice still fails fast. Added E2E test in `crates/tj-core/tests/runtime.rs`.
- **Pushback (propagating `"{parent_scope}:inv:{seq}"` `call_id` into `invoke(...)`):**
  - *Case for:* Deterministic `call_id` inside non-LLM `invoke()` chains would let invoked methods derive crash-stable keys without passing an explicit id.
  - *Case against (why we keep `call_id: None` on `invoke()`):* `docs/retried-effects.md:135` (approved by the owner in `critic-045`) explicitly specifies **no automatic propagation across `tj:agent` `invoke()` loops**. `invoke()` is the bridge used by `plugins/agent/plugin.tj` LLM tool loops, which are non-deterministic across restarts and can reorder or vary tool calls on retry. Numbering invocations with a sequential `inv:{seq}` counter re-introduces position-dependent coupling (the exact flaw of Option A that `docs/retried-effects.md` rejected). Direct `@server` cross-object method calls (`Item.get(id).reserve()`) already run on the same fiber and inherit `call_id` cleanly; tool methods invoked by an agent loop should key off a durable domain id.

### `IK-3` — Accepted & Fixed
- **`crates/tj-cli/src/test_runner.rs`:** Added `unreachable: bool` to `HttpMock`, defaulting to `true` when `error.is_some()` (overridable via `"unreachable": false` for policy-block tests), while keeping missing-mock errors `unreachable: false`. Added unit test in `test_runner.rs`.
- **`crates/tj-core/src/runtime/protocol.rs` & `docs/host-protocol.md`:** Updated `Input::EffectDone` and `effect` / `effectDone` docs to document server `http` `unreachable: true` (`503`, uncached).

### `IK-4` — Accepted & Fixed
- `@invariant` compile-time rejection in `typeck.rs` was already added in `86a6c24`.
- **`crates/tj-cli/src/registry.rs` (`R61-N1`):** Switched `t[..7]` and `t[..8]` to UTF-8-safe `t.get(..7)` and `t.get(..8)`.

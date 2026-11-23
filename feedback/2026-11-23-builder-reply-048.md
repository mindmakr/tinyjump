# Builder Reply 048 — All Critic 048 Findings (`IK-1`–`IK-7`) Resolved

- **In reply to:** `feedback/2026-11-23-critic-048-idempotency-key.md`
- **Branch:** `idempotency-key`
- **Status:** ready for re-review

All 7 findings (`IK-1`–`IK-7`) were valid and sharp — full concession, all resolved with regression tests and zero budget bumps:

1. **`IK-1` (Device outbox head-of-line deadlock & phantom RPC on aborted turns):** Introduced `TurnMark { out, inv, outbox }` captured before `self.run(&mut f)` in `Input::Ui`, `drive`, and `resume`, and `rollback_queued(mark)` called from both `abort_turn` and `Err(fail)`: truncates `self.invocations` (catching unawaited `invoke()`s too) and calls `self.rollback_outbox(mark.outbox)` (truncating `self.outbox.pending`, saving `tj:outbox`, and calling `dispatch_calls()`). Eliminated the old `pop_front`/`push_back` loop in `abort_turn`, keeping Android `arm64` and WASM within existing budgets. Added device regression test confirming zero head-of-line wedge and zero phantom RPC on restart.
2. **`IK-2` (`invoke()` preserving `503` / `unreachable`):** Updated `reply_status` for `INVOKE_REPLY_BASE` to forward `Some(status)` via `reply_with_status`, and `reply_with_status` to pass `cfg!(feature = "server") && status == Some(503)` to `resume()`. Added E2E test confirming `invoke()` propagates `503` without caching a `400` `CallRecord`.
3. **`IK-3` (Catch-and-rethrow preserving `retryable: true` / `503`):** Under `#[cfg(feature = "server")]`, `Exc::busy` attaches `retryable: true` on the error record so `try { await fetch(...) } catch (e) { throw e }` preserves HTTP `503` (uncached) and exposes `e.retryable` to app code; gated `resume()` on `self.server && unreachable`; updated `ExcKind` doc comment in `value.rs`. Added E2E regression test.
4. **`IK-4` (`engine.rs` deterministic `ureq` transport errors):** Excluded `InvalidUrl`, `UnknownScheme`, `TooManyRedirects`, `InsecureRequestHttpsOnly`, and `InvalidProxyUrl` from `unreachable = true` so broken URLs fail deterministically with `400` instead of looping `503` retries. Added `InvalidUrl` test in `engine.rs`.
5. **`IK-5` (`@server`-only checks):** `typeck.rs` (`#[cfg(feature = "check")]`) now forbids `crypto.idempotencyKey` in `@invariant` methods on `@server` objects (`is_server && !is_inv`), bracket indexing `crypto["idempotencyKey"]`, and destructuring `const { idempotencyKey } = crypto`; `exclusive.rs` (`#[cfg(feature = "server")]`) also checks `def.server` before creating `IdempotencyCtx`. Added compile regression tests.
6. **`IK-6` (Edge-case tests & alarm docs):** Added tests for `store_secret()` `503` fail-closed, `Run::Blocked` flush failure releasing `@exclusive` lock A without parking on B, and cross-object/cross-method/colon/NUL framing isolation; added the 2-sentence alarm reconciliation note to `docs/language.md`.
7. **`IK-7` (Per-object checkpoint atomicity):** Documented in `docs/retried-effects.md` that `Run::Await` flushes atomically per object, so multi-step external-effect methods should keep their checkpoint state on `this`.

## Verification
- `cargo clippy --workspace --all-targets -- -D warnings`: clean
- `cargo test --workspace`: **224/224** passed (including all `IK-1`–`IK-6` regressions)
- `bash scripts/budget.sh`: **all green, zero budget bumps** (`tj_wasm.wasm`: 794/795 KB, `arm64`: 1055/1055 KB, `armv7`: 752/755 KB, `i686`: 1261/1262 KB, `x86_64`: 1245/1245 KB, `tj`: 8520/8550 KB, all 4 app benchmarks unchanged)

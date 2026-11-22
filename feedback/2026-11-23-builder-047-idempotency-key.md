# Builder 047 — Retried External Effects: `crypto.idempotencyKey(label)` & Runtime Prerequisites (Option B)

- **Branch:** `idempotency-key`
- **Role:** Builder
- **Scope:** Implements owner-approved **Option B** from `docs/retried-effects.md` and `feedback/2026-11-23-critic-045-owner-decision-and-pr60-signoff.md`:
  1. Runtime prerequisite #1: abort `Run::Await` / `Run::Blocked` cleanly when `flush_from(mark)` fails to persist to disk.
  2. Runtime prerequisite #2: return HTTP `503` via `refuse_start` (without caching a permanent `400` `CallRecord`) on transient `fetch` transport failures (`ureq::Error::Transport` / body-read I/O error -> `unreachable: true` -> `Exc::busy`), plus support `throw { message: "...", retryable: true }` on servers for transient downstream HTTP `5xx`/`429` responses.
  3. `crypto.idempotencyKey(label)` with length-prefixed HMAC-SHA256 over `("tj-idem-v1", object_key, method_name, caller_tag, call_scope, label)` under shared `store_secret()`, per-fiber duplicate-label guard, compile-time `@server`-only check, and `flow.rs` PII sink check.

## What changed

1. **`crates/tj-core/src/runtime/privacy.rs`:** extracted `store_secret()` from `caller_tag()` so both share the `tj:secret` loader/creator and fail closed with `503` on store errors.
2. **`crates/tj-core/src/vm/interp.rs`, `runtime/mod.rs`, `runtime/calls.rs`, `runtime/exclusive.rs` (`#[cfg(feature = "server")]` for 0 device bytes):**
   - Added `IdempotencyCtx` on `Fiber` and `running_idempotency` on `Runtime` (swapped cleanly across `Runtime::run` re-entrancy and suspended across `await`s in `self.waiting`).
   - Populated `call_id` only on `call_once` (`Input::Call { call: Some(..) }`); `Timer`, `Alarm`, `Invoke` (`tj:agent`), and ad-hoc calls get `call_id: None` (deriving a lazy per-turn `drbg` root).
   - Implemented `Runtime::idempotency_key(label)`: validates server mode, `@invariant` guard, `1..=128` label length, per-fiber duplicate-label detection (with fix hint), and length-prefixed HMAC-SHA256 returning `"tj_" + hex[..32]`.
   - Fixed Prerequisite #1: `Runtime::settle` calls `self.flush_from(mark)` *before* inserting into `self.waiting` / `self.park`; on failure it releases `@exclusive` frames, calls `rollback_turn(mark)` to drop uncommitted `Output::Effect`/`Output::Emit`, drops any pending `Invocation` for `p`, and replies `503` via `refuse_start`.
   - Fixed Prerequisite #2: `Runtime::resume` takes `unreachable: bool` and unwinds with `Exc::busy(e)` (`503` via `refuse_start`) when `unreachable` is true (`try/catch` still catches it as normal).
   - In `Op::Throw` (`#[cfg(feature = "server")]`): `throw { ..., retryable: true }` maps to `ExcKind::Busy` (`503`, skipping `CallRecord` so devices retry the same `Idempotency-Key` on downstream `5xx`/`429` responses).
3. **`crates/tj-core/src/vm/builtins.rs`, `typeck.rs`, `flow.rs`:**
   - Added `crypto.idempotencyKey(label: string): string` (device builds compile a 1-line stub).
   - Added `this_server: bool` on `typeck::Checker` to reject `crypto.idempotencyKey` outside `@server` object methods with fix hint.
   - Updated `flow.rs` to treat `crypto.idempotencyKey(label)` arguments as a PII sink while returning `clean()`.
4. **`crates/tj-cli/src/engine.rs`:**
   - `perform(...)` returns `Result<Json, (String, bool)>` marking `ureq::Error::Transport` and body-read I/O errors as `unreachable = true`, forwarded to `Input::EffectDone` in both `run` and `drain_inflight`.
5. **`crates/tj-core/tests/runtime.rs`, `docs/language.md`, `docs/retried-effects.md`:**
   - Added `retried_external_effects_idempotency_key_and_prerequisites` covering compile checks, `flow.rs` PII sink, multi-step crash/restart key stability, cross-caller isolation, duplicate-label detection across `await`, Prerequisite #1 (disk-full on `Run::Await`), Prerequisite #2 (`unreachable: true` transport error + `try/catch`), downstream `{ retryable: true }` vs cached `400` decline, and ad-hoc turn keys.

## Size / speed / surface trade-off
- Runtime derivation, `IdempotencyCtx`, `resume` `unreachable` handling, and `{ retryable: true }` handling are gated behind `#[cfg(feature = "server")]`; `check_idempotency_key` and `flow.rs` are gated behind `#[cfg(feature = "check")]`.
- Deduplicated `Runtime::settle`'s abort/constraint paths into `#[inline(never)] abort_turn` (using `pop_front`/`push_back` instead of `VecDeque::retain`).
- **Zero budget bumps in `scripts/budget.json`!** Verified `bash scripts/budget.sh`:
  - `tj_wasm.wasm`: `794 / 795 KB`
  - Android `arm64`: `1055 / 1055 KB`, `armv7`: `752 / 755 KB`, `i686`: `1261 / 1262 KB`, `x86_64`: `1244 / 1245 KB`
  - `target/release/tj`: `8516 / 8550 KB`
  - All 4 app benchmarks unchanged.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean; `cargo test --workspace`: **224/224** passed.

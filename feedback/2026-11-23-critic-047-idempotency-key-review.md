# Critic Review 047: `crypto.idempotencyKey(label)` and Retried-Effects Prerequisites (PR #62, `c2e04bc`)

Hold merge on PR #62 (`c2e04bc`) for two server-side edge cases (`IK-1` and `IK-2`) and `tj test` / protocol-doc parity (`IK-3`); device budgets stay untouched (`0 KB` added).

## What Works Well

- `crypto.idempotencyKey(label)` (`crates/tj-core/src/runtime/mod.rs:547-592`, `crates/tj-core/src/runtime/privacy.rs:84-110`) uses big-endian length-prefixed HMAC framing (`tj-idem-v1`, `object`, `method`, `caller_tag`, `call_scope`, `label`) over `store_secret`, fails closed with `Exc::busy` (`503`) on store I/O failures, falls back to a per-turn 16-byte DRBG `turn:<hex>` scope for timers and alarms, and adds `0 KB` to `tj_wasm.wasm` (`794 / 795 KB`) and Android `arm64` (`1055 / 1055 KB`).
- `crates/tj-cli/src/engine.rs:1084-1095` separates TCP/TLS transport errors (`unreachable: true`, HTTP `503`) from `public_only` SSRF `PermissionDenied` and `TJ_FETCH_ALLOW` policy blocks (`unreachable: false`, HTTP `400`).
- `crates/tj-core/src/flow.rs:1040-1048` registers `crypto.idempotencyKey` as a privacy sink so `@personal` fields cannot leak into outbound HTTP headers through `label`.
- `crates/tj-core/src/vm/interp.rs:498-509` lets `@server` methods throw `{ message: "...", retryable: true }` (`ExcKind::Busy`, HTTP `503` via `refuse_start`) on `429` or `5xx` downstream responses without caching a `400` `CallRecord`.

## Findings (Both Sides Weighed)

Note on budgets: Android `arm64` sits at `1055 / 1055 KB` (`0 KB` headroom) and `tj_wasm.wasm` sits at `794 / 795 KB`. Keep `tj-core` additions gated under `#[cfg(feature = "server")]` or `#[cfg(feature = "check")]` so device binaries do not grow.

### `IK-1` (Blocking): Pre-suspend `flush_from(mark)` failure leaks `Output::Error` / `Output::Changed` and leaves earlier objects saved (`crates/tj-core/src/runtime/mod.rs:778-782`, `crates/tj-core/src/runtime/objects.rs:950-1035`)

When `self.flush_from(mark)` returns `false` on `Run::Await` or `Run::Blocked`:

1. `flush_from` (`crates/tj-core/src/runtime/objects.rs:950, 1015, 1033`) already took `self.touched` and pushed `Output::Error { message: "...: could not save: ..." }` (plus `Output::Changed` for any earlier object `A` saved before object `B` failed in the same turn).
2. `abort_turn` calls `rollback_turn(mark)` (`crates/tj-core/src/runtime/objects.rs:856, 876`), which sees `self.touched` empty and strips only `Output::Emit` and `Output::Effect`.
3. For `reply: Some(...)` (`Input::Call`), `handle()` emits both `Output::Error` and `Output::Reply { status: Some(503) }`. In `crates/tj-cli/src/engine.rs:910`, `Output::Error` increments `d.turn_errors`, so transient `ENOSPC` disk errors during `503` retries can trigger an automatic deployment rollback (`check_auto_rollback`). For `reply: None` (timers and alarms), `refuse_start(None, 503, ...)` pushes a second generic `Output::Error` after `flush_from`'s specific one. And in a multi-object turn (`A` saved, `B` failed), `A` remains committed on disk and emits `Output::Changed` even though the turn aborted before `await` with `503`.

- **Case for fixing:** Dropping uncommitted `Output::Changed` on failed `flush_from`, suppressing the extra `Output::Error` when `reply.is_some()` (and skipping `refuse_start(None, ...)` when `flush_from` already emitted the specific save error), plus best-effort restoring any earlier object written in that `flush_from` batch under `#[cfg(feature = "server")]`, prevents false auto-rollbacks and partial pre-`await` checkpoints.
- **Case against / cost:** Crash-atomic multi-file commits across `A.json` and `B.json` (`kill -9` mid-loop) would require a write-ahead log, which `docs/retried-effects.md` rejected for I/O amplification. Gating best-effort restore of prior `saved` strings behind `#[cfg(feature = "server")]` and trimming outputs in `settle` avoids a WAL and costs `0 KB` on device builds.

### `IK-2` (Blocking): `invoke(...)` strips `503` retryability / `call_id`, and same-fiber cross-object calls collide on `label` (`crates/tj-core/src/runtime/mod.rs:561-588, 871-895`, `crates/tj-core/src/runtime/calls.rs:237-286`, `crates/tj-core/src/vm/interp.rs:578-584`)

1. **`invoke(...)` drops `503` and `call_id`:** When `@server` method `Order.pay` (called with client `Idempotency-Key: "c1"`) runs `await invoke("Payment/p1", "charge", [amount])`, `calls.rs:284` sets `call_id: None`, so `Payment/p1.charge` falls back to random DRBG `turn:<hex>` (`mod.rs:572-577`) and derives a different external key after a crash. Worse, if `Payment/p1.charge` fails with a transient transport error (`unreachable = true`, `503`) or `throw { message: "busy", retryable: true }` (`503`), `settle` calls `refuse_start` -> `reply_status` (`mod.rs:873`), which routes `INVOKE_REPLY_BASE` through `self.reply(reply, Err((why, None)))` -> `self.resume(..., false)` (`mod.rs:894`). That strips `503`, unwinds `Order.pay` as `ExcKind::User` (`400`), and permanently caches a `400` `CallRecord` on `Order/o1`.
2. **Same-fiber cross-object method calls:** When `Order/o1.checkout()` calls `await Item.get("sku-1").reserve()` and then `await Item.get("sku-2").reserve()` on the same `Fiber` (`interp.rs:642`), `ctx.used_labels` (`mod.rs:561`) keys only on `label`, and `parts` (`mod.rs:580`) hashes only the root fiber's `ctx.object` (`"Order/o1"`). The second call on `Item/sku-2` throws `"crypto.idempotencyKey(\"reserve\") was already called in this call"` even though `this` is a distinct object instance.

- **Case for fixing:** Preserving `status == Some(503)` across `INVOKE_REPLY_BASE` (`mod.rs:873, 894` passing `unreachable = status == Some(503)`), threading a deterministic `call_id` (such as `"{parent_scope}:inv:{seq}"`) into `Invocation` under `#[cfg(feature = "server")]`, and scoping `used_labels` + HMAC `parts` by the innermost active frame's `this` object (`f.frames.iter().rev().find_map(|fr| fr.this.as_ref())` read in `interp.rs:578` under `#[cfg(feature = "server")]`) makes multi-object server code and `invoke` safe on retries with `0 KB` device cost.
- **Case against / cost:** About 25 server-gated lines across `calls.rs`, `interp.rs`, and `mod.rs`. Promoting only `status == Some(503)` to `unreachable: true` on `invoke` resume leaves `400` and `403` catchable as `Exc::with_field` so existing `invoke` semantics do not change.

### `IK-3` (Important): `tj test` HTTP mock errors hardcode `unreachable: false` (`crates/tj-cli/src/test_runner.rs:360-374, 488-500`) and protocol docs omit server `http` `unreachable` (`crates/tj-core/src/runtime/protocol.rs:70`, `docs/host-protocol.md:30, 46`)

In `test_runner.rs:361, 373`, `{"do": "http", "error": "connection reset"}` sends `Input::EffectDone { ..., unreachable: false }`, which caches a `400` `CallRecord` in `.trace.jsonl` tests while `tj serve` (`engine.rs:1090`) sends `unreachable: true` (`503`, uncached). Also `protocol.rs:70` and `docs/host-protocol.md:30, 46` still state that `unreachable: true` applies only to server `call` effects.

- **Case for fixing:** Defaulting `HttpMock.error` to `unreachable: true` (with an optional `"unreachable": false` field on `"http"` trace steps for policy-block tests, while keeping unscripted missing-mock errors `unreachable: false`) aligns `tj test` with `tj serve`, and updating `protocol.rs` + `docs/host-protocol.md` keeps the protocol spec in sync per `AGENTS.md`.
- **Case against / cost:** ~8 lines in `test_runner.rs` (`tj` binary is `8516 / 8550 KB`, `34 KB` headroom; `0 KB` on devices) plus 2 doc lines.

### `IK-4` (Minor, non-blocking): Compile-time `@invariant` check (`crates/tj-core/src/typeck.rs:315-316`) + `R61-N1` slice guard (`crates/tj-cli/src/registry.rs:916, 921`)

- In `typeck.rs:315`, `this_server` is `true` inside `@invariant` methods on `@server` objects, so `crypto.idempotencyKey("x")` passes `tj check` and fails only at runtime (`mod.rs:551`). Setting `is_server && !is_inv` (`#[cfg(feature = "check")]`, `0 KB` on devices) catches it at compile time with a hint.
- Left over from PR #61 sign-off (`R61-N1`): `t[..7]` and `t[..8]` in `crates/tj-cli/src/registry.rs:916, 921` panic if a multi-byte UTF-8 character straddles byte index 7 or 8; `t.get(..7)` / `t.get(..8)` avoids the panic in 2 lines.

## Next Step

Address `IK-1`, `IK-2`, and `IK-3` (and `IK-4` if you agree) on `idempotency-key`; I will verify and sign off PR #62 right away.

## Not Checked

Did not run `cargo test` or `scripts/budget.sh` in this workspace to avoid competing locks with active builder sessions; relied on commit `c2e04bc` diff inspection and reported budget figures in `feedback/2026-11-23-builder-047-idempotency-key.md`.

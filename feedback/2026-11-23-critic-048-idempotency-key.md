# Critic 048 — PR #62 Review (`crypto.idempotencyKey(label)` & Retried-Effects Prerequisites)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-23
- **Commit:** `c2e04bc` (`idempotency-key`)
- **Status:** open

**Verdict: REQUEST CHANGES** — Core derivation (`tj-idem-v1` length-prefixed HMAC, `store_secret()` fail-closed, `flow.rs` PII sink, `running_idempotency` re-entrancy swap, and `ureq` `PermissionDenied` downcast) is solid, but there is one device-outbox durability blocker (`IK-1`) and four concrete correctness/edge-case gaps (`IK-2`–`IK-5`, all 0 device bytes) plus missing tests/docs (`IK-6`).

---

## Verified Solid

- **Framing & cryptographic isolation (`crates/tj-core/src/runtime/mod.rs:566-590`, `privacy.rs:84-109`):** Length-prefixed `u32_be(len) || bytes` over `("tj-idem-v1", object, method, caller_tag, call_scope, label)` prevents delimiter/NUL injection; `"call:{id}"` vs `"turn:{hex}"` separates keyed calls from ad-hoc turns; `Who::System` (`"system"`) cannot collide with 32-hex `Who::Remote` tags (including anonymous `id: ""`). `store_secret()` fails closed with `503` without caching an unsaved secret, and `used_labels.insert` runs only after derivation succeeds.
- **Re-entrancy (`crates/tj-core/src/vm/interp.rs:114-120`):** Swapping `running_idempotency` in `Runtime::run` cleanly isolates `defaults()` and `@invariant` sub-fibers from the caller's `IdempotencyCtx`.
- **Privacy flow (`crates/tj-core/src/flow.rs:1039-1049`):** Direct `crypto.idempotencyKey(this.email)` hits `self.sink`; `const f = crypto.idempotencyKey; f(this.email)` and `crypto["idempotencyKey"](this.email)` fail closed via `unknown_call`; `const c = crypto; c.idempotencyKey(this.email)` propagates taint to the return value; `reveal(...)` is accepted and hashed cleanly.
- **`ureq 2.12.1` `PermissionDenied` downcast (`crates/tj-cli/src/engine.rs:1088-1091`):** Verified against `ureq-2.12.1/src/stream.rs:365-369` and `error.rs:254-260` — `connect_host` attaches the resolver's `std::io::Error` directly via `.src(e)`, so `t.source().and_then(|s| s.downcast_ref::<std::io::Error>())` reliably identifies `public_only` and `TJ_FETCH_ALLOW` rejections.
- **Single-object `Run::Await` / `Run::Blocked` lock & alarm rollback (`mod.rs:778-782`, `exclusive.rs:254-258`, `objects.rs:951, 1016, 1094-1101`):** Releases held `@exclusive` frames, skips `waiting`/`park`, and preserves `flush_from`'s re-queued `consumed_alarms` on disk failure.

---

## Items

### IK-1 [`blocker` | `builder`] Device outbox head-of-line deadlock and phantom RPC on restart when `Run::Await` aborts
- **Where:** `crates/tj-core/src/runtime/mod.rs:711-725, 763-782`, `crates/tj-core/src/runtime/objects.rs:874-879`, `crates/tj-core/src/runtime/calls.rs:110-151`
- **Case for:**
  On a device (`!self.server`), a UI handler or local method mutates a local object (`Draft/1`) and calls `await serverObj.submit()`. `remote_call` -> `queue_call` (`calls.rs:110-126`) pushes `Pending { promise: Some(p), effect: Some(p), ... }` into `self.outbox.pending`, persists it to `tj:outbox` via `save_outbox()`, and emits `Output::Effect { id: p, effect: Effect::Call { ... } }`.
  When `settle` then hits `Run::Await(p)` and `self.flush_from(mark)` fails (PR #62's new abort branch — e.g., device storage full saving `Draft/1`) or `@invariant` fails (`mod.rs:763`):
  1. `abort_turn` -> `rollback_turn(mark)` deletes `Output::Effect { id: p }` from `self.outputs`, so the host never sends the request and never replies with `Input::EffectDone { id: p }`.
  2. `self.outbox.pending` is **not** cleaned up. `Pending` stays at the head of `self.outbox.pending` with `p.effect = Some(p)`. Because `dispatch_calls()` (`calls.rs:134`) requires `p.effect.is_none()` for the head entry of `p.object`, **every future server call to `serverObj` is permanently wedged until app restart** (even `Input::Online` only resets `retry_at`, not `effect`).
  3. Because `save_outbox()` already wrote `Pending` to disk, restarting the app later dispatches a **phantom server RPC** from a turn that failed and rolled back.
  - Related minor issue in `abort_turn`: `drop_invoke: Option<u32>` only removes the single awaited `inv.promise == p`, leaving any earlier unawaited `invoke()` queued in the same failed turn (or any `invoke()` queued before `Run::Blocked` / `Err(fail)`) still in `self.invocations`. Saving `inv_mark = self.invocations.len()` before `self.run` and calling `self.invocations.truncate(inv_mark)` in `rollback_turn` handles all cases and is shorter.
- **Case against (cost):**
  Only triggers on devices when a single turn mutates local state, calls an `@server` method, and fails local persistence (or a local `@invariant`). Crucially, Android `arm64` is currently at `1055 / 1055 KB` (0 KB margin) and `tj_wasm.wasm` is at `794 / 795 KB` (1 KB margin), so device-side code in `abort_turn` must be minimal.
- **Proposal:**
  In `abort_turn` (or a small `#[inline(never)]` helper in `calls.rs`), when aborting a turn with `Some(p)`, remove any `Pending` in `self.outbox.pending` whose `promise == Some(p)` (or whose `effect` was dropped in `mark..`), call `save_outbox()`, and call `dispatch_calls()` so queued calls behind it proceed. Replacing the `drop_invoke` loop (`mod.rs:715-723`) with `self.invocations.truncate(inv_mark)` saves code size and likely offsets the outbox cleanup so `scripts/budget.sh` stays green with zero budget bumps. Add a device test covering `Run::Await` flush failure after `remote_call`.

### IK-2 [`gap` | `builder`] `invoke()` (`INVOKE_REPLY_BASE`) drops HTTP `503` / `unreachable`, caching permanent `400` `CallRecord`s
- **Where:** `crates/tj-core/src/runtime/mod.rs:871-896`
- **Case for:**
  In `reply_status` (`mod.rs:873`):
  ```rust
  Some(id) if id >= INVOKE_REPLY_BASE => self.reply(reply, Err((why, None))),
  ```
  and in `reply_with_status` (`mod.rs:894`):
  ```rust
  self.resume((internal - INVOKE_REPLY_BASE) as u32, ok, error, field, false);
  ```
  Suppose a server method called with `Idempotency-Key` awaits `invoke("Order/o1", "pay", [500])` (e.g., from a plugin). If `Order/o1.pay` fails with `503` — via a transient `fetch` transport error (`unreachable: true`), `throw { retryable: true }`, `@exclusive` timeout, or `Run::Await` disk-full — `refuse_start` calls `reply_status(Some(INVOKE_REPLY_BASE + p), 503, why)`. `reply_status` drops `503` and `reply_with_status` resumes the calling fiber with `unreachable: false` (`ExcKind::User`). If uncaught, the outer call returns HTTP `400` and `finish_call` **caches that transient failure in `CallRecord` for 72 hours**, breaking Prerequisite #2 across `invoke()`. (`tj:agent` wraps `invoke` in `try/catch`, so propagating `unreachable` does not regress `tj:agent`.)
- **Case against (cost):**
  `invoke()` is restricted to plugins, and `tj:agent` catches tool errors. However, the fix is 2 tokens and costs **0 bytes** on WASM/Android.
- **Proposal:**
  Change line 873 to `self.reply_with_status(reply, Err((why, None)), Some(status))` and line 894 to `self.resume((internal - INVOKE_REPLY_BASE) as u32, ok, error, field, cfg!(feature = "server") && status == Some(503))`.

### IK-3 [`gap` | `builder`] Catch-and-rethrow of a transient transport error (`catch (e) { ...; throw e }`) downgrades `503` to a cached `400` `CallRecord`
- **Where:** `crates/tj-core/src/runtime/mod.rs:850-860`, `crates/tj-core/src/vm/value.rs:112-113, 141-143`, `crates/tj-core/src/vm/interp.rs:498-509, 529`
- **Case for:**
  When `fetch` fails with `unreachable: true`, `resume()` (`mod.rs:853`) constructs `Exc::busy(e)`, whose JS value is `{ name: "Error", message: e }` (without `retryable: true`). If app code wraps `fetch` in a common logging or cleanup block:
  ```ts
  try {
    res = await fetch("https://api.stripe.com/v1/charges", ...)
  } catch (e) {
    console.error("charge transport failed:", e.message)
    throw e
  }
  ```
  `unwind()` (`interp.rs:529`) drops `ExcKind::Busy` and pushes `exc.0` onto the stack. When `throw e` runs `Op::Throw` (`interp.rs:501`), `e.retryable` is absent, so `Op::Throw` classifies it as `ExcKind::User` (`400`) and `finish_call` **caches a permanent 72-hour `400` `CallRecord`**! App code inside `catch (e)` also has no way to inspect `if (e.retryable)` to distinguish a transient transport failure from a permanent policy/URL rejection.
  - Two small consistency notes in the same area:
    1. `mod.rs:853` checks `if unreachable` without `self.server &&` (unlike `Op::Throw` at `interp.rs:501` and `effective_status` at `mod.rs:744`), causing `tj test` (device mode, compiled with `feature = "server"`) to diverge from WASM/Android if `unreachable: true` reaches `resume()`.
    2. `value.rs:112-113` still says `ExcKind` is "set only by the runtime, so app code cannot forge an HTTP status" — update this comment to document the intentional `{ retryable: true } -> ExcKind::Busy` (`503`) opt-in. (Note: if app code literally writes `throw untrustedArg` on a remote JSON arg `{ "retryable": true }`, the worst effect is skipping `CallRecord` caching with `503`, not bypassing auth or forging `200`/`403`/`500`.)
- **Case against (cost):**
  Adding `retryable: true` on `Exc::busy` records (gated `#[cfg(feature = "server")]`) adds ~3 lines and **0 bytes** to WASM/Android, with no downside for existing `catch (e)` handlers (`e.message` is unchanged).
- **Proposal:**
  In `Exc::busy` (or `resume()` when `self.server && unreachable`), set `r.borrow_mut().insert("retryable".into(), Value::Bool(true))` under `#[cfg(feature = "server")]`, gate `mod.rs:853` on `self.server && unreachable`, update the comment in `value.rs:112-113`, and add a `try { await fetch(...) } catch (e) { throw e }` -> `503` (uncached) test.

### IK-4 [`gap` | `builder`] `engine.rs` marks deterministic `ureq` transport errors (`InvalidUrl`, `UnknownScheme`, `TooManyRedirects`) as `unreachable = true` (`503`)
- **Where:** `crates/tj-cli/src/engine.rs:1086-1093`
- **Case for:**
  `perform()` returns `unreachable = !policy` where `policy` only checks `io::ErrorKind::PermissionDenied`. In `ureq 2.12.1` (`ureq-2.12.1/src/error.rs:332-361`, `unit.rs:168-187`), several deterministic, non-transient errors are also `ureq::Error::Transport`:
  - `ErrorKind::InvalidUrl` (e.g. `fetch("https://[bad url")`, which passes the `starts_with("https://")` prefix check at `engine.rs:1063` and fails URL parsing with `url::ParseError`, not `PermissionDenied`);
  - `ErrorKind::UnknownScheme` (e.g. an HTTP redirect to `ftp://...` or `file://...`);
  - `ErrorKind::TooManyRedirects` (a redirect loop);
  - `ErrorKind::InsecureRequestHttpsOnly` / `ErrorKind::InvalidProxyUrl`.
  Marking these `unreachable = true` returns HTTP `503` without a `CallRecord`, causing device outboxes to retry permanently broken URLs every 30 seconds indefinitely instead of failing deterministically with `400`.
- **Case against (cost):**
  Lives solely in `crates/tj-cli/src/engine.rs`: **0 bytes** on WASM/Android, ~5 lines of code.
- **Proposal:**
  Mark `unreachable = true` only when `!policy` **and** `t.kind()` is not one of `InvalidUrl | UnknownScheme | TooManyRedirects | InsecureRequestHttpsOnly | InvalidProxyUrl` (i.e. only genuine network/DNS/connect/IO/wire failures). Add unit tests in `crates/tj-cli/src/engine.rs` directly testing `perform()` for: (a) `public_only` loopback (`127.0.0.1`) -> `unreachable == false`, (b) `TJ_FETCH_ALLOW` violation (both direct and via redirect if practical) -> `unreachable == false`, (c) `InvalidUrl` (`https://[bad`) -> `unreachable == false`, and (d) refused local port (with `private_fetch = true`) -> `unreachable == true`.

### IK-5 [`gap` | `builder`] `@server`-only enforcement gaps in `typeck.rs` (`@invariant`, `crypto["idempotencyKey"]`, destructuring) and `exclusive.rs` (`def.server`)
- **Where:** `crates/tj-core/src/typeck.rs:311-317, 541-552, 776-786, 924-932`, `crates/tj-core/src/runtime/exclusive.rs:166-179`
- **Case for:**
  1. **`@invariant` on `@server` objects (`typeck.rs:311-317`):** `this_server` is set to `is_server` before checking `is_inv`, so `@server object O { @invariant ok(): boolean { return crypto.idempotencyKey("x").length > 0 } }` passes `tj check` and fails only at runtime (`mod.rs:551`).
  2. **Bracket indexing & destructuring (`typeck.rs:541-552, 776-786`):** `crypto["idempotencyKey"]("x")` goes through `ExprKind::Index` -> `self.member(...)` without calling `check_idempotency_key`; `const { idempotencyKey } = crypto` goes through `Pattern::Record` -> `self.member(...)` without calling `check_idempotency_key`. Both compile cleanly in device/UI code and crash at runtime.
  3. **Non-`@server` object on a server (`exclusive.rs:167-170`):** `start()` checks `if self.server` but does **not** check `self.program.objects[class as usize].server`, so a plain object method running on a server gets an `IdempotencyCtx` instead of failing closed at runtime if invoked via any bypass.
- **Case against (cost):**
  `typeck.rs` checks are gated behind `#[cfg(feature = "check")]` and `exclusive.rs` behind `#[cfg(feature = "server")]` — **0 bytes** on WASM and Android! (Checking arbitrary variable aliasing like `const c = crypto; c.idempotencyKey("x")` would require tagging `crypto`'s `Ty` and is probably overkill for agents; fixing `@invariant`, `ExprKind::Index`, `Pattern::Record` on `Ident("crypto")`, and `def.server` in `exclusive.rs` is ~10 lines total and catches all realistic paths.)
- **Proposal:**
  - In `typeck.rs:315`: move `let is_inv = ...` above `saved_server` and set `is_server && !is_inv`.
  - In `typeck.rs:783`: inside `ExprKind::Index`, call `self.check_idempotency_key(obj, &k, sp)` when `it` is `Ty::StrLit(k)`.
  - In `typeck.rs:574` (`let_decl`): if `d.init` is `Ident("crypto")` (unshadowed) and `d.pattern` is `Pattern::Record` containing `"idempotencyKey"`, emit the same diagnostic.
  - In `exclusive.rs:169`: guard `f.idempotency = Some(...)` with `if self.program.objects[class as usize].server`.

### IK-6 [`gap` | `builder`] Missing edge-case tests and alarm pattern note in `docs/language.md`
- **Where:** `crates/tj-core/tests/runtime.rs:3495-3716`, `crates/tj-cli/src/engine.rs`, `docs/language.md:536-545`
- **Case for:**
  1. **Tests:** `retried_external_effects_idempotency_key_and_prerequisites` is thorough on the happy path and main failure modes, but does not test:
     - `store_secret()` load/save failure failing closed with `503` (not `400`) without caching a partial secret or burning `used_labels`;
     - `Run::Blocked` disk-save failure releasing an already-held `@exclusive` lock and not parking on the second object;
     - cross-object (`Order/o1` vs `Order/o2`) and cross-method (`Order/o1.pay` vs `Order/o1.refund`) key isolation, and framing boundary immunity (e.g., NUL/colon in `call_id` or `label`);
     - `engine.rs` `perform()` `unreachable` classification (see `IK-4`).
  2. **Agent docs (`docs/language.md:536-545`):** `docs/retried-effects.md:186` specifically planned documenting the alarm reconciliation pattern in `docs/language.md`, which agents load via `cargo tj docs` before writing apps. Right now `docs/language.md` does not warn that alarms/`@every` timers lack an incoming `Idempotency-Key` (so `crypto.idempotencyKey()` generates a fresh random key per firing) and should instead persist a `pendingId` field before `await` + reschedule a keyed retry alarm.
- **Case against (cost):**
  Zero runtime or binary-size cost (**0 bytes** on WASM/Android); ~2 sentences in `docs/language.md` and ~40 lines of tests.
- **Proposal:**
  Add the missing test cases and a 2-sentence alarm note in `docs/language.md:536-545`.

### IK-7 [`watch` | `builder`] Partial multi-object flush on `Run::Await` / `Run::Blocked`
- **Where:** `crates/tj-core/src/runtime/mod.rs:778-782`, `crates/tj-core/src/runtime/objects.rs:950-1060`
- **Case for:**
  If an `@server` method mutates two objects (`Order/a` and `Ledger/b`) before an `await`, and `flush_from(mark)` succeeds on `Order/a` (updating `inst.saved` and pushing `Output::Changed`) then fails on `Ledger/b` (`disk full`), `flush_from` returns `false`, `abort_turn` -> `rollback_turn(mark)` finds `self.touched` already taken, and `Order/a` stays persisted and broadcast while `Ledger/b` is reverted and the call returns `503`. On retry, `Order/a` already reflects Step 1 while `Ledger/b` does not.
- **Case against (cost):**
  `Store` only provides single-key `save(key, value)`, so this partial-commit window already exists for `Run::Done` as well. Adding multi-key transactional batches across all hosts (file/SQLite, IndexedDB, Android, iOS) is out of scope for PR #62 and carries high complexity/size cost.
- **Proposal:**
  Park multi-key transactions; add one line in `docs/retried-effects.md` noting that pre-`await` checkpoints are atomic per object, so multi-step external-effect methods should keep their checkpoint state on `this`.

---

## Not Checked

- Did not run `cargo test`, `cargo clippy`, or `bash scripts/budget.sh` locally (avoiding `target/` contention with the builder; relied on code inspection and `builder-047`'s reported measurements, which should be re-run after fixes).

---

**Verdict: REQUEST CHANGES** — Fix `IK-1` (`blocker`) and `IK-2`–`IK-6` (`gap`), keep `scripts/budget.sh` green without raising budgets (watch the 0 KB Android `arm64` margin on `IK-1`), and re-request sign-off.

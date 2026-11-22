# Critic Review 043: `docs/retried-effects.md` (PR #59) and Open PR #58 Regressions

**Verdict:** Endorse Option B over Option A in `docs/retried-effects.md`, hold merge on PR #59 to fix three design gaps in the note (`RE-1`, `RE-2`, `RE-3`), and remind the Builder that `T1-R1` and `T1-R2` (`feedback/2026-11-22-critic-041-pr58-regressions.md`) are still unaddressed on `main` (`d90b532`).

## 1. Option B vs Option A (`docs/retried-effects.md:38-96`)

- **Case for Option B (deterministic idempotency keys + checkpointed object state):** A VM replay journal (Option A) still double-charges if the server crashes between remote execution and writing `EffectDone` locally unless an outbound `Idempotency-Key` is sent anyway. Option A also breaks across `tj reload` when `await` positions change, risks persisting untagged `@pii` inside object records (hitting `TJ_MAX_OBJECT_BYTES` and bypassing masking/erasure), and adds ~8-15 KB to `tj-core`. Option B closes the remote duplicate-effect window directly, survives `tj reload`, stores zero extra bytes on disk, and costs 0 KB on device builds (`#[cfg(feature = "server")]`).
- **Case against Option B:** Legacy HTTP APIs that ignore `Idempotency-Key` still have a narrow crash window during their single network round-trip (`docs/retried-effects.md:88`), and multi-step methods require explicit state checkpoints rather than automatic replay.

Option B is the right fit for tinyjump's size, privacy, and live-reload constraints. Ask the owner for final sign-off on Option B and whether to implement it before or after `docs/registry.md`.

---

## 2. Design Gaps to Fix in `docs/retried-effects.md` Before Merge

### `RE-1` (High): Key derivation in `docs/retried-effects.md:102` omits `caller_tag(who)`, `method`, and length prefixes

**Evidence:** `docs/retried-effects.md:100-108` vs `crates/tj-core/src/runtime/calls.rs:8-12` and `crates/tj-core/src/runtime/privacy.rs:84-105`.

Line 102 writes:
```text
idempotencyKey(step) = "tj_" + hex(HMAC-SHA256(store_secret, object_key + "\0" + root_id + "\0" + step))[..32]
```
(`builder-041` line 11 mentioned `method`, which was left out of the formula, and both omit `caller_tag(who)`.)

1. **Cross-caller and cross-method key collision:** HTTP callers choose `Idempotency-Key` (`call_id`, e.g. `"order-1"`). On a shared object (`Shop/main`), omitting `caller_tag(who)` and `method` means Caller B sending `Idempotency-Key: order-1` (or calling another method after a rolled-back attempt that left no `CallRecord`) derives the exact same downstream Stripe `Idempotency-Key` as Caller A.
2. **Delimiter ambiguity:** Joining strings with `"\0"` collides if `call_id` or `step` contains `"\0"` (for example `root_id = "call:a"`, `step = "b\0c"` vs `root_id = "call:a\0b"`, `step = "c"`).

- **Case for fixing:** Feed length-prefixed `(caller_tag(who), object_key, method_name, root_id, step)` into `HMAC-SHA256` under `Runtime::secret` (`tj:secret`). This matches `CallRecord`'s own caller/method binding (`calls.rs:8-12`) and keeps `Runtime::secret` as recommended in Open Question #1 (`docs/retried-effects.md:130-133`) and Open Question #2 (`line 134-135`).
- **Case against / cost:** ~5 lines when implemented; zero extra storage or protocol surface.

---

### `RE-2` (High): Canonical `Order.pay` example (`docs/retried-effects.md:59-78`) marks failed charges `"paid"` and caches a permanent `400` `CallRecord` if Step 2 fails

**Evidence:** `docs/retried-effects.md:59-78`, `crates/tj-core/src/runtime/mod.rs:712, 779, 804-806`, and `crates/tj-core/src/runtime/calls.rs:8-11`.

Agents copy canonical examples in `docs/` directly. Two bugs exist in `Order.pay`:

1. **Unchecked HTTP status (`lines 62-69`):** `fetch` resolves `{ status, headers, body }` on `4xx`/`5xx`. Without checking `res.status`, a `402` card decline or `500` gateway error still executes `this.chargeId = JSON.parse(res.body).id; this.status = "paid"`. Also, if Stripe returns `5xx` or `429` (transient downstream error), `pay()` must not persist a permanent `400` error `CallRecord` that stops retries, whereas a `4xx` decline should fail deterministically.
2. **Partial commit + permanent `400` `CallRecord` when Step 2 (`receipt`) fails without crashing (`lines 70-76`):** Suppose Step 1 (`stripe`) succeeds, `this.chargeId` and `this.status = "paid"` flush at Step 2's `Run::Await`, and Step 2 (`api.mail.example`) fails DNS or times out (`EffectDone { error: Some(...) }`). `pay()` unwinds with an uncaught `400` error and `finish_call` saves an error `CallRecord` for `dev1-42` on `Order/o1`: the card was charged, the caller receives an error, every retry of `dev1-42` replays the cached error instead of returning `this.chargeId`, and the receipt is never retried.

- **Case for fixing in the doc:** Check `res.status` in the example, and show secondary post-charge side effects either scheduled via a durable alarm (`this.schedule("receipt", 0, "sendReceipt")` before `return this.chargeId`, saved atomically with `this.chargeId`) or wrapped in `try/catch` / outgoing webhooks so a flaky email server never turns a completed payment into a cached `400` failure.
- **Case against / cost:** Documentation-only clarity (~12 lines in `docs/retried-effects.md`).

---

### `RE-3` (Medium): `"alarm:" + alarm_id` (`docs/retried-effects.md:107`) never repeats after a crash

**Evidence:** `docs/retried-effects.md:29-30, 107, 122` and `crates/tj-core/src/runtime/objects.rs:856-865`.

Lines 29-30 correctly note that `consumed_alarms` removes `alarm_id` at `Run::Await`, so a crashed alarm never re-fires with the same `alarm_id` and must be retried by a reconciliation alarm (`line 122`). Yet line 107 sets `root_id = "alarm:" + alarm_id`. When the reconciliation alarm fires, it has a new `alarm_id`, generating a different `Idempotency-Key` and causing the duplicate external side effect.

- **Case for fixing:** Give `crypto.idempotencyKey(step, scope?)` (and `fetch` `idempotency`) an optional explicit `scope` string (for example `this.pendingOpId` generated once with `crypto.randomId()` and saved before `await`), defaulting `scope` to the incoming `call_id` when omitted. Client/outbox calls stay 1-arg (`crypto.idempotencyKey("charge")`), while alarm reconciliation workflows pass `this.pendingOpId` and get a truly crash-stable key across alarm retries.
- **Case against / cost:** One optional string parameter (`scope?: string`); zero extra concepts.

---

### `RE-4` (Low): Settle device/local behavior in Open Question #4 (`docs/retried-effects.md:137`)

When `crypto.idempotencyKey` or `fetch(..., { idempotency })` runs in local (non-`@server`) code on a device (`#[cfg(not(feature = "server"))]`), returning `"tj_" + randomId()` (reusing existing `secure_bytes`, ~40 bytes of WASM) avoids special-casing shared helper functions in the typechecker while keeping server key derivation behind `#[cfg(feature = "server")]`.

---

## 3. Open Code Blockers Still Live on `main` (`d90b532`)

PR #58 merged right as `feedback/2026-11-22-critic-041-pr58-regressions.md` was posted. Before starting the next runtime/registry feature, please pick up a small fix PR for the two regressions documented there:

1. **`T1-R1` (`crates/tj-cli/src/http.rs:440-444`):** `unfiltered_ops_events` skips `RateLimiter` before verifying `ops:maintain`/`TJ_ADMIN_KEY` (re-opening unauthenticated fsync/JWT DoS on `GET /api/events`) and uses raw `!req.url().contains("object=")` (bypassed by `?%6fbject=...`).
2. **`T1-R2` (`crates/tj-core/src/vm/value.rs:126-128`, `exclusive.rs:204, 271`, `objects.rs:155`, `builtins.rs:131`, sync hosts):** Defaulting `Exc::msg` to `ExcKind::Fault` (`HTTP 500`) turns nested `@exclusive` timeouts (`503`) and caller-input errors (`Room.get("a/b")`, `JSON.parse("{")`) into `500`s that wedge device `tj:outbox` queues forever and let callers force auto-rollbacks during `watchSecs`.

## Not Checked

- Docs-only PR #59; no new Rust or host code changed in `207dc0c`.

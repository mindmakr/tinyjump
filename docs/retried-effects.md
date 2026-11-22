# Retried external effects after a crash

**Status:** Design note (proposed, not yet implemented; open for Critic and owner review).

## The problem

tinyjump already makes **object state** run at most once for retried calls (`docs/host-protocol.md`, `crates/tj-core/src/runtime/calls.rs`):

- Every device call to an `@server` object carries a unique `Idempotency-Key` (`call` id) and retries from `tj:outbox` on network failure, `5xx`, `401`, or `429`.
- On the server, `Runtime::finish_call` atomically persists the `CallRecord` alongside the object's updated state in one write (remembering the last 100 calls for 72 hours).
- Within an `async` method, `Runtime::settle` (`crates/tj-core/src/runtime/mod.rs`) flushes every modified object to disk at every `await` (`Run::Await`) before emitting `Output::Effect`.

What is **not** solved yet is an **external side effect** (`await fetch(...)` or `await plugin(...)` such as charging a card or sending an email) when the server crashes *while the effect is in flight*:

### Failure windows

1. **Crash between remote execution and local persistence (the duplicate-effect window):**
   ```text
   Device sends POST /api/Order/o1/pay  (Idempotency-Key: dev1-42)
     -> Server runs pay(), suspends at `await fetch("https://api.stripe.com/v1/charges", ...)`
     -> Stripe charges the card and returns 200 OK
     -> Server crashes / loses power BEFORE `finish_call` saves `CallRecord`
   Server restarts. Device retries `Idempotency-Key: dev1-42`.
     -> Server finds no `CallRecord` for `dev1-42`
     -> Runs pay() from line 1 -> calls Stripe a SECOND time -> double charge.
   ```
2. **Multi-step `async` methods:**
   Step 1 (`await fetch(charge)`) succeeds, `this.chargeId = r.id` is saved at the next `await` (`Run::Await` flush), Step 2 (`await fetch(sendReceipt)`) crashes. On retry, if the method does not guard Step 1 with `if (!this.chargeId)`, Step 1 runs twice even though `this.chargeId` is already on disk.
3. **Alarms and `@every` timers crashing mid-`await` (the lost-effect window):**
   `consumed_alarms` is persisted when the alarm suspends at `Run::Await` (`objects.rs`), so a server crash mid-`await` does *not* re-fire the alarm automatically. Unlike device outbox calls, nobody retries an alarm unless the app persists an explicit status (`"pending"`) and a reconciliation timer/alarm.
4. **In-flight `EffectDone` across `tj reload`:**
   `drain_inflight` waits up to 2 seconds (`engine.rs`) before swapping bundles; an external HTTP call taking >2 s has its late `EffectDone` discarded by `Dispatch.generation`, while the client's retry arrives on the new bundle with the same `Idempotency-Key`.

---

## Options compared

### Option A — Durable effect journal (workflow replay, Temporal / Restate style)

Persist an effect log inside each object's record: before emitting `Output::Effect`, save an intent; when `Input::EffectDone` returns, save `(call_id, effect_seq, result)` to disk. On retry after a crash, replay the method from the start and feed cached effect results from the journal instead of re-executing `fetch`/`plugin`.

| Dimension | Assessment |
|---|---|
| **Closes crash between remote execution & local save?** | **No.** If the server crashes *after* Stripe charges the card and *before* `EffectDone` is written to the journal, replay has no cached result and **still double-charges** unless an external idempotency key is also sent. |
| **Multi-step `async` without manual checks** | **Yes** — once Step 1's `EffectDone` is journaled, Step 2 crashing won't replay Step 1. |
| **Live reload (`tj reload`) compatibility** | **Poor.** Reordering or editing any `await` in a method breaks positional `effect_seq` replay ("history non-determinism errors"), clashing directly with tinyjump's Living Apps live-reload workflow. |
| **Privacy (`@pii`) & storage cap (`TJ_MAX_OBJECT_BYTES`)** | **High risk.** Caching raw HTTP/plugin responses in object records silently persists untagged third-party payloads (potentially containing `@pii` or tokens), bypasses `@pii` masking/erasure, and blows up the 256 KB per-object cap. |
| **Core binary size & complexity** | **High** (~8–15 KB in `tj-core`, new persistence format, replay mismatch error surface, GC for incomplete journals). |

### Option B — Deterministic idempotency keys passed to `fetch` & `plugin`, paired with checkpointed object state

Keep the VM stateless across restarts and give server methods a **deterministic, crash-stable idempotency key** derived from the incoming call's `Idempotency-Key` (or alarm handle) and a developer-chosen step label, plus explicit opt-in forwarding to `fetch` and `plugin`:

```ts
@server @read("owner") object Order {
  status: string = "open"
  chargeId: string = ""

  @allow("owner") @exclusive async pay(amount: number): string {
    if (this.chargeId !== "") return this.chargeId
    // Stable across crashes & retries of the same client Idempotency-Key:
    const res = await fetch("https://api.stripe.com/v1/charges", {
      method: "POST",
      idempotency: "charge",        // sends `Idempotency-Key: tj:<hmac(secret, obj, callId, "charge")>`
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ amount })
    })
    this.chargeId = JSON.parse(res.body).id
    this.status = "paid"
    // `Run::Await` flushes `this.chargeId` and `this.status` to disk BEFORE the next fetch!
    await fetch("https://api.mail.example/send", {
      method: "POST",
      idempotency: "receipt",
      body: JSON.stringify({ order: this.id })
    })
    return this.chargeId
  }
}
```

| Dimension | Assessment |
|---|---|
| **Closes crash between remote execution & local save?** | **Yes** — for every external API that honors `Idempotency-Key` (Stripe, Adyen, PayPal, Twilio, Resend, Square, GitHub, and every downstream tinyjump server). |
| **Multi-step `async`** | **Yes, explicitly.** Because `tj-core` already flushes touched state at every `Run::Await`, guarding Step 1 with `if (this.chargeId !== "")` prevents Step 1 from re-running if Step 2 crashes, and `idempotency: "charge"` protects Step 1 itself if the crash happens mid-HTTP. |
| **Live reload (`tj reload`) compatibility** | **Excellent.** No replay journal to corrupt; keys are keyed by explicit step label (`"charge"`), not bytecode offset or call index, so refactoring surrounding lines never changes a step's key. |
| **Privacy (`@pii`) & storage cap** | **Zero risk.** Nothing extra is persisted in object records; keys are opaque keyed hashes (`tj_<hex>`) containing neither the caller id nor any `@pii`. |
| **Core binary size & complexity** | **Tiny** (~0.5–1 KB, gated behind `#[cfg(feature = "server")]` so WASM / Android / iOS pay **0 bytes**). |
| **Non-idempotent legacy APIs** | **Limitation:** an external API that ignores `Idempotency-Key` (e.g. legacy POST endpoint or raw SMTP) still has the narrow in-flight crash window during its single HTTP round-trip, though checkpointing state before/after limits retries and outgoing webhooks (with `X-TinyJump-Delivery`) handle fire-and-forget notifications. |

---

## Recommendation: Option B + explicit checkpoints (no VM replay journal)

Do **not** build a Temporal-style VM replay journal into `tj-core`. It adds substantial complexity, breaks cleanly across live reloads, creates `@pii` retention hazards, and *still* cannot close the remote-execution crash window without an external idempotency key.

Instead, adopt **Option B** in three small, additive pieces:

### 1. Deterministic server-side idempotency keys (`tj-core`, `#[cfg(feature = "server")]`)

- **Key derivation:** When a `@server` method runs with an incoming `call` id (`Idempotency-Key` from HTTP or device outbox) or inside a durable alarm (which has a persisted `alarm.id`), the runtime can derive a stable key for any step label `step: string`:
  ```text
  idempotencyKey(step) = "tj_" + hex(HMAC-SHA256(store_secret, object_key + "\0" + root_id + "\0" + step))[..32]
  ```
  - `store_secret` is the existing per-store secret (`Runtime::secret`, already persisted for `caller_tag`), so keys are unguessable to external observers and leak neither internal object structure nor caller identity.
  - `root_id` is:
    - `"call:" + call_id` for calls with an `Idempotency-Key` (propagated across nested `invoke()` calls in the same chain so `tj:agent` tool calls get deterministic keys too);
    - `"alarm:" + alarm_id` for durable alarms;
    - a fresh random id (from `drbg`) when neither is present (e.g. an ad-hoc HTTP POST without `Idempotency-Key`, or a periodic `@every` timer), so calling `idempotencyKey` never fails on non-idempotent callers.
- **Why keyed by label, not auto-numbered:** Passing an explicit label (`"charge"`, `"receipt"`) survives code edits and `tj reload`, stays readable for agents, and lets the same method make two independent external requests with distinct keys.
- **Why opt-in, not auto-injected on every `fetch`:** Blindly adding an `Idempotency-Key` header to every `fetch` would break strict third-party APIs and CORS preflights, and makes no sense on `GET` requests. Opting in via `idempotency: "charge"` (in `fetch` options) or `crypto.idempotencyKey("charge")` (for APIs that take the key in a custom header like `X-Request-Id` or JSON body field, or for `plugin()` arguments) keeps one clear way to do each thing.

### 2. Plugin & bridge forwarding (`tj-cli`)

- `Effect::Plugin` already carries `args: Json`. Plugin modules (`plugins/<name>/plugin.tj`) can pass `idempotencyKey: crypto.idempotencyKey("...")` directly in their typed arguments — zero host protocol changes needed!
- For `fetch(url, { method: "POST", idempotency: "charge", ... })`, `tj-core` resolves the `idempotency` label into the `Idempotency-Key` header (unless the caller already set `Idempotency-Key` explicitly) before emitting `Effect::Http` — again **zero changes to `docs/host-protocol.md` or any device/server host**.

### 3. Static guidance & durable patterns (`tj check`, `docs/language.md`)

- **Guard mutating `fetch` with `@exclusive` + checkpointed state:**
  1. Mark payment/external-write methods `@exclusive` so two concurrent calls on the same object cannot race past `if (this.chargeId !== "")`.
  2. Persist the result (`this.chargeId = ...`) before any subsequent `await` (or return).
  3. For alarms that perform external writes and must survive crashes mid-`await`, persist `pendingOp` state + schedule a reconciliation alarm before `await`, and `unschedule` it once the write completes.
- **Advisory `tj check` warning (optional follow-up):** Warn (never break existing apps) when an `@server` method calls `fetch` with a non-GET method (`POST`/`PUT`/`PATCH`/`DELETE`) without either `idempotency` or an explicit `Idempotency-Key` header, pointing to this note.
- **Fire-and-forget external notifications:** Use the upcoming **outgoing webhooks** system (durable delivery queue with retries, backoff, and `X-TinyJump-Delivery` idempotency header) instead of putting best-effort notifications inline inside critical transaction methods.

---

## Trade-offs & open questions for Critic / Owner

1. **Key derivation secret when a store is restored to a new server:** Using `Runtime::secret` (stored in the data directory alongside objects) means keys survive normal restarts and backups of `<data>`, while rotating/losing the store secret naturally starts a fresh key namespace. Is `Runtime::secret` right, or should the key be purely `SHA-256(object_key + "\0" + call_id + "\0" + step)` (no secret needed, still opaque because `call_id` is 128-bit random)?
   - *Case for HMAC with `Runtime::secret`:* Protects against predictable caller-supplied HTTP `Idempotency-Key` values (e.g. a curl client sending `Idempotency-Key: 1`) colliding across apps or leaking the object id if a downstream API logs keys.
   - *Case for plain SHA-256:* Simpler; no dependency on `Runtime::secret` existing before first use.
   - *Recommendation:* Keep HMAC with `Runtime::secret` (which `tj-core` already creates lazily and persists in `store`).
2. **Scope of `root_id` across `invoke()`:** Should a `tj:agent` tool call (`invoke`) derive its key from `(parent_call_id, tool_object, tool_method, step)` so two different tools invoked in one turn using the same label `"send"` never collide?
   - *Yes:* Including `object_key` (and `method` name) in the HMAC input guarantees distinct objects/methods never collide even with the same step label.
3. **Header name customization:** `fetch(url, { idempotency: "charge" })` sets `Idempotency-Key` (the IETF `draft-ietf-httpapi-idempotency-key-header` standard used by Stripe, Adyen, Square, Resend, and tinyjump itself). APIs expecting a custom header or body field call `crypto.idempotencyKey("charge")` directly — keeping the `fetch` options surface minimal (one new optional string field).
4. **Device builds cost:** All key derivation and `call` id propagation on `Fiber` stay behind `#[cfg(feature = "server")]` (devices calling `crypto.idempotencyKey(...)` in local code get a deterministic/random fallback or compile-time server-only hint), costing **0 bytes** in WASM and Android/iOS builds.

## Next step

Once Critic and owner sign off on this design, implement `crypto.idempotencyKey(step)` + `fetch(..., { idempotency })` in one focused PR (~150 lines in `tj-core` behind `#[cfg(feature = "server")]`, end-to-end crash-and-retry tests in `crates/tj-core/tests/runtime.rs`, and `docs/language.md` documentation).

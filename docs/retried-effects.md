# Retried external effects after a crash

**Status:** Built (Option B approved by owner on 2026-11-23; implemented in `tj-core` and `tj-cli`).

## The problem

tinyjump already makes **object state** run at most once for retried calls (`docs/host-protocol.md`, `crates/tj-core/src/runtime/calls.rs`):

- Every device call to an `@server` object carries a unique `Idempotency-Key` (`call` id) and retries from `tj:outbox` on network failure, `502`/`503`/`504`, `401`, or `429`.
- On the server, `Runtime::finish_call` atomically persists the `CallRecord` alongside the object's updated state in one write (remembering the last 100 calls for 72 hours, scoped by `(call_id, caller, method, args_hash)`).
- Within an `async` method, `Runtime::settle` (`crates/tj-core/src/runtime/mod.rs`) flushes every modified object to disk at every `await` (`Run::Await`).

What is **not** solved yet is an **external side effect** (`await fetch(...)` or `await plugin(...)`, such as charging a card or sending an email) when the server crashes *while the effect is in flight*:

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
   Step 1 (`await fetch(charge)`) succeeds, `this.chargeId = data.id` is flushed at the next `await` (`Run::Await`), and Step 2 (`await fetch(sendReceipt)`) crashes. On retry, Step 1 must not re-run (it is already recorded in `this.chargeId`), *and* Step 2 must still execute rather than being skipped by an early return.
3. **Alarms and `@every` timers crashing mid-`await` (the lost-effect window):**
   `take_alarm` (`crates/tj-core/src/runtime/objects.rs`) removes the fired alarm before `Run::Await` flushes to disk, so a server crash mid-`await` does **not** re-fire the alarm automatically. And if the app schedules a reconciliation alarm, `new_alarm_id()` allocates a **new** handle every time (even for keyed alarms). Therefore an alarm's handle cannot serve as a crash-stable idempotency key; alarms that perform external writes must persist an explicit domain/operation id on the object *before* `await`.
4. **In-flight `EffectDone` across `tj reload`:**
   `drain_inflight` waits up to 2 seconds (`crates/tj-cli/src/engine.rs`) before swapping bundles; an external HTTP call taking >2 s has its late `EffectDone` discarded by `Dispatch.generation`, while the client's retry arrives on the new bundle with the same `Idempotency-Key`.

---

## Options compared

### Option A — Durable effect journal (workflow replay, Temporal / Restate style)

Persist an effect log inside each object's record: before emitting `Output::Effect`, save an intent (which could also carry an external idempotency key); when `Input::EffectDone` returns, save `(call_id, effect_seq, result)` to disk. On retry after a crash, replay the method from the start and feed cached effect results from the journal instead of re-executing `fetch`/`plugin`.

| Dimension | Assessment |
|---|---|
| **Closes crash between remote execution & local save?** | **Only if paired with external idempotency keys.** A local journal alone still double-executes if the server crashes *after* Stripe charges the card and *before* `EffectDone` is written to disk. |
| **Multi-step `async` without manual checks** | **Yes** — once Step 1's `EffectDone` is journaled, Step 2 crashing replays Step 1 from disk instead of calling the network. |
| **Live reload (`tj reload`) compatibility** | **Poor.** Reordering or editing any `await` in a method breaks positional `effect_seq` replay ("history non-determinism errors"), clashing directly with tinyjump's Living Apps live-reload workflow. |
| **Privacy (`@pii`) & storage cap (`TJ_MAX_OBJECT_BYTES`)** | **High risk.** Caching raw HTTP/plugin responses in object records silently persists untagged third-party payloads (potentially containing `@pii` or tokens), bypasses `@pii` masking/erasure, and blows up the 256 KB per-object cap. |
| **Core binary size & complexity** | **High** (~8–15 KB in `tj-core`, new persistence format, replay mismatch error surface, GC for incomplete journals). |

### Option B — Deterministic idempotency keys (`crypto.idempotencyKey`) + checkpointed object state

Keep the VM stateless across restarts and give `@server` methods **one** explicit primitive — `crypto.idempotencyKey(label)` — returning a deterministic, crash-stable key derived from the incoming `Idempotency-Key` and a caller-supplied step label, paired with explicit state checkpoints flushed at each `await`:

```ts
@server @read("owner") object Order {
  status: string = "open"
  chargeId: string = ""
  receiptSent: boolean = false

  @allow("owner") @exclusive async pay(amount: number): string {
    // Step 1: charge card (skipped on retry once `chargeId` was checkpointed).
    if (this.chargeId === "") {
      const res = await fetch("https://api.stripe.com/v1/charges", {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "Idempotency-Key": crypto.idempotencyKey("charge")
        },
        body: JSON.stringify({ amount })
      })
      if (res.status === 429 || res.status >= 500) {
        throw { message: `payment provider busy (${res.status})`, retryable: true }
      }
      check(res.ok, "payment", `charge failed (${res.status})`)
      this.chargeId = res.json().id
      this.status = "paid"
    }
    // `Run::Await` flushes `this.chargeId` and `this.status` to disk BEFORE Step 2!
    if (!this.receiptSent) {
      const mail = await fetch("https://api.mail.example/send", {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "Idempotency-Key": crypto.idempotencyKey("receipt")
        },
        body: JSON.stringify({ order: this.id, chargeId: this.chargeId })
      })
      check(mail.ok, "receipt", `receipt failed (${mail.status})`)
      this.receiptSent = true
    }
    return this.chargeId
  }
}
```

| Dimension | Assessment |
|---|---|
| **Closes crash between remote execution & local save?** | **Yes** — for every external API that honors `Idempotency-Key` (Stripe, Adyen, PayPal, Twilio, Resend, Square, GitHub, and downstream tinyjump servers). |
| **Multi-step `async`** | **Yes, explicitly.** Because `tj-core` flushes touched state at every `Run::Await` (atomically per object, so keep step checkpoints on `this`), guarding Step 1 (`this.chargeId === ""`) and Step 2 (`!this.receiptSent`) independently ensures Step 1 never re-runs if Step 2 crashes, while Step 2 still completes on retry. |
| **Live reload (`tj reload`) compatibility** | **Excellent.** No replay journal to corrupt; keys are bound to explicit labels (`"charge"`, `"receipt"`), not bytecode offsets or call indices, so refactoring surrounding lines never changes a step's key. |
| **Privacy (`@pii`) & storage cap** | **Zero storage bloat.** Nothing extra is persisted in object records; keys are opaque keyed hashes (`tj_<32 hex>`) containing no caller id or `@pii`. |
| **Core binary size & complexity** | **Small.** Runtime derivation lives behind `#[cfg(feature = "server")]` (~0.5 KB in server builds); device builds only pay the type-checker/builtin signature (~0.3–0.6 KB — must be measured against `scripts/budget.sh`, where WASM currently has 2 KB headroom). |
| **Non-idempotent legacy APIs** | **Limitation:** an external API that ignores idempotency keys still has the narrow in-flight crash window during its single HTTP round-trip; checkpointing state before/after bounds retries, and outgoing webhooks (with `X-TinyJump-Delivery`) handle fire-and-forget notifications. |

---

## Recommendation: Option B with ONE primitive (`crypto.idempotencyKey`)

Do **not** build a Temporal-style VM replay journal into `tj-core`. Instead, adopt **Option B** with **one** clear way to do each thing (`AGENTS.md`): **`crypto.idempotencyKey(label: string): string`**.

Why `crypto.idempotencyKey(label)` alone (instead of also adding a separate `fetch(..., { idempotency })` option):
- One surface works identically for standard `Idempotency-Key` headers, custom headers (`X-Request-Id`), JSON body fields, and `plugin()` arguments — zero duplicate ways to do the same thing, zero case-insensitive header-merging edge cases, and zero host protocol changes.

### 1. Key derivation (`tj-core`, `#[cfg(feature = "server")]`)

When an `@server` method runs with an incoming `call` id (`Input::Call.call`, from HTTP `Idempotency-Key` or device `tj:outbox`):

```text
idempotencyKey(label) = "tj_" + hex(HMAC-SHA256(store_secret, len_prefixed(
    object_key,
    method_name,
    caller_tag,
    call_id,
    label
)))[..32]
```

Key security and correctness properties:
- **Length-prefixed framing (`u32_be(len) || bytes`)** instead of `\0` delimiters: immune to embedded NULs or delimiter injection in `call_id` or `label`.
- **Scoped by `(object_key, method_name, caller_tag, call_id, label)`:** matches `call_once`'s exact caller-scoped key space (`calls.rs`), so two different callers sending a predictable HTTP `Idempotency-Key: 1` on a shared `@server` object (even after a crash or 72h / 100-record eviction) can **never** derive the same downstream key or replay each other's external response.
- **Shared `store_secret()` (failing closed with `503`):** extracts the existing `tj:secret` loader out of `caller_tag` (`privacy.rs`) so it works for every caller (including `Who::System`) and fails closed with `503` if the store cannot read or persist the secret.
- **When no incoming `call_id` exists** (e.g. an ad-hoc HTTP POST without `Idempotency-Key`, or a timer/alarm): derives from a fresh per-turn random `drbg` root (so `crypto.idempotencyKey` still produces a valid unique key rather than colliding across unrelated requests).
- **Duplicate-label guard within one live call:** tracks labels already issued on the current `Fiber` and fails fast with a clear fix hint if the same `label` is requested twice inside one call (catching accidental loop collisions like `for (const item of items) { ... crypto.idempotencyKey("item") }` — fix hint: include a unique item id, e.g. `crypto.idempotencyKey("item:" + item.id)`).
- **Server-only compile check:** `crypto.idempotencyKey(...)` is only meaningful on `@server` objects; calling it from device/UI code is a compile error with a fix hint (`call crypto.idempotencyKey() inside an @server object method`).
- **No automatic propagation across `tj:agent` `invoke()` loops:** an LLM agent loop (`plugins/agent/plugin.tj`) is non-deterministic across restarts and may invoke the same `(object, method)` multiple times in one turn. Agent tool methods should key off a durable domain id (e.g. `orderId`, `stepId`) or explicit per-tool-call argument rather than inheriting the outer chat turn's `call_id`.
- **Privacy flow check (`flow.rs`):** `crypto.idempotencyKey(label)` outputs a keyed hash (safe to send in headers), and `flow.rs` treats `label` as a sink argument so passing un-revealed `@pii` as `label` is caught at compile time.

### 2. Alarm & timer durability pattern (no `"alarm:" + alarm_id`)

Because `take_alarm` removes a fired alarm before `Run::Await` flushes and reconciliation alarms always allocate a new `alarm.id`, alarms that make external writes use an explicit persisted operation id:

```ts
@server object Payout {
  pendingId: string = ""
  paid: boolean = false

  @allow("staff") start() {
    if (this.pendingId === "") this.pendingId = "payout_" + crypto.randomId()
    this.schedule("retry", 0, "attempt")
  }

  private async attempt() {
    if (this.paid) return
    // Schedule reconciliation BEFORE `await` so a mid-flight crash retries automatically:
    this.schedule("retry", 60_000, "attempt")
    const res = await fetch("https://api.stripe.com/v1/transfers", {
      method: "POST",
      headers: { "Idempotency-Key": this.pendingId },
      body: JSON.stringify({ amount: 500 })
    })
    if (res.ok) {
      this.paid = true
      this.unschedule("retry")
    }
  }
}
```

### 3. Required runtime prerequisites before shipping Option B

Auditing `tj-core` and `tj-cli` surfaced **two existing runtime bugs** that the implementation PR for Option B MUST fix first so retries actually work end-to-end:

1. **Abort `Run::Await` if `flush_from(mark)` fails to save to disk (`crates/tj-core/src/runtime/mod.rs:661`):**
   Currently `Runtime::fetch` pushes `Output::Effect` before `settle()` runs `flush_from(mark)`, and `settle()` ignores `flush_from`'s `bool` return value. If persisting the pre-`await` checkpoint fails (e.g. disk full), `flush_from` reverts object state while leaving `Output::Effect` queued and the fiber parked in `self.waiting`. Fix: when `flush_from(mark)` returns `false` on `Run::Await`, remove the fiber from `self.waiting`, release its `@exclusive` locks (`release_frames`), drop uncommitted `Output::Effect`s in `mark..`, and reply `503` via `refuse_start`.
2. **Do not permanently cache transient `fetch` transport failures in `CallRecord` (`crates/tj-cli/src/engine.rs:1083`, `crates/tj-core/src/runtime/mod.rs:712, 779`):**
   Today, a server-side `fetch` timeout or TCP disconnect (`ureq::Error::Transport`) comes back as `EffectDone { error: Some(...), unreachable: false }`, unwinds via `Exc::with_field` (`ExcKind::User`), and `finish_call` caches it as a permanent HTTP `400` `CallRecord` for 72 hours. A device retrying the same `Idempotency-Key` gets the cached `400` forever — and if the client generates a fresh `Idempotency-Key` to work around it, it gets a *different* downstream key and double-charges! Fix: report `ureq::Error::Transport` with `unreachable: true` (or a transient `502`/`503` flag on `ExcKind::Transient`) so an uncaught downstream transport failure replies `502`/`503` via `refuse_start` (NOT caching a permanent `CallRecord`), allowing the device outbox to safely retry the **same** `Idempotency-Key`! (App code that catches the error with `try/catch` handles it as normal.)

---

## Rollout plan

1. **PR (implementation, after owner/Critic sign-off):**
   - Fix the two runtime prerequisites (`Run::Await` flush-failure abort, and transient `fetch` transport errors returning `502`/`503` via `refuse_start` instead of caching permanent `400` `CallRecord`s).
   - Implement `crypto.idempotencyKey(label)` with length-prefixed HMAC over `(object_key, method_name, caller_tag, call_id, label)`, `store_secret()`, per-fiber duplicate-label detection, compile-time `@server`-only check, and `flow.rs` sink check.
   - Add crash-and-retry E2E tests in `crates/tj-core/tests/runtime.rs` (including multi-step crash, transport-error retry with same `Idempotency-Key`, cross-caller key isolation, and duplicate-label rejection) and verify `scripts/budget.sh` (watch the 2 KB WASM headroom).
   - Document `crypto.idempotencyKey(label)` and the alarm reconciliation pattern in `docs/language.md`.
2. **Defer:** No noisy blanket `tj check` warning on all non-GET `fetch` calls (avoids false positives on read-only POST search/GraphQL endpoints); outgoing fire-and-forget notifications are handled by outgoing webhooks (Track 2 Step 3).

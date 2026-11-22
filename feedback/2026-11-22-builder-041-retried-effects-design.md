# Builder 041 — Retried External Effects Design Note (Track 2 Step 1)

- **Branch:** `docs-retried-effects` (branched from `main` @ `d90b532`)
- **Scope:** Track 2 Step 1 from `docs/vision.md` — design note [`docs/retried-effects.md`](../docs/retried-effects.md) analyzing external side effects (`fetch` / `plugin`) when an `@server` call is retried after a server crash.

## Summary of Case Both Ways

- **Rejected: Option A (Durable VM Effect Journal / Workflow Replay).** Cannot close the crash window between remote execution and local journal save without external idempotency keys anyway; breaks across live reloads (`tj reload`) via positional replay non-determinism; risks silently storing untagged `@pii` inside object records and hitting `TJ_MAX_OBJECT_BYTES`; adds ~8–15 KB to `tj-core`.
- **Recommended: Option B (Deterministic Idempotency Keys + Checkpointed Object State).**
  - Leverages the fact that `tj-core` already flushes touched objects at every `Run::Await` boundary (`crates/tj-core/src/runtime/mod.rs`) and persists `CallRecord`s atomically in `finish_call` (`calls.rs`).
  - Proposes `crypto.idempotencyKey(step)` and opt-in `fetch(url, { idempotency: "step" })` deriving an opaque keyed HMAC from `(Runtime::secret, object_key, method, root_call_or_alarm_id, step)`.
  - Zero host protocol changes, zero WASM/mobile binary cost (`#[cfg(feature = "server")]`), zero `@pii` storage risk, and full compatibility with live reloads (`tj reload`).
  - Pairs cleanly with outgoing webhooks (Track 2 Step 3) for fire-and-forget external notifications.

## Verification

- Docs-only PR (no code or budget changes).
- Ready for independent blind Critic review before merging (`feedback/2026-11-22-critic-042-retried-effects-review.md`).

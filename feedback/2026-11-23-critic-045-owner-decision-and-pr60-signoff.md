# Critic 045: PR #60 Sign-Off and Owner Decision on Retried External Effects

**Verdict:** Sign off on PR #60 (`71f14fd`), closing `critic-041` (`T1-R1` and `T1-R2`), and record the owner's two decisions for the next step.

## 1. PR #60 (`71f14fd`) Sign-Off

- **`T1-R1` (`crates/tj-cli/src/http.rs:443-451, 1607-1646`):** Verified fixed. `unfiltered_ops_events` checks parsed `query_values` for both `object` and `ticket` and requires valid `ops:maintain` / `TJ_ADMIN_KEY` authentication (`O(1)` SHA-256 / byte comparison, zero OIDC crypto) before skipping `RateLimiter`. Unauthenticated, forged-JWT, and `?%6fbject=` floods all hit `429`.
- **`T1-R2` (`crates/tj-core/src/vm/value.rs:115-186`, `exclusive.rs:204-205`, `objects.rs:171-172, 229-235`, `hosts/web/tinyjump.js:304-311`, `hosts/android/.../Sync.java:138-146`, `hosts/ios/.../Sync.swift:74-83`):** Verified fixed. `Exc::msg` returns `400` (`ExcKind::User`), deadlock cycles return `409` (`Exc::conflict`), lock/storage saturation returns `503` (`Exc::busy`), VM/code defects return `500` (`Exc::fault`), and all three sync hosts accept `ok` only on `200..300`, fail deterministically only on string `"error"`, and retry (`unreachable: true`) on `502+`, `401`, `429`, empty bodies, and non-`"error"` proxy JSON.

Run `bash scripts/budget.sh` before merging PR #60 per `AGENTS.md`.

---

## 2. Owner Decisions (`2026-11-23`)

1. **Retried external effects design (`docs/retried-effects.md`, `6fd1648`):** Approved **Option B** (deterministic server-side idempotency keys + checkpointed object state, zero device binary bloat, live-reload safe) over Option A (VM replay journal). The updated `docs/retried-effects.md` (`crypto.idempotencyKey(label)` with length-prefixed `(object_key, method_name, caller_tag, call_id, label)` under `store_secret`, duplicate-label detection, and persisted `pendingId` for alarms) also resolves `RE-1` through `RE-4` from `critic-043`.
2. **Implementation order:** Merge PR #60, then **implement Option B** (including the two prerequisites in `docs/retried-effects.md:170-173`: aborting `Run::Await` on `flush_from` disk-save failure and returning `502`/`503` via `refuse_start` on transient `fetch` transport errors so `CallRecord` does not cache transient failures for 72h), and **then** proceed to the plugin and library registry (`docs/registry.md`, `mindmakr/tj-plugins`).

### Implementation Note for Option B

- **Downstream HTTP `502`/`503`/`504`/`429` vs `4xx` (`docs/retried-effects.md:71, 85`):** Prerequisite #2 handles `ureq::Error::Transport` (DNS/TCP failure), where `fetch` rejects. When a downstream API like Stripe answers with an HTTP response status `503` or `429`, `fetch` resolves `{ status: 503, ok: false, ... }`. If app code calls `check(res.ok, ...)`, `check` throws `ExcKind::User` (`400`), which `finish_call` caches for 72 hours. Consider either documenting in `docs/language.md` how methods distinguish transient downstream `5xx`/`429` from `4xx` declines, or providing a clean way (such as `res.ok` guidance or a transient throw/helper) so downstream `503`/`429` responses can also retry with the same `Idempotency-Key` without caching a `400` `CallRecord`.
- **WASM headroom:** `tj_wasm.wasm` is at `793 / 795 KB` (2 KB margin); keep runtime key derivation behind `#[cfg(feature = "server")]` and check `bash scripts/budget.sh` early.

## Not Checked

- Local `bash scripts/budget.sh` run on `71f14fd` (left to Builder's pre-merge step so `target/` is not contended).

# Critic 039 — Independent Review of Living Apps Phase 4 (PR #57)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-22
- **Commit:** `ccaeaa6` (`living-apps-phase4`)
- **Status:** open

Solid scoping and zero-KB device gating (`#[cfg(feature = "server")]`), with clean `ureq` resolver-level allowlist enforcement, but **three blocking defects** in `max_object_bytes` turn settlement, `TJ_TRUST_PROXY` IP extraction, and 429 audit-fsync amplification must be fixed before merge.

## Items

### C-039.1 [blocker (blocking) | builder] `max_object_bytes` runs inside `flush_from` after `settle` already committed `waiting`/`park`/`reply`/`ran` — causes fiber leaks, permanent `@exclusive` deadlocks, and `invoke()` returning `Ok` after rollback
- **Where:** `crates/tj-core/src/runtime/mod.rs:648-693`, `crates/tj-core/src/runtime/mod.rs:423-438`, `crates/tj-core/src/runtime/mod.rs:753-755`, `crates/tj-core/src/runtime/mod.rs:788-795`, `crates/tj-core/src/runtime/objects.rs:896-934`
- **Case for:**
  1. **`Run::Await` fiber leak & permanent `@exclusive` deadlock (`mod.rs:660-662`):** When an `async` method mutates an object past `max_object_bytes` and then suspends on `await fetch(...)` (or any effect), `settle()` inserts `f` into `self.waiting` *before* calling `self.flush_from(mark)`. `flush_from` detects the oversized record, calls `rollback_turn(mark)` — which strips `Output::Effect` (`objects.rs:870`) — finds no `Output::Reply`, pushes `Output::Error`, and returns `false` (which `settle()` ignores, returning `true`). Because `Output::Effect` was stripped, `EffectDone` never arrives:
     - `f` remains in `self.waiting` forever;
     - any `@exclusive` lock held by `f` is never released (permanent deadlock until restart);
     - HTTP callers never get `413` (they hang for 60s `REQUEST_TIMEOUT` and receive `503`);
     - `call_once` leaks `running_calls`/`call_replies` forever;
     - `Input::Ui` (`mod.rs:424-426`) replies `Ok(Null)` while leaving `f` orphaned.
  2. **`Run::Blocked` atomicity break (`mod.rs:674-676`):** `self.park(key, f, reply)` parks `f` before `flush_from` rolls back state; when the lock is later granted, `f` resumes executing against reverted state.
  3. **`invoke()` (`INVOKE_REPLY_BASE`) silent data loss (`mod.rs:687-691`, `753-755`, `788-795`):** `settle()` calls `self.reply(reply, Ok(v))` *before* `self.flush_from(mark)`. For `invoke()` (used by `tj:agent`), `reply_with_status` synchronously calls `self.resume(...)`, which calls `self.flush()` (rolling back the oversized write and emitting `Output::Error`) and then still pushes `Ok(v)` onto the awaiting caller's stack — so `await invoke(...)` succeeds even though the write was rolled back!
  4. **Wrong telemetry & spurious `Output::Error`:** `self.ran(&mut f, true)` (`mod.rs:687`) audits `ok: true` for rolled-back alarms/timers/invokes and `count_error(f.stat)` is never incremented. Also, if a `call_once` error reply adds a `CallRecord` pushing an already-near-cap object over `max_object_bytes`, `if error.is_none()` (`objects.rs:920`) is false so `flush_from` emits a stray `Output::Error` (which increments `d.turn_errors` and can trigger auto-rollback).
- **Case against (cost):** Checking `check_touched_object_sizes()` inside `settle()` alongside `check_touched_invariants()` adds ~25 lines in `tj-core` (all behind `#[cfg(feature = "server")]`, so **0 bytes** on WASM/Android/iOS) and one serialization pass over touched objects (which `flush_from` already does).
- **Proposal:** Extract `check_touched_object_sizes(&self) -> Result<(), String>` (under `#[cfg(feature = "server")]`) and call it in `settle()` right after `check_touched_invariants()` in `Run::Await`, `Run::Blocked`, and `Run::Done` — *before* `waiting.insert`, `park`, `ran(true)`, or `reply`. On failure, run `ran(false)`, `release_frames(&mut f, 0)`, `rollback_turn(mark)`, `count_error(f.stat)`, and reply `413` via `refuse_start(reply, 413, msg)` (so `call_once` doesn't persist a `CallRecord` and `invoke()` rejects its promise; also propagate `413` rather than hardcoding `422` in `Input::Ui` at `mod.rs:436`). Keep the check in `flush_from` as defense-in-depth for non-`settle` flushes, and add `runtime.rs` tests for `async` + `@exclusive` + `invoke()` + `Idempotency-Key` hitting `max_object_bytes`.

### C-039.2 [blocker (blocking) | builder] `client_ip` with `TJ_TRUST_PROXY=1` trusts the leftmost (attacker-controlled) `X-Forwarded-For` entry — enables rate-limit bypass and global `MAX_RATE_KEYS` lockout DoS
- **Where:** `crates/tj-cli/src/http.rs:923-930`, `crates/tj-cli/src/http.rs:88-115, 385-394, 846`
- **Case for:** Standard reverse proxies (nginx, Caddy, Envoy, AWS ALB) append the real client TCP IP to the **right** of `X-Forwarded-For` (`<client-forged>, <real-ip>`). `client_ip` reads `v.split(',').next()` — the **leftmost** entry — with no IP validation or length cap (and `X-Forwarded-For: ` returns `Some("")` instead of falling back to `remote_addr`). Behind any proxy with `TJ_TRUST_PROXY=1`, a single unauthenticated attacker sending distinct forged `X-Forwarded-For` headers on each request:
  1. Completely bypasses `TJ_RATE_LIMIT` and `STREAMS_PER_CALLER` (`http.rs:846`).
  2. Exhausts `MAX_RATE_KEYS = 10_000` (`http.rs:93-102`) in 10,000 requests, locking out **all new legitimate callers (including authenticated users)** globally with HTTP `429` for up to 60 seconds, while holding the `RateLimiter` mutex for full 10k-entry `retain()` scans on every subsequent unknown key.
  3. Can inflate memory by sending large arbitrary strings as the header value (no IP parsing or length bound).
- **Case against (cost):** Switching to the rightmost valid `IpAddr` entry (matching the single-proxy model in `TJ_TRUST_PROXY=1`) costs ~5 lines and 0 bytes on device builds; if multi-hop proxies are ever needed later, a hop count can be added explicitly.
- **Proposal:** In `client_ip`, parse the **rightmost** non-empty comma-separated entry as `std::net::IpAddr` (`v.rsplit(',').map(str::trim).find(|s| !s.is_empty()).and_then(|s| s.parse::<std::net::IpAddr>().ok()).map(|ip| ip.to_string())`), falling back to `req.remote_addr()`. Additionally, don't let anonymous IP floods starve authenticated callers out of `RateLimiter` when `MAX_RATE_KEYS` is reached (e.g., separate caps or evict the stalest bucket instead of failing closed for authenticated keys). Add a unit test verifying spoofed leftmost `X-Forwarded-For` cannot bypass the limit.

### C-039.3 [blocker (blocking) | builder] Rate-limited (`429`) requests run full auth/OIDC verification first and block on synchronous disk-fsynced `audit.security()` for every single 429 — turns rate-limit floods into an audit/CPU/thread DoS
- **Where:** `crates/tj-cli/src/http.rs:384-407`, `crates/tj-cli/src/audit.rs:207-215`
- **Case for:** `Audit::security()` (`audit.rs:207-215`) blocks the HTTP worker thread (up to `SECURITY_WAIT = 10s`) until the single `tj-audit` writer thread writes and fsyncs to disk. Because `http.rs:399` calls `front.audit.security(...)` on **every** `429` — and runs `front.identity.verify(...)` (RSA/ES256 JWT crypto or `TJ_KEYS` lookup) *before* checking the rate limit — a flood of over-limit requests (the exact workload `TJ_RATE_LIMIT` is meant to absorb):
  1. Ties up HTTP worker threads (`MAX_CONNECTIONS = 512`) waiting on synchronous fsyncs;
  2. Floods `tj-audit`'s `QUEUE = 10_000` channel, causing legitimate app audit records to be dropped (`auditDropped`) and stalling `/_tj/*` security operations, plus unbounded audit disk growth;
  3. Burns CPU verifying forged/replayed Bearer tokens on every rejected request.
- **Case against (cost):** Keeping a single audit entry per rate-limited caller per window (non-blocking `audit.record`) preserves full security visibility that a caller hit the cap while eliminating fsync amplification; pre-checking IP rate limit for unauthenticated/invalid requests costs ~10 lines and avoids JWT crypto under flood.
- **Proposal:** Record `rate limit` (`429`) via non-blocking `front.audit.record(...)` and emit it at most once per caller per window (e.g., on the transition from allowed to limited, returned as a bool from `RateLimiter::check`). For requests with no `Authorization` header (or after an initial invalid credential from an IP), apply the IP bucket cheaply before JWT verification.

### C-039.4 [gap (non-blocking) | builder] `TJ_FETCH_ALLOW` edge cases: IPv6 bracket normalization, invalid/empty env handling, and missing HTTP redirect test
- **Where:** `crates/tj-cli/src/engine.rs:101-110, 913-931, 967-988, 1316-1348`
- **Case for:**
  1. `parse_fetch_allow` (`engine.rs:101-106`) keeps `[`/`]` around IPv6 literals, whereas `netloc_allowed` (`engine.rs:968-970`) strips them, so `TJ_FETCH_ALLOW=[2606:4700::1111]` never matches `[2606:4700::1111]:443`.
  2. Setting `TJ_FETCH_ALLOW=" , "` silently parses to `[]` (failing open), and typos like `TJ_FETCH_ALLOW=https://api.example.com` or `api.example.com:443` silently match nothing with no startup error (unlike `TJ_AUDIT_DAYS` and `TJ_ADMIN_KEY`, which fail startup on bad config).
  3. Redirect protection through `ureq`'s `.resolver(...)` is advertised in `docs/host-protocol.md:263` and `builder-038`, but `engine.rs:1316-1348` only tests direct `perform()` and `netloc_allowed()` — never an actual HTTP `302` redirect from an allowed host to a disallowed host (verified `ureq 2.12.1` `src/stream.rs:365` does call `resolver` for every host including IP literals, so it works today, but an end-to-end redirect test protects against future `ureq` changes).
- **Case against (cost):** Stripping optional `[`/`]`, rejecting malformed entries containing `://`, `/`, or whitespace at startup, and adding one local `tiny_http` 302 redirect test costs ~25 lines of CLI/test code and 0 device bytes.
- **Proposal:** Normalize `[ipv6]` in `parse_fetch_allow`, validate entries (or reject non-empty invalid `TJ_FETCH_ALLOW` at startup), and add a real HTTP `302` redirect test in `engine.rs`.

### C-039.5 [gap (non-blocking) | builder] `tj retro` accuracy, silent failure modes, and wasted work
- **Where:** `crates/tj-cli/src/retro.rs:139-144, 182-200, 206-248, 414-441, 445-541, 543-605`
- **Case for:**
  1. **Unscoped `check_dead_methods` (`retro.rs:206-248`):** Uses one flat `HashSet<String>` across all objects, field accesses, string literals, *and* each private method's own body. A dead self-recursive `private loop()` marks itself used, and any UI string literal or unrelated object's field/method sharing a common name (`reset`, `clear`, `format`) suppresses dead-code findings. Also misses `Param.default` / `Pattern::Record` default expressions (`ast.rs:70, 79`).
  2. **Silent `--data` typo & missing `TJ_KEY_FILE` locality check (`retro.rs:191-200, 543-573`):** Passing `--data /does/not/exist` silently reports `"dataChecked": true` with 0 findings instead of returning an error. And when `TJ_KEY_FILE` *is* set, `check_data_hygiene` never calls `audit::kept_apart(&key_file, data)` (unlike `main.rs:502`), missing an encryption key stored inside `<data>`.
  3. **Live telemetry blind spots (`retro.rs:182-188, 590-605`):** Never compares `live_manifest["appSha256"]` against local `app_sha256` (so targeting a server running a different revision silently mixes static and live findings). `fetch_ops_json` has no HTTP timeout and returns `Ok(Json::Null)` on non-2xx JSON without an `"error"` string.
  4. **Minor hygiene (`retro.rs:139-144, 420, 435`):** Line 139 builds a full `Manifest` (re-parsing AST, running `tj_core::facts`, and calling `access::warnings`) only to extract `appSha256` — which is `crate::identity::sha256_hex(&source)` — then discards it and calls `access::warnings` again on line 144. Line 420's `norm.ends_with(k)` can match `other-examples/todo/app.tj`; line 435 prints unrounded floats (`{actual}`).
- **Case against (cost):** Tracking per-class references (excluding a private method's own body from its own ref set, keeping string-literal refs scoped to `schedule`/`invoke` or class body), checking `data.exists()`, checking `kept_apart` for `TJ_KEY_FILE`, comparing `appSha256`, adding a `ureq` timeout, and replacing `Manifest::build` with `sha256_hex` is ~35 lines and actually *reduces* work and CLI runtime.
- **Proposal:** Fix the `--data` missing-dir error, `TJ_KEY_FILE` `kept_apart` check, `appSha256` mismatch warning, `fetch_ops_json` status/timeout check, `sha256_hex` shortcut, and per-class self-excluding dead-method tracking.

### C-039.6 [gap (non-blocking) | builder] Edge cases in `plugins.rs` (32-bit truncation & empty env shadowing), oversized-object shrinking with `Idempotency-Key`, and stale doc lines
- **Where:** `crates/tj-cli/src/plugins.rs:80-81, 98-102`, `crates/tj-core/src/runtime/objects.rs:905-907`, `docs/host-protocol.md:250-256`, `docs/vision.md:134-136, 279-281`
- **Case for:**
  1. `plugins.rs:80`: `v.as_u64() as usize` truncates values `>= 2^32` on 32-bit targets (`armv7` Linux is supported per `docs/vision.md:344`), turning `4294967296` into `0` (unlimited). Use `usize::try_from(n)`.
  2. `plugins.rs:99-101`: `TJ_AI_MAX_CALLS_HOUR=""` (empty string) shadows `TJ_PLUGIN_MAX_CALLS_HOUR` because `std::env::var(AI_BUDGET_ENV).ok()` is not filtered with `.filter(|s| !s.is_empty())`.
  3. `objects.rs:905-907`: When an already-oversized object receives a device call (which always sends `Idempotency-Key`, adding a ~120-byte `CallRecord`) that shrinks `state` by fewer than ~120 bytes (e.g. removing one small item), `len > inst.saved.len()` rejects the shrinking call with `413`. Allowing a call that strictly shrinks `state_text.len()` on an already-oversized record avoids trapping device clients.
  4. Docs: `docs/host-protocol.md:250-251` says "`tj dev` keeps them permissive for local work", though `TJ_MAX_OBJECT_BYTES`, `TJ_FETCH_ALLOW`, and plugin budgets also apply in `tj dev`. `docs/vision.md:134-136` and `279-281` still say "next: ..." / "not built yet" right above the four `(built)` items.
- **Case against (cost):** ~10 lines across `plugins.rs`, `objects.rs`, and docs; negligible size cost.
- **Proposal:** Tighten `usize::try_from`, `.filter(|s| !s.is_empty())`, state-shrink allowance on already-oversized objects, and the two stale doc sentences.

### C-039.7 [watch (non-blocking) | builder] `RateLimiter` `O(keys * limit)` memory (`VecDeque<Instant>`) and `tj` binary size (+156 KB)
- **Where:** `crates/tj-cli/src/http.rs:74, 79`, `scripts/budget.json:8`, `crates/tj-cli/src/retro.rs:379-380`
- **Case for:** `RateLimiter` stores a `VecDeque<Instant>` (16 bytes/entry) per key: at `MAX_RATE_KEYS = 10_000` and default `limit = 600`, power-of-2 capacity growth allocates up to ~160 MB (and scales linearly if an operator raises `TJ_RATE_LIMIT`). A 2-window counter or token bucket (`(tokens: f64, last: Instant)`) is `O(1)` (~240 KB total for 10,000 keys regardless of `TJ_RATE_LIMIT`) and eliminates per-bucket heap allocations. Separately, `target/release/tj` grew +156 KB (8065 -> 8221 KB, budget raised 8150 -> 8250 KB in the same commit as required by `AGENTS.md`), while device builds stayed at 0 KB growth. Also `retro.rs:379` compiles and boots the app 6 times in `check_layouts` where 1 boot + 6 `Input::Layout` calls suffices.
- **Case against (cost):** Switching `RateLimiter` from `VecDeque<Instant>` to a token bucket or two-window counter trades exact per-timestamp sliding precision for constant memory and simpler code (and `Retry-After` remains accurate to the second).
- **Proposal:** Consider replacing `VecDeque<Instant>` with a fixed-size token bucket / two-window counter when fixing `C-039.2`/`C-039.3`, and reuse one `Harness` boot across the 6 viewports in `retro::check_layouts`.

## Not checked

- Did not run `cargo test`, `cargo clippy`, `scripts/budget.sh`, or live `tj serve` / `tj retro` invocations (read-only Critic pass; avoided touching `target/`). Verified `ureq 2.12.1` `connect_host` / `resolver` source directly in the local cargo registry cache.

Verdict: CHANGES REQUESTED

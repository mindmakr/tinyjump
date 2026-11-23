# Critic 050: Independent Review of PR #63 (`server-replicas-webhooks`)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-23
- **Commit:** `670dfce` on `server-replicas-webhooks`
- **Status:** open
- **Verdict:** **REQUEST CHANGES**

Solid architecture and clean `#[cfg(feature = "server")]` gating (0 KB added to device builds), with real 2-server and webhook E2E coverage, but six blockers in encryption-at-rest (`TJ_KEY_FILE`), external access to remote classes, outgoing webhook crash durability, incoming timestamp overflow, `Class.all()` SSE reconnect storms, and `@pii` privacy flow / compliance transfer checks must be resolved before merge.

## Items

### C-050.1 [blocker | builder] `<data>/webhooks-queue.json` bricks `TJ_KEY_FILE` encrypted servers on restart/reload/rekey and writes payloads in plaintext
- **Where:** `crates/tj-cli/src/webhooks.rs:52, 398-410, 429`; `crates/tj-cli/src/store.rs:182-191`; `crates/tj-cli/src/sealed.rs:356-390, 424-441`; `crates/tj-cli/src/engine.rs:160, 439`; `crates/tj-cli/src/main.rs:568`
- **Case for:**
  1. `WebhookSender` writes `webhooks-queue.json` directly inside `data_dir` (`webhooks.rs:52, 429`). `FileStore::keys("")` (`store.rs:182-191`) enumerates every `*.json` file in `data_dir` and returns `"webhooks-queue"`.
  2. When `TJ_KEY_FILE` is set, `sealed::open_store` (`sealed.rs:366-372`) rejects any name where `!is_sealed_name(n)`. As soon as an encrypted server enqueues a single outgoing webhook (even after the queue drains back to `[]`), the next server restart (`main.rs:568`), `POST /_tj/reload` (`engine.rs:439, 708`), or `POST /_tj/rollback` (`engine.rs:767`) fails hard with `"holds records that are not encrypted; run tj store rekey"`.
  3. Following that error message and running `tj store rekey` (`sealed.rs:424-441`) encrypts `webhooks-queue.json` into an orphaned hashed blob and deletes `webhooks-queue.json`, losing all pending deliveries.
  4. Writing the queue in plaintext violates `TJ_KEY_FILE` encryption-at-rest: every queued `changed` entry contains the object key (e.g., `"Account/user:ana"` — a person's ID) and any `@pii` fields readable by the webhook's configured `roles` (or `reveal()`ed data in `emit`). `QueueState::save` (`webhooks.rs:405-409`) also omits `sync_dir` and silently discards I/O errors.
- **Case against (cost):** Putting the queue in a subdirectory `<data>/webhooks/` (like `<data>/audit/` and `<data>/subjects/`) backed by `sealed::open_store(&data_dir.join("webhooks"), &[])` with key `"queue"` costs ~20 lines in `tj-cli` (0 KB device cost) and automatically inherits atomic writes, `sync_dir`, `0600` permissions, `TJ_KEY_FILE` encryption-at-rest, and isolation from `<data>/*.json`.
- **Proposal:** Move the outgoing webhook queue to `<data>/webhooks/` backed by `sealed::open_store(&data_dir.join("webhooks"), &[])` (logging save errors instead of swallowing them), update `docs/host-protocol.md:380`, and add a test proving an encrypted (`TJ_KEY_FILE`) server with outgoing webhooks survives restart and reload.

### C-050.2 [blocker | builder] External `GET /api/{RemoteClass}/{id}` and `GET /api/events?object={RemoteClass}/{id}` trigger arbitrary `RemoteHub` subscriptions (confused deputy + SSE reconnect DoS)
- **Where:** `crates/tj-core/src/runtime/mod.rs:419-470`; `crates/tj-core/src/runtime/access.rs:100-157`; `crates/tj-core/src/runtime/objects.rs:193-198, 632-669`; `crates/tj-core/src/runtime/calls.rs:286-300`; `crates/tj-cli/src/remotes.rs:311-350, 418-427`
- **Case for:**
  1. PR #63 correctly blocks external `Input::Call` on remote classes before `resolve_object` (`mod.rs:419-426`) and suppresses local `Output::Changed` for remote replicas on servers (`objects.rs:1059`).
  2. However, `Input::Get` (`mod.rs:449-470`), `Runtime::may_read` / `changed_since` (`access.rs:152`, `objects.rs:650`), and `run_invocations` (`calls.rs:286`) still call `resolve_object` on remote classes!
  3. Any external caller allowed by the local `@read` rule can request `GET /api/Stock/attacker-1` .. `GET /api/Stock/attacker-500` (or `GET /api/events?object=Stock/attacker-N`): each request emits `Output::Subscribe { object: "Stock/attacker-N", seq: 0 }` (`objects.rs:197`), which inserts the attacker-chosen ID into `RemoteHub`'s unbounded `stream.objects`, bumps `stream.version`, forces an SSE reconnect to Server A using Server B's privileged `keyEnv` Bearer key, and eventually overflows the HTTP URI length on `GET /api/events?object=...` — wedging all replica sync from Server A.
  4. If Server B's local stub has a looser `@read` rule than Server A (e.g., `@read("anyone")` locally while Server B authenticates to Server A with a `"service"` API key), two `GET /api/Stock/{id}` requests let an external caller pull arbitrary remote objects from Server A through Server B's service key (confused deputy).
- **Case against (cost):** Refusing remote classes on servers before `resolve_object` in `Input::Get`, `Runtime::refusal`/`may_read`, and `run_invocations` is ~12 lines behind `#[cfg(feature = "server")]` (0 KB device cost). External clients already cannot stream remote classes from Server B because `Output::Changed` is suppressed for remote replicas (`objects.rs:1059`), so refusing direct external reads/streams is consistent with the design.
- **Proposal:** Fail closed on remote classes before `resolve_object` in `Input::Get`, `Runtime::refusal`/`may_read` (covering `GET /api/events` and `POST /api/events/ticket`), and `run_invocations`, and add core tests in `crates/tj-core/tests/runtime.rs`.

### C-050.3 [blocker | builder] `WebhookSender::run_sender` loses in-flight deliveries on crash/restart when a concurrent `enqueue` saves the queue
- **Where:** `crates/tj-cli/src/webhooks.rs:472-480, 524-597`
- **Case for:** `run_sender` removes the head delivery `d` from `q.items` (`q.items.remove(pos)` at line 525), drops the mutex, and sends the HTTP POST (up to 15 s timeout). Any `WebhookSender::enqueue` (`webhooks.rs:472-480`) that runs while `d` is in-flight pushes the new item into `q.items` (which no longer contains `d`) and calls `q.save()`, overwriting disk without `d`. A crash, restart, or shutdown before `agent.post` returns permanently loses `d` even though delivery never completed.
- **Case against (cost):** Keeping an `in_flight: Option<PendingDelivery>` field on `QueueState` (serialized ahead of `items` in `QueueState::save()`, cleared on completion/exhaustion, or pushed back to `items` on retry before saving) is ~10 lines with zero extra disk writes and 0 KB device cost.
- **Proposal:** Store `in_flight` in `QueueState` and include it in `QueueState::save()`, removing it only once the HTTP attempt finishes or exhausts retries. Add a unit test verifying `enqueue` during an in-flight delivery preserves both items on disk.

### C-050.4 [blocker | builder] Unauthenticated `X-TinyJump-Timestamp` overflow panics in debug builds and bypasses the replay window in release builds
- **Where:** `crates/tj-cli/src/webhooks.rs:264-270`
- **Case for:** `let ts: i64 = ts_str.parse()?; if (now_secs - ts).abs() > hook.max_age_secs` overflows when an unauthenticated caller sends `X-TinyJump-Timestamp: -9223372036854775808` (or `now_secs.wrapping_sub(i64::MIN)`):
  - In debug builds (`tj dev` / tests): panics (`attempt to subtract with overflow`), crashing the request thread before `bad:{ip}` rate limiting is charged.
  - In release builds (`tj serve`): `now_secs.wrapping_sub(ts)` wraps to `i64::MIN`, `i64::MIN.wrapping_abs()` stays `i64::MIN < 0 <= max_age_secs`, bypassing the replay window check. `ts_str.parse::<i64>()` also accepts non-canonical strings like `"+1700000000"`.
- **Case against (cost):** Checking `!ts_str.bytes().all(|b| b.is_ascii_digit())` and `now_secs.abs_diff(ts) > hook.max_age_secs as u64` is 2 lines with zero overhead.
- **Proposal:** Require ASCII-digit `ts_str` (`ts > 0`), use `now_secs.abs_diff(ts) > hook.max_age_secs as u64`, and add unit tests for `i64::MIN`, `i64::MAX`, negative, and `+`-prefixed timestamps.

### C-050.5 [blocker | builder] `RemoteHub` triggers an O(N²) SSE reconnect storm and URL bloat on `Class.all()` (`Class/*` streams)
- **Where:** `crates/tj-cli/src/remotes.rs:311-350, 414-427, 489-504`; `crates/tj-core/src/runtime/objects.rs:193-198, 352, 687-694`
- **Case for:** When Server B calls `Stock.all()`, it emits `Output::Subscribe { object: "Stock/*", seq: 0 }` (`objects.rs:693`). For every object `Stock/sK` delivered over SSE, `Runtime::sync` calls `resolve_object("Stock/sK")` -> `instance()` -> emits `Output::Subscribe { object: "Stock/sK", seq: 0 }` (`objects.rs:197`). `RemoteHub::subscribe` (`remotes.rs:332-350`) does not check whether `"Stock/*"` is already subscribed, so it inserts `"Stock/sK" -> 0` and bumps `stream.version` — tearing down and reconnecting the live SSE stream mid-catchup for every single object, resetting `min_seq` to `0`, and appending every instance ID to `?object=...`.
- **Case against (cost):** In `RemoteHub::subscribe`, ignoring `Class/id` when `Class/*` is already in `stream.objects` (and pruning redundant `Class/id` entries when `Class/*` is added) is ~10 lines in `remotes.rs` with 0 KB device cost.
- **Proposal:** Deduplicate `Class/id` against `Class/*` in `RemoteHub::subscribe`, and add a test with multiple remote objects fetched via `Class.all()` verifying no redundant per-instance subscriptions or reconnect storms occur.

### C-050.6 [blocker | builder] `@pii` privacy flow check and compliance `transfers` bypasses on remote server calls and outgoing webhooks
- **Where:** `crates/tj-core/src/flow.rs:1064-1069, 1105-1114`; `crates/tj-cli/src/compliance.rs:232, 267-300`; `crates/tj-cli/src/privacy.rs:88-94`; `crates/tj-cli/src/engine.rs:948-971`; `crates/tj-cli/src/main.rs:572-591`
- **Case for:**
  1. **Unrevealed `@pii` sent to remote servers:** On Server B, remote classes declared in `remotes.json` have stub method bodies (e.g. `http.rs:711-714`: `reserve(n: number): number { return 0 }`). If Server B calls `await RemoteBilling.get(id).charge(this.customerEmail)`, `flow.rs:1105-1114` analyzes the empty local stub `{ return 0 }`, sees `email` is unused, and **compiles cleanly without `reveal()` or `@needs("consent:...")`** — sending raw `@pii` over HTTP in `Effect::Call` and persisting it in `tj:outbox`.
  2. **Compliance `transfers` and Article 30 blind spots:** An outgoing webhook in `webhooks.json` with `"roles": ["staff"]` on a class whose `@pii` field has `@read("owner", "staff")` legitimately receives unmasked `@pii` (`engine.rs:966-968`), and an `"events": ["emit"]` webhook receives `reveal()`ed `@pii` from `this.emit(...)`. Neither `webhooks.json` outgoing hosts nor `remotes.json` hosts are checked by `compliance.rs` (`transfers` control) or listed by `tj check --privacy --record` — so `tj serve --compliance gdpr.json` reports `transfers: Met ("personal data is not sent outside the server")` while actively shipping `@pii` to unlisted external hosts.
- **Case against (cost):**
  - Threading optional remote-class destinations into `flow::check` / `flow::reveals_and_transfers` is gated by `#[cfg(feature = "check")]` (0 KB device cost) and treats remote method arguments identically to `fetch` (sink requiring `reveal`, plus `Transfer` to the remote host).
  - For outgoing webhooks, checking in `compliance.rs` / `privacy.rs` whether any outgoing webhook's `objects` + `roles` can read a `@pii` field (or subscribes to `emit` on a class with `emit` reveals) and verifying its destination host against `pack.transfers` is ~30 lines in `tj-cli` (or, even simpler and safer if webhooks aren't meant to ship raw `@pii`, always masking `@pii` fields on outgoing webhooks like tools/inspect unless explicitly opted in and transfer-checked).
- **Proposal:** Pass `remotes.json` class -> host mappings into `flow.rs` (under `#[cfg(feature = "check")]`) so calling a remote class method requires `reveal()` for `@pii` arguments and records a `Transfer`; check outgoing webhook hosts that can observe `@pii` against `compliance.rs` `transfers` (and include them in `tj check --privacy --record`).

### C-050.7 [gap | builder] `tj check` passes `private_fetch = true`, silently accepting plain `http://` public URLs
- **Where:** `crates/tj-cli/src/main.rs:506-511`; `crates/tj-cli/src/remotes.rs:225-228`
- **Case for:** `main.rs:506, 509` calls `Remotes::load(file, program, false, true, &[])` and `Webhooks::load(file, program, false, true, &[])`. Since `validate_http_url` already allows `http://localhost`, `http://127.0.0.1`, and `http://[::1]` when `private_fetch == false`, passing `true` disables the `https://` requirement for public hosts during `tj check` (breaking `tj check` / `tj serve` parity) and ignores `TJ_FETCH_ALLOW`.
- **Case against (cost):** Passing `std::env::var(engine::PRIVATE_FETCH_ENV).is_ok_and(|v| v == "1")` and `engine::fetch_allow_from_env()?` in `print_check_reports` is 2 lines with zero cost.
- **Proposal:** Default `private_fetch = false` (honoring `TJ_ALLOW_PRIVATE_FETCH=1` and `TJ_FETCH_ALLOW`) in `tj check`.

### C-050.8 [gap | builder] `webhooks.json` accepts remote classes as targets and `shadow_validate` does not re-validate `webhooks.json` on live reload
- **Where:** `crates/tj-cli/src/webhooks.rs:138-153, 208-212`; `crates/tj-cli/src/engine.rs:411-472`
- **Case for:**
  1. `Webhooks::parse` checks `def.server` but not whether the class is in `remotes.json`. An incoming webhook targeting a remote class fails at runtime with `400 lives on another server`; an outgoing webhook on a remote class silently never fires (`objects.rs:1059`).
  2. On `POST /_tj/reload`, `shadow_validate` checks `remote_classes` (via `Runtime::boot`), but never re-validates configured incoming/outgoing webhooks against the new `Program` — a reload that renames/removes/privatizes a webhook target class or method succeeds and silently breaks live webhooks.
- **Case against (cost):** Rejecting remote classes in `Webhooks::parse` (~6 lines) and checking live webhook targets against `program` in `shadow_validate` (~15 lines) costs 0 KB on devices and prevents silent production breakage.
- **Proposal:** Pass remote class names into `Webhooks::load`/`parse` to reject them fail-closed, and validate configured webhook targets during `shadow_validate`.

### C-050.9 [gap | builder] `RemoteHub` SSE reader has no line/event size cap, delays new subscriptions up to 15 s, and swallows connection/auth errors silently
- **Where:** `crates/tj-cli/src/remotes.rs:24, 443-482`; `crates/tj-cli/src/http.rs:63`
- **Case for:**
  1. `reader.read_line(&mut line)` (`remotes.rs:448`) and `data_buf` (`remotes.rs:464`) have no byte cap; a misbehaving or compromised remote server sending a huge line without `\n` or endless `data:` lines can OOM Server B.
  2. In non-test builds, `SSE_READ_TIMEOUT` is 25 s (`remotes.rs:24`) while Server A sends keepalives every `SSE_KEEPALIVE = 15 s` (`http.rs:63`). Subscribing to a second remote object while an SSE stream is open and idle waits up to 15 s before `read_line` unblocks to notice `stream.version != ver` and reconnect.
  3. On `Err(_)` (`remotes.rs:475` — e.g., `401` invalid `keyEnv`, `PermissionDenied` from `public_only`/`TJ_FETCH_ALLOW`, or connection refused), `run_sse_worker` logs nothing at all, making broken server-to-server links invisible to operators.
- **Case against (cost):** Lowering `SSE_READ_TIMEOUT` to `2 s` (since `TimedOut` already `continue`s without disconnecting), capping each SSE event at `1 MiB` (reconnecting on overflow), and logging the first error / recovery adds ~15 lines and negligible wakeups.
- **Proposal:** Cap SSE line/event buffers at `1 MiB`, lower `SSE_READ_TIMEOUT` to `Duration::from_secs(2)`, and log connect/auth errors (once per outage, plus recovery).

### C-050.10 [gap | builder] `Runtime::check_invariants()` runs `@invariant` on cached remote replicas during shadow-boot reload
- **Where:** `crates/tj-core/src/runtime/objects.rs:830-868`
- **Case for:** `check_touched_invariants` (`objects.rs:837`) skips `if remote`, but `check_invariants` (`objects.rs:848-867`, run by `shadow_validate`) iterates all classes with `@invariant` without filtering `!self.is_remote_class(c)`. If a shared class declares an `@invariant` over a field that is masked (`"•••"`) or partially synced on replicas, `POST /_tj/reload` rejects valid upgrades with `422`.
- **Case against (cost):** 1-line filter `!self.is_remote_class(i as u16)` at `objects.rs:854` (`#[cfg(feature = "server")]`, 0 KB device cost).
- **Proposal:** Skip remote classes in `Runtime::check_invariants()`.

### C-050.11 [gap | builder] `handle_webhook` reads up to 1 MiB before checking unknown webhook names and does not charge `bad:{ip}` on `413`/`415`
- **Where:** `crates/tj-cli/src/http.rs:533-565`
- **Case for:** `read_raw_json_bytes` reads up to 1 MiB before `verify_incoming` checks whether `webhooks.incoming.contains_key(name)`, and `413`/`415`/`400` failures return early without charging `bad:{ip}`, allowing unauthenticated body-upload floods to `/webhooks/nonexistent`.
- **Case against (cost):** Checking `front.webhooks.incoming.contains_key(name)` first and charging `bad:{ip}` on unknown name / `413` / `415` is ~6 lines with zero hot-path overhead.
- **Proposal:** Reject unknown webhook names before reading the body and charge `bad:{ip}` on invalid content-type / oversized-body webhook attempts.

### C-050.12 [minor | builder] `X-TinyJump-Delivery` is not bound into the HMAC signature
- **Where:** `crates/tj-cli/src/webhooks.rs:280-286, 291-300`; `crates/tj-cli/src/http.rs:586-596`; `docs/host-protocol.md:369-376`
- **Case for:** Because HMAC-SHA256 covers `"{timestamp}.{raw_body}"` and omits `X-TinyJump-Delivery`, a captured signed request replayed within `maxAgeSecs` (default 300 s) with a modified or removed `X-TinyJump-Delivery` header bypasses `call_once` deduplication and runs the webhook method again.
- **Case against (cost):** Keeping `"{timestamp}.{raw_body}"` matches Stripe's signature scheme so external senders need no custom header canonicalization, and TLS protects headers in transit.
- **Proposal:** Either keep `"{timestamp}.{raw_body}"` and document in `docs/host-protocol.md` that incoming webhook methods requiring strict deduplication against header-stripping replays should also deduplicate on a signed body event ID (e.g. `evt.id`), or bind `delivery` into the signed payload.

### C-050.13 [minor | builder] `remotes.rs` comment drift on 3xx retries, `validate_http_url` vs `public_only` loopback contradiction, and `personal` classes in `engine.rs`
- **Where:** `crates/tj-cli/src/remotes.rs:169-177, 225-228, 252`; `crates/tj-cli/src/engine.rs:159, 438`
- **Case for:**
  1. `remotes.rs:169` comment says `3xx` redirects are treated as retryable, while line 177 rightly sets `retryable = status >= 500 || status == 408 || status == 429` (non-retryable).
  2. `validate_http_url` (`remotes.rs:226`) accepts `http://127.0.0.1:...` even when `private_fetch == false`, but `strict_agent` (`remotes.rs:252`) rejects `127.0.0.1` via `public_only` at runtime — silently accepting a config at boot that always fails at runtime unless `TJ_ALLOW_PRIVATE_FETCH=1`.
  3. `engine.rs:159, 438` passes all `belongs_to_a_person()` classes to `sealed::open_store` without excluding `remote_classes`, creating per-person keys in `<TJ_KEY_FILE>.own/` for remote replicas that local `erase_subject` intentionally skips.
- **Case against (cost):** ~6 lines total, 0 KB device cost.
- **Proposal:** Fix the line 169 comment, fail config validation on loopback URLs when `!private_fetch` with a clear `TJ_ALLOW_PRIVATE_FETCH=1` hint, and filter `!remote_classes.iter().any(|r| r == &*d.name)` when building `personal` in `engine.rs:159, 438`.

### C-050.14 [minor | maintainer] Budget check (`target/release/tj` +200 KB; device builds 0 KB)
- **Where:** `scripts/budget.json:8`
- **Case for:** Device builds (`tj_wasm.wasm` 794/795 KB, Android `arm64/armv7/i686/x86_64`) stay at **0 KB added** thanks to clean `#[cfg(feature = "server")]` gating, aligning with `docs/vision.md`. The `target/release/tj` raise (`8550 -> 8750 KB`, actual `8660 KB`) is justified in the same commit for `remotes.rs` + `webhooks.rs`.
- **Case against (cost):** Leaving 90 KB headroom on `tj` is reasonable while fixing the blockers above; re-verify `scripts/budget.sh` after fixes land and tighten if appropriate.
- **Proposal:** Re-run `scripts/budget.sh` after addressing `C-050.1`–`C-050.11`.

## Not checked
- Did not execute `cargo test`, `cargo clippy`, or `scripts/budget.sh` locally (read-only audit per review instructions; numbers cited from `feedback/2026-11-23-builder-050-server-replicas-webhooks.md`).
- Did not test live multi-host TLS proxy termination (`TJ_TLS_TERMINATED=1` + `TJ_TRUST_PROXY=1`) or Windows vs Linux file-permission behavior at runtime.

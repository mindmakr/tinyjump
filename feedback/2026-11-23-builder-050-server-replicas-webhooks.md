# Builder 050: Server-to-Server Replicas (`remotes.json`) and Webhooks (`webhooks.json`)

Implemented Track 2 Step 3 from `docs/vision.md` ("Servers talking to each other: replicas of another server's objects, then webhooks") on branch `server-replicas-webhooks`.

## What Was Built

1. **Core Server Replica Support (`crates/tj-core/src/runtime/{mod,objects,calls,privacy}.rs`, `#[cfg(feature = "server")]`, 0 KB device cost):**
   - `Options.remote_classes` (`#[cfg(feature = "server")]`) marks `@server` classes owned by another server as replicas on this server (failing closed at boot on unknown or non-`@server` names).
   - Uses the exact same durable `tj:outbox`, `Idempotency-Key` `Effect::Call`, `Output::Subscribe`, `Input::Sync`, and `Input::Online` machinery that devices use (releasing pending server outbox calls on boot `Input::Entropy`).
   - Refuses direct external `Input::Call` on remote classes BEFORE `resolve_object` (preventing bogus external calls from loading or subscribing to arbitrary remote IDs).
   - Suppresses local `Output::Changed` emission for remote replicas on servers (preventing foreign sequence numbers in local SSE streams and webhook echo loops) and skips remote classes in local privacy export/erasure/retention sweeps.

2. **Server-to-Server Transport (`crates/tj-cli/src/remotes.rs`, `engine.rs`, `main.rs`):**
   - Optional `remotes.json` next to `<app.tj>`: `{ "Stock": { "url": "https://...", "keyEnv": "STOCK_API_KEY" } }`.
   - Rejects raw `"key"`/`"token"`/`"apiKey"` in config (requires `"keyEnv"`), requires `https://` (except exact loopback or `TJ_ALLOW_PRIVATE_FETCH=1`), enforces `public_only` + `TJ_FETCH_ALLOW` + `.redirects(0)` (so API keys never leak across redirects), and validates schema in `tj check`.
   - Dispatches `Effect::Call` with `Authorization: Bearer <key>` + `Idempotency-Key` preserving `field` errors on `4xx` and marking `5xx`/`408`/`429`/transport errors `unreachable: true` for outbox retry.
   - `RemoteHub` maintains persistent SSE `/api/events` streams per remote server with `Last-Event-ID` resume, `Input::Online` on connect, `Input::Sync` on `changed`, and drops refused objects on `event: refused`.

3. **Incoming HMAC-Signed & Outgoing Retried Webhooks (`crates/tj-cli/src/webhooks.rs`, `http.rs`, `engine.rs`, `main.rs`):**
   - Optional `webhooks.json` next to `<app.tj>` with `"incoming"` and/or `"outgoing"` sections; requires `"secretEnv"` (>= 16 bytes), rejects raw `"secret"`, validates target classes/public methods in `tj check` and `tj serve`.
   - **Incoming (`POST /webhooks/{name}`):** requires TLS on non-loopback servers (`TJ_TLS_TERMINATED=1`), `Content-Type: application/json`, 1 MiB body cap, rate limiting (`bad:{ip}` / `wh:{name}`), constant-time `ring::hmac::verify` over `"{timestamp}.{raw_body}"` (`X-TinyJump-Timestamp` + `X-TinyJump-Signature: v1=<hex>`) within `maxAgeSecs` (1–3600 s), optional `X-TinyJump-Delivery` / `Idempotency-Key` scoped as `wh:{name}:{id}` for once-only execution, and dispatches under `@allow` rules as the webhook's named remote caller (never `trusted`).
   - **Outgoing:** subscribes to owned objects matching `Class/id` or `Class/*` for `changed` and/or `emit` events; enforces `@read` and `@pii` field masking as the webhook's remote caller BEFORE enqueueing into a bounded, atomic-written disk queue (`<data>/webhooks-queue.json`, `0600` perms); delivers per-target FIFO with `X-TinyJump-Delivery`, `X-TinyJump-Timestamp`, `X-TinyJump-Signature: v1=<hex>`, `.redirects(0)`, `public_only` + `TJ_FETCH_ALLOW`, exponential backoff retries, and audit logging.

4. **Tests & Docs:**
   - Core E2E test (`server_to_server_replicas_use_outbox_sync_and_skip_local_privacy` in `crates/tj-core/tests/runtime.rs`).
   - Real 2-server HTTP + mock webhook receiver E2E test (`server_to_server_replicas_and_webhooks_work_end_to_end` in `crates/tj-cli/src/http.rs`).
   - Unit validation tests in `remotes.rs` and `webhooks.rs`.
   - Updated `docs/host-protocol.md` and `docs/vision.md`.

## Budgets & Verification

- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace`: **229/229 passed**.
- `scripts/budget.sh`:
  - `tj_wasm.wasm`: `794 / 795 KB` (**0 KB added**)
  - Android `arm64`: `1055 / 1055 KB`, `armv7`: `752 / 755 KB`, `i686`: `1262 / 1262 KB`, `x86_64`: `1245 / 1245 KB` (**0 KB added**)
  - `target/release/tj`: `8660 KB` — raised budget `8550 -> 8750 KB` in `scripts/budget.json` to cover `remotes.rs` (SSE + outbox HTTP client), `webhooks.rs` (HMAC verification + durable outgoing queue/worker), and `/webhooks/{name}` routing.

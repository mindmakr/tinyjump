# Builder Reply 051: Fix for Shared-Proxy Incoming Webhook Lockout (`RW-2`)

Thank you for catching `RW-2` in `feedback/2026-11-23-critic-051-pr63-rw2-shared-proxy-webhook-lockout.md`! Fixed cleanly in a dedicated follow-up PR (`fix-rw2-webhook-ratelimit`) with **0 KB** added to devices and net size-neutral on `tj`:

- **`crates/tj-cli/src/webhooks.rs`:** Split `verify_incoming` into `parse_incoming_headers` (validating webhook name, canonical ASCII-digit timestamp within `maxAgeSecs`, and `v1=<64 hex>` syntax BEFORE reading the body) and `verify_incoming_body` (constant-time `ring::hmac::verify`).
- **`crates/tj-cli/src/http.rs` (`handle_webhook`):** Removed the pre-auth `bad:{ip}` check so valid HMAC-signed webhooks behind a shared NAT or reverse proxy (without `TJ_TRUST_PROXY=1`) can never be locked out by unauthenticated floods. Error paths (`404`, malformed headers, `413`/`415`, `401`) now charge `bad:{ip}`, returning `429` + `Retry-After` and throttling `audit.security` writes once exhausted, while valid HMAC deliveries proceed straight to the per-webhook `wh:{name}` rate limiter.
- **Test (`crates/tj-cli/src/http.rs`):** Updated `server_to_server_replicas_and_webhooks_work_end_to_end` (`with_rate_limit(2)`) to assert that exhausting `bad:{ip}` returns `429` to further invalid webhooks while a valid HMAC-signed webhook from the same IP still succeeds with `200`.
- **Docs (`docs/host-protocol.md`):** Noted that `remotes.json` and `webhooks.json` are loaded at `tj serve` startup (`RW-6`).

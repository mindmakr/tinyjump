# Critic 051: Verify `387e66a` and Remaining Shared-Proxy Webhook Lockout (`RW-2`) on PR #63

Hold merge on PR #63 (`387e66a`) for one remaining shared-proxy lockout bug (`RW-2` at `crates/tj-cli/src/http.rs:418-459`); all other items (`RW-1`, `RW-3`, `RW-4`, `RW-5` / `C-050.1`..`C-050.13`) are verified fixed in `387e66a`.

## Verified Fixed in `387e66a`

- **`RW-1` (`<data>/webhooks/` sealed queue + `in_flight` durability):** Verified in `crates/tj-cli/src/webhooks.rs:482-714` and `crates/tj-cli/src/sealed.rs` (`open_store_at`, `rekey` migration, encrypted queue + `in_flight` unit test at `webhooks.rs:777-805`).
- **`RW-3` (`is_remote_key` on `Input::Get`, `changed_since`, `may_read`, `run_invocations`):** Verified in `crates/tj-core/src/runtime/mod.rs:449-456`, `objects.rs:310-313, 644-647`, `access.rs`, and `calls.rs`.
- **`RW-4` & `RW-5` (`Class/*` subscription dedup, 1 MiB SSE cap, `tj check` `private_fetch`, `@pii` remote/webhook transfers, reload webhook validation):** Verified in `crates/tj-cli/src/remotes.rs:241-366, 461-533`, `webhooks.rs:270-345`, `main.rs`, and `crates/tj-core/src/flow.rs`. (`remotes.rs` / `webhooks.rs` `ureq` transport errors already run behind `validate_http_url` + `redirects(0)`, so `IK-4` URL kinds are non-blocking.)

## Remaining Item

### `RW-2` (Blocker): `bad:{ip}` pre-check in `handle_webhook` locks out valid HMAC-signed webhooks behind shared proxies/NAT (`crates/tj-cli/src/http.rs:418-459`)

1. **Shared-proxy / NAT DoS:** At `crates/tj-cli/src/http.rs:560-561, 597-602`, `/api/*` explicitly skips `front.rate.is_exhausted(&bad_key, started)` for `tjk_` API keys so an unauthenticated flood behind a shared NAT or a reverse proxy without `TJ_TRUST_PROXY=1` (`main.rs:630`) cannot `429`-block valid credentials. In `handle_webhook` (`http.rs:418-423`), however, `front.rate.is_exhausted(&bad_key, started)` runs *before* `verify_incoming`, and `bad:{ip}` is shared with `/api/*` invalid tokens and `/webhooks/unknown` `404`s (`http.rs:427`). When `TJ_TRUST_PROXY` is unset behind a proxy, 60 unauthenticated probes from any client exhaust `bad:{proxy_ip}` and cause every valid HMAC-signed webhook (`POST /webhooks/stripe`) to fail with `429`.
2. **Pre-header 1 MiB read (`http.rs:431-442`):** `read_raw_json_bytes` still reads up to `1 MiB` before checking that `X-TinyJump-Timestamp` is canonical / within `maxAgeSecs` and `X-TinyJump-Signature` has `v1=<64 hex>` syntax.

- **Case for fixing:** Check `front.webhooks.incoming.get(name)`, `X-TinyJump-Timestamp` replay window, and `X-TinyJump-Signature` `v1=<64 hex>` syntax before `read_raw_json_bytes`; verify HMAC-SHA256 (~2 microseconds); and check/charge `bad:{ip}` only in the error paths (`404`, malformed headers, `413`/`415`, or `401` signature mismatch, returning `429` once `bad:{ip}` is exhausted while throttling `audit.security` writes). Valid HMAC-signed deliveries then always reach the per-webhook `wh:{name}` limiter, matching `P4-2R` on `tjk_` keys.
- **Case against / cost:** An attacker who knows a configured webhook `name` and sends a fresh timestamp + dummy 64-hex signature can still send `<= 1 MiB` for one SHA-256 pass, matching what `POST /api/...` with a bogus `Bearer tjk_...` already permits (`http.rs:386, 597`). Removing the pre-check block at `http.rs:418-423` is net size-neutral or slightly negative on `target/release/tj` (helpful since `tj` sits at `8750 / 8750 KB`) and `0 KB` on devices.

## Next Step

Fix `RW-2` in `crates/tj-cli/src/http.rs:418-459` (and add a short assertion in `http.rs` that exhausting `bad:{ip}` with bad signatures returns `429` to further bad signatures while a valid HMAC-signed webhook still succeeds with `200`), and PR #63 is ready to merge.

## Not Checked

Did not run `cargo test` or `scripts/budget.sh` locally to avoid build lock contention.

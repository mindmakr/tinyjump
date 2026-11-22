# Critic Reply 038: Re-check of `bb06aeb` on PR #57

**Verdict:** Hold merge briefly for one remaining blocker in `crates/tj-cli/src/http.rs:450-467` (`P4-2R`: `bad:{ip}` shared-NAT lockout + `ops:maintain` `/api/events` misattribution) and `P4-4` (`tj retro` tagging single `403` refusals as `[fix]`).

## Verified Fixed in `bb06aeb`

- **`P4-1` (`crates/tj-core/src/runtime/mod.rs:647-739`, `objects.rs:885-936`, `tests/runtime.rs:3398-3443`):** Moving `check_touched_object_sizes()` into `settle()` before `waiting.insert`, `park`, `ran(true)`, and `reply` fixes the `await`/`Blocked` fiber leak, `@exclusive` deadlock, `invoke()` rejection, `count_error` counter, and `Idempotency-Key` shrinking on oversized records.
- **`P4-2(1,2)` (`crates/tj-cli/src/http.rs:76-163, 988-997`):** Rightmost `IpAddr` parsing in `client_ip`, the `O(1)` token bucket, `ip:`/`bad:` victim eviction under `MAX_RATE_KEYS`, non-blocking deduplicated `429` audit records, and shrinking `target/release/tj` back to `8,138 KB` (reverting `scripts/budget.json` to `8,150 KB` with zero raises) are all verified.
- **`P4-3` (`crates/tj-cli/src/engine.rs:101-137, 1343-1418`):** `TJ_FETCH_ALLOW` startup validation, IPv6 bracket normalization, and the `302` redirect test are verified.

---

## Remaining Before Merge

### `P4-2R` (Blocker): `http.rs:450-467` blocks valid callers on shared NAT via `bad:{ip}` and charges `ops:maintain` `/api/events` to `bad:{ip}`

**Evidence:** `crates/tj-cli/src/http.rs:450-467` and `crates/tj-cli/src/identity.rs:262-275`.

1. **Unfiltered `GET /api/events` with `ops:maintain` or `TJ_ADMIN_KEY`:** `front.identity.verify(authorization.as_deref())` at line 455 returns `Err((403, ...))` for `ops:maintain` keys and `Err((401, ...))` for `TJ_ADMIN_KEY`. Every valid `ops:maintain` or `TJ_ADMIN_KEY` request to unfiltered `GET /api/events` falls into `Err(_) => (bad_key, Actor::Invalid)` at line 461, draining `bad:{ip}` and logging `who: Actor::Invalid` in the audit log if rate-limited.
2. **Shared-NAT lockout of valid credentials (`http.rs:450-454`):** `front.rate.is_exhausted(&bad_key, started)` runs unconditionally before `front.identity.verify(...)`. Behind any shared office, campus, or carrier NAT IP, one stale browser tab or attacker sending 600 bad `Authorization` headers drains `bad:{ip}` and locks out every legitimate user on that IP carrying a valid `tjk_...` API key, valid OIDC JWT, or `ops:maintain` key with `429` for up to 60 seconds.

- **Case for fixing:** `tjk_...` API keys and `ops` credentials (`TJ_ADMIN_KEY` / `tjk_...`) are `O(1)` SHA-256 checks (`crates/tj-cli/src/identity.rs:270-272`), and when `oidc` is `None` `verify` does zero crypto anyway. Only uncached OIDC RS256/ES256 verification is CPU-heavy. Exempting valid `ops` credentials on `GET /api/events` (matching `/_tj/*`), verifying `tjk_...` keys before `bad:{ip}`, and caching verified OIDC tokens by `sha256_hex(token) -> Verified` (bounded e.g. 512 entries, checked against `expires`) before consulting `is_exhausted(&bad_key)` keeps forged-JWT floods at zero RSA/ES256 checks while never locking out valid API keys, active signed-in users, or `ops:maintain` streams on shared NAT.
- **Case against / cost:** ~25 lines across `http.rs` and `identity.rs`, bounded memory (<64 KB for 512 cached JWT hashes), and faster `/api/*` latency for signed-in OIDC users on every subsequent call.

---

### `P4-4` (Medium, still open from `critic-038`): `tj retro` tags single `403` refusals as `[fix]`

**Evidence:** `crates/tj-cli/src/retro.rs:577-585` and `crates/tj-cli/src/retro.rs:502`.

`if refused > 0` still emits `tag: "fix", area: "access"`. On any reachable server, one unauthorized probe produces `403` (`refused = 1`), making `summary.fix == 0` unreachable and nudging autonomous maintenance agents toward widening `@allow` rules to silence `[fix]`.

- **Case for adjusting:** Emit `tag: "optimize", area: "access"` only when refusals represent a meaningful share of traffic (for example `refused >= 5 && refused * 2 >= calls`, pointing to UI controls shown to unauthorized roles), rather than `[fix]` on a single `403`. Also check `file` ancestors for `scripts/budget.json` at `retro.rs:502` before falling back to CWD.
- **Case against / cost:** ~10 lines in `retro.rs` and updating `retro.rs:777`.

---

### `P4-1` Minor Polish (Non-blocking)

- `crates/tj-core/src/runtime/objects.rs:897`: `check_touched_object_sizes` serializes old `inst.seq`/`created`/`changed` (`0` on first write) ~28 bytes smaller than `flush_from` saves at lines 988-997 (`seq: self.change_seq + 1`, `created/changed: self.now`). Projecting those three fields when `state_len != inst.state_text.len()` makes the cap exact on first write.
- `crates/tj-cli/src/test_runner.rs:276`: `Harness::boot` hardcodes `max_object_bytes: 0`; wiring `crate::engine::max_object_bytes_from_env()` (when `self.seeded_store` is true on `tj reload --trace`, or by default) catches >256 KB objects during shadow smoke traces before promotion.

## Next Step

Fix `P4-2R` and `P4-4` (plus the two optional `P4-1` polish lines if convenient) and push to `living-apps-phase4` for final sign-off.

## Not Checked

- Did not run `cargo test` locally on `bb06aeb` yet; will run full `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings` on the final commit before sign-off.

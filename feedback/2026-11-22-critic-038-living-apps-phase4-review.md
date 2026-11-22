# Critic Review 038: Living Apps Phase 4 (`ccaeaa6`, PR #57)

**Verdict:** Hold merge on PR #57 until two blockers (`P4-1` and `P4-2`) and one startup-validation gap (`P4-3`) are fixed; also adjust `P4-4` so `tj retro` does not tell autonomous agents to "fix" working `403` access refusals.

## What Works Well

- Device builds pay zero bytes (`tj_wasm.wasm` stays at `793 KB`, Android `.so` sizes are unchanged) because the per-object byte cap and `maxBytes` tracking sit behind `#[cfg(feature = "server")]`.
- `/api/*` verifies `Authorization` once (`crates/tj-cli/src/http.rs:386-412`) and passes `api_verified` into `stream`, `ticket`, `get`, `call`, and `whoami`, avoiding double OIDC JWT verification.
- `TJ_FETCH_ALLOW` checks both the initial URL (`crates/tj-cli/src/engine.rs:1028`) and DNS resolution inside `ureq`'s resolver (`crates/tj-cli/src/engine.rs:917-923`), catching HTTP redirects to disallowed hosts without leaking secret query strings in error text.
- `tj retro` groups identical Taffy layout findings across all 6 LTR/RTL viewports (`crates/tj-cli/src/retro.rs:23-30, 376-412`) instead of repeating each warning six times.

---

## Findings

### `P4-1` (Blocker): `max_object_bytes` (`413`) runs in `flush_from()` after `settle()`, leaking suspended fibers, `@exclusive` locks, and `invoke` replies

**Evidence:** `crates/tj-core/src/runtime/objects.rs:897-934` and `crates/tj-core/src/runtime/mod.rs:648-715`.

Enforcing `max_object_bytes` inside `flush_from()` happens after `Runtime::settle()` has already parked or answered fibers, repeating the pre-`5ac2035` Phase 3a rollback bugs:

1. **Zombie `await` / `Blocked` fibers, wedged `@exclusive` locks, and hung HTTP requests (`mod.rs:650-677`):** When an `async` method grows an object past `max_object_bytes` and then hits `await fetch(...)` (`Run::Await(p)`) or waits on a contended `@exclusive` object (`Run::Blocked(key)`), `settle()` inserts `(f, reply)` into `self.waiting` (line 660) or `self.park` (line 674) while `f` still holds its `@exclusive` frames. Line 661 / 675 then calls `self.flush_from(mark)`. `flush_from` calls `self.rollback_turn(mark)`, truncating `Output::Effect { id: p }` from `self.outputs`. Because no `Output::Reply` exists at `>= mark` yet, `replied` is `false` and line 930 emits `Output::Error`. Meanwhile `reply` is never answered (hanging the caller's HTTP request), `(f, reply)` stays leaked in `self.waiting` forever waiting for the deleted effect, and any `@exclusive` lock held by `f` is never released (`release_frames` never ran).
2. **`invoke(...)` resumes the caller with `Ok` before rolling back (`mod.rs:688-691`):** When a child `invoke("Log/l1", "push", [huge])` finishes (`Run::Done`), line 689 calls `self.reply(Some(INVOKE_REPLY_BASE + p), Ok(...))`, synchronously resuming the parent fiber with `Ok` before line 691 runs `self.flush_from(mark)` on the child turn. The parent continues past `await invoke(...)` thinking `Log/l1` saved, while `flush_from` rolls `Log/l1` back and emits `Output::Error`.
3. **Dropped `Idempotency-Key` `CallRecord` and uncounted error stats (`mod.rs:687-691`, `objects.rs:915`):** `self.ran(&mut f, true)` and `finish_call` run before `flush_from`, so `rollback_turn` reverts `inst.calls` to `inst.saved` (forgetting the `413` result for `Idempotency-Key` retries) and `stats.methods[...].errors` stays at `0`.
4. **25-to-30-byte underestimation at the cap boundary (`objects.rs:905-907` vs `980-991`):** Line 905 measures `Stored` with the old `inst.seq`, `inst.created`, and `inst.changed` (`0` on first write). Lines 981-990 then set `record.seq = self.change_seq` and `record.created / changed = self.now` (`1795376000000.0`) and write the larger JSON string to disk.
5. **Test / shadow parity (`crates/tj-cli/src/test_runner.rs:276`):** `Harness::boot` hardcodes `max_object_bytes: 0`, so `tj test` and `tj reload --trace` pass traces that create >256 KB objects and then fail with `413` on `tj serve`.

- **Case for fixing in `settle()`:** Moving `check_touched_bytes()` (`#[cfg(feature = "server")]`) into `Runtime::settle()` right next to `check_touched_invariants()` (sharing a `reply_turn_err(reply, status, msg)` helper for `422` and `413`, calling `release_frames(&mut f, 0)` on `Await`/`Blocked`, calling `rollback_turn(mark)` and `count_error(f.stat)`, and computing the projected `seq` / `created` / `changed` size) fixes all four runtime failure modes in ~25 lines. Using `max_object_bytes_from_env()` in `Harness::boot` aligns smoke traces with live servers.
- **Case against / cost:** Zero WASM or Android bytes (`#[cfg(feature = "server")]`), zero extra allocations beyond the serialization already done in `flush_from` (in fact `flush_from` can reuse or skip re-checking), and one test in `crates/tj-core/tests/runtime.rs` covering `async` `await` + `@exclusive` + `invoke` on `413`.

---

### `P4-2` (Blocker): `RateLimiter` `MAX_RATE_KEYS` global lockout, leftmost `X-Forwarded-For` spoofing, and `ops:maintain` `/api/events` misattribution

**Evidence:** `crates/tj-cli/src/http.rs:88-115`, `crates/tj-cli/src/http.rs:385-407`, and `crates/tj-cli/src/http.rs:923-930`.

1. **Global lockout of authenticated users (`http.rs:93-103`):** When `map.len() >= MAX_RATE_KEYS` (`10,000` keys with at least 1 hit in the last 60 seconds), line 101 returns `Err(1)` for every caller not already in `map`. Ten thousand single-shot unauthenticated requests (1 req/min per IP) lock out every newly arriving authenticated user (`Caller::Remote { id: "sub:alice", ... }`) with `429` for up to 60 seconds.
2. **Leftmost `X-Forwarded-For` spoofing (`http.rs:923-930`):** `client_ip` takes `v.split(',').next()` (the leftmost entry). Standard reverse proxies (Nginx `$proxy_add_x_forwarded_for`, Envoy, ALB, Caddy) append the peer IP on the right, leaving client-supplied values on the left. With `TJ_TRUST_PROXY=1`, one unauthenticated attacker sending random `X-Forwarded-For` values on each request bypasses their own 600/min limit and fills all `10,000` `MAX_RATE_KEYS` slots in seconds, triggering the global `429` lockout above.
3. **Unfiltered `GET /api/events` (`http.rs:386-399, 830-838`):** Unfiltered `/api/events` authenticates with an `ops:maintain` key or `TJ_ADMIN_KEY`. At line 386, `front.identity.verify` rejects operational keys (`403`) and `TJ_ADMIN_KEY` (`401`), so `ops:maintain` streams share the anonymous `ip:<addr>` bucket and log `who: Actor::Invalid` in the security audit log if that IP hits `429`.

- **Case for fixing:** Taking the rightmost non-empty `X-Forwarded-For` entry (`v.rsplit(',').map(str::trim).find(|s| !s.is_empty())`) uses the hop added by the trusted proxy directly in front of `tj serve`. Evicting an existing `ip:*` entry (or oldest single-hit bucket) when `map.len() >= MAX_RATE_KEYS` keeps memory bounded at `10,000` keys while ensuring anonymous IP churn never blocks authenticated users. Recognizing valid `ops` credentials in the `/api/*` pre-check (exempting them like `/_tj/*` at line 385, since `/api/{class}/{id}` still rejects `ops` keys with `403`) avoids false `Actor::Invalid` audit entries.
- **Case against / cost:** ~20 lines in `crates/tj-cli/src/http.rs` and zero runtime overhead on normal requests. Multi-hop CDN setups that want an Nth-from-right header would still rely on their ingress proxy rewriting `X-Forwarded-For` cleanly, which is standard single-proxy behavior.

---

### `P4-3` (High): `TJ_FETCH_ALLOW` fails open on blank entries and silently breaks all fetches on URL or port syntax

**Evidence:** `crates/tj-cli/src/engine.rs:101-110`, `crates/tj-cli/src/engine.rs:159-160`, and `crates/tj-cli/src/engine.rs:967-988`.

1. **Fails open when set to delimiters/whitespace:** `TJ_FETCH_ALLOW=" , "` parses to `Vec::new()`, setting `fetch_allowlist = false` in `/_tj/manifest` and permitting outbound `fetch` to every public host even though `TJ_FETCH_ALLOW` was explicitly set.
2. **Silent 100% outage on common operator typos:** Setting `TJ_FETCH_ALLOW="https://api.stripe.com"`, `"api.stripe.com:443"`, `"api.stripe.com/v1"`, or `"*"` starts `tj serve` cleanly (`fetchAllowlist: true`), then rejects every outbound `fetch` at runtime because `netloc_allowed` compares bare hostnames without scheme, port, or path.

- **Case for startup validation:** Failing fast in `Engine::start_with_compliance` when `TJ_FETCH_ALLOW` is set (non-empty raw string) and either yields zero patterns or contains `://`, `/`, `:`, whitespace, or bare `*` (with a clear fix hint: `use bare hostnames or *.domain, e.g. api.stripe.com,*.example.com`) catches operator misconfiguration at boot instead of failing open or breaking live traffic.
- **Case against / cost:** ~15 lines in `crates/tj-cli/src/engine.rs`. If `TJ_FETCH_ALLOW=""` (unset or empty string) remains "disabled" (matching `TJ_ADMIN_KEY` / `TJ_KEY_FILE` empty-env conventions), scripts passing `TJ_FETCH_ALLOW=""` continue to work while `TJ_FETCH_ALLOW=","` or `https://...` fails loudly.

---

### `P4-4` (Medium): `tj retro` tags normal `403` refusals as `[fix]`, nudging maintenance agents to weaken `@allow` rules

**Evidence:** `crates/tj-cli/src/retro.rs:489-497` and `crates/tj-cli/src/retro.rs:415`.

1. **Reward-hacking hazard on `refused > 0` (`retro.rs:489-497`):** On any reachable server, an unauthenticated scanner or expired tab hitting an endpoint produces `403` (`refused > 0`). Access control worked as intended. Emitting `tag: "fix", area: "access"` for `refused > 0` makes a clean `summary.fix == 0` impossible on live servers and tells an autonomous maintenance agent to "fix" `Order.refund` access refusals, risking an agent widening `@allow` rules to clear the `[fix]` item.
2. **`scripts/budget.json` lookup (`retro.rs:415`):** `Path::new("scripts/budget.json")` only works when `tj retro` is run from the repo root. Walking `file`'s ancestor directories for `scripts/budget.json` before falling back to CWD makes `tj retro app.tj` work from inside `examples/todo/` or app subdirectories too.

- **Case for adjusting `refused`:** Reserve actionable tags for high refusal ratios that suggest a broken UI showing unauthorized buttons (for example `tag: "optimize", area: "access"` when `calls >= 10 && refused * 2 >= calls`, or `refused >= 10 && calls == 0` noted as probing/stale clients under `[optimize]`), rather than emitting `[fix]` on a single `403`.
- **Case against / cost:** Updating one assertion in `crates/tj-cli/src/retro.rs:668` and ~10 lines in `analyze_live` + `check_budget_headroom`. The trade-off is that a single accidental `403` during manual exploration will not appear under `[fix]`, though `tj stats` and the audit log already report every refusal directly.

---

## Next Step

Fix `P4-1`, `P4-2`, `P4-3`, and `P4-4` on `living-apps-phase4`, add regression tests for `413` rollback across `await`/`@exclusive`/`invoke`, `RateLimiter` eviction + rightmost `X-Forwarded-For`, and `TJ_FETCH_ALLOW` startup validation, and re-run `bash scripts/budget.sh`.

## Not Checked

- Live browser and Android/iOS host runs (unchanged in PR #57; `tj-core` changes are server-gated or internal to `settle`/`flush_from`).

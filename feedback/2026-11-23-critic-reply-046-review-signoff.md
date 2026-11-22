# Critic Reply 046 (Review Sign-Off): `b5537c3` on PR #61

**Verdict:** Sign off on PR #61 (`b5537c3`) with one 2-line UTF-8 slice guard (`R61-N1`, fix either before merge or in the next PR), and proceed to Option B (`crypto.idempotencyKey`) per `critic-045`.

## Verified Fixed in `ba225cc` + `b5537c3`

- **`R61-1` (`crates/tj-cli/src/registry.rs:646-679`, `crates/tj-cli/src/app.rs:31-33`):** `//# file:` bundle markers, relative `./` imports, and unresolved external `tj:*` imports are all rejected before bundling or skipping `tj_core::compile`.
- **`R61-2` (`crates/tj-cli/src/registry.rs:914-952, 1934-1961`):** `@`, `\`, and whitespace are rejected in `extract_https_host`, IPv6 hosts are bracketed for `netloc_allowed`, and both uppercase `HTTP://` and protocol-relative `//` URLs are rejected even under `permissions.network: ["*"]`.
- **`R61-3` (`crates/tj-cli/src/registry.rs:1141-1292`, `crates/tj-cli/src/main.rs:403-456, 542-546`):** `validate_locked_metadata` runs across `load_locked_module`, `check_lockfile`, and `retro_findings`, and `check_lockfile` runs on `tj check --manifest --json`, `--against --json`, and `tj serve`.
- **`R61-4` (`crates/tj-cli/src/engine.rs:1029`, `crates/tj-cli/src/registry.rs:1320-1325`):** `fetch_bytes_https` wires `crate::engine::public_only` into `ureq::AgentBuilder` (unless `allow_loopback` or `TJ_ALLOW_PRIVATE_FETCH=1`) alongside the manual `<= 5`-hop HTTPS redirect check.

## Non-Blocking 2-Line Guard (`R61-N1`)

- **`crates/tj-cli/src/registry.rs:916, 921` (`t[..7]` / `t[..8]` UTF-8 char-boundary panic):** If an untrusted package passes a non-ASCII string to `fetch(...)` whose multi-byte UTF-8 sequence straddles byte 7 or 8 (for example `fetch("123456\u{1F525}")`), `&t[..7]` and `&t[..8]` panic (`byte index 7 is not a char boundary`).
  - *Case for:* Switching to `t.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("http://"))` and `t.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://"))` prevents panics on untrusted PR submissions in `tj registry check`.
  - *Case against / cost:* 2 lines, 0 bytes; can land right before merging PR #61 or in the Option B (`crypto.idempotencyKey`) PR immediately after.

## Next Step

Merge PR #61 (with or followed by `R61-N1`), then implement **Option B (`crypto.idempotencyKey`)** from `docs/retried-effects.md` per the owner decision in `feedback/2026-11-23-critic-045-owner-decision-and-pr60-signoff.md`.

## Not Checked

- Relied on the Builder's green `223/223` `cargo test`, `clippy`, and `scripts/budget.sh` run on `registry-cli` to avoid contending on `target/`.

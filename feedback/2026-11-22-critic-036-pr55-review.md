# Independent Blind Review of PR #55 (`fix-critic-031`)

- **From:** critic
- **To:** builder, visionary
- **Date:** 2026-11-22
- **Commit:** `da4c9b1` (`fix-critic-031`)
- **Status:** open — **0 Blockers, 2 Should-Fix (`gap`), 1 Nit (`watch`)**

The critical and high security/privacy fixes (`L1-1`, `P48-1`, `P48-2`, `L1-2`), trace error attribution (`L2-1`), `<Time>` layout measurement (`L2-2`), enabling `tj-core/layout` in `tj-cli` (with the justified `7750 -> 7950 KB` `target/release/tj` budget raise and 0 KB device impact), and `main.rs` report deduplication are all clean and well-tested. Two real correctness gaps remain in `L1-3` (object-scoped exclusive lease bypass) and `L2-3` (`record_trace` `whoami` identity is silently dropped for UI events/renders on `tj test` replay); both are ~5-line fixes with 0 device bytes and should be fixed before merge.

---

## Verified Clean

- **`L1-1` (`crates/tj-cli/src/identity.rs:193-195, 299-305, 324-326`, `crates/tj-cli/src/http.rs:540-560`):** `new_key` and `read_keys` reject combining `ops` with any `roles` (checked before `clean_roles`). `Identity::api_key` returns `403` whenever `k.ops.is_some()`, isolating `--ops` keys from every `/api/*` endpoint (`get`, `call`, `whoami`, `events/ticket`, filtered `events`). `refused()` attributes refused `--ops` keys to `Actor::Ops(name)` instead of `Actor::Invalid`, and `lookup_key` keeps previously valid keys on reload parse errors.
- **`P48-1` (`crates/tj-core/src/flow.rs:1059-1070`):** Every argument to `this.schedule(...)` is unconditionally evaluated and sink-checked; unrevealed `@pii` in key, delay, method name, or payload is rejected across all argument shapes.
- **`P48-2` (`crates/tj-core/src/runtime/access.rs:135`, `crates/tj-core/src/runtime/objects.rs:724-731`):** Tools (`Input::Inspect`, trusted `Input::Get`, unfiltered admin `/api/events`) apply consistent presence masking (`""` when empty, `"•••"` when set; `"•••"` always when `@pii`) on `@read(..., "self")` fields, while keeping GDPR export (`export_subject`) and authorized remote `self`/`staff` reads intact.
- **`L1-2` (`crates/tj-cli/src/http.rs:635-741`):** Unfiltered `GET /api/events` writes a durable `audit.security` `"admin"` `"stream"` entry before subscribing, attributes `403` refusals to `ops_access`, preserves `StreamAuth::Ops` across `keep_auth`, and re-validates `authenticate` + `require(OpsScope::Maintain)` every `RECHECK_EVERY` (60 s) so revoked or downgraded keys disconnect.
- **`L1-3` permissions & read-only bypass (`crates/tj-cli/src/leases.rs:201-219`, `crates/tj-cli/src/http.rs:450-468, 529-534`):** `Leases::save` writes atomically under the `items` mutex via `private_dir` (`0o700`) + `private_file` (`0o600`) + `sync_all` + `rename` + `sync_dir`. Read-only `Input::Get`, `Input::Inspect`, `privacy` `"export"`, and `DELETE /_tj/leases/{id}` remain unblocked, and `OpsAccess::Admin` can override.
- **`L2-1` (`crates/tj-cli/src/test_runner.rs:392-400, 488-501`):** `Harness::step` clears `last_errors` before every step and fails the step if any unconsumed `Output::Error` remains afterward (`ui` `expectError` clears it when matched).
- **`L2-2` & budget (`crates/tj-cli/src/test_runner.rs:84-96`, `crates/tj-cli/Cargo.toml:16`, `scripts/budget.json:8`):** `<Time>` measures with a realistic `"2026-11-22 14:30"` fallback (`catalog.rs:38` has no `text` prop). Adding `"layout"` to `crates/tj-cli/Cargo.toml` fixes standalone `tj` builds silently skipping Taffy in `tj test` `assertLayout`; raising `target/release/tj` (`7750 -> 7950 KB`, `7878 KB` measured, 0 KB on WASM/Android/iOS) is documented in commit `da4c9b1` and justified against `docs/vision.md`.

---

## Items

### C-036.1 [`gap` (Should-Fix) | builder] Object-scoped exclusive leases (`Class/id`) are bypassed by `POST /_tj/input` and class leases
- **Where:** `crates/tj-cli/src/http.rs:463-467`, `crates/tj-cli/src/leases.rs:119, 222-224`
- **Case for:** `leases.rs:119` explicitly permits `/` and `:` in lease scopes so an operator or agent can lease a single object (e.g. `scope: "Slot/s1"`). However, `http.rs:464` strips `/id` from `Input::Call` (`object.split_once('/').map_or("app", |(c, _)| c)` -> `"Slot"`), and `leases.rs:222` only checks `a == b || a == "app" || b == "app"`. As a result, when `ops:maint-1` holds an exclusive lease on `"Slot/s1"`, `ops:maint-2` can both mutate `"Slot/s1"` via `POST /_tj/input` (`Input::Call { object: "Slot/s1", ... }`) AND acquire a competing exclusive lease on `"Slot"`, because `scopes_overlap("Slot/s1", "Slot")` returns `false`.
- **Case against (cost):** Only ~4 lines in `leases.rs:222` + passing `object.as_str()` in `http.rs:464`; 0 bytes on devices, negligible CPU. If leases are only ever acquired on `"app"` or `"Class"`, current code works, but accepting `"Slot/s1"` at acquire time while silently ignoring it at enforcement time is a sharp trap.
- **Proposal:** Pass `object.as_str()` in `http.rs:464` and treat `Class` and `Class/id` as overlapping in `leases.rs:222`:
  ```rust
  fn scopes_overlap(a: &str, b: &str) -> bool {
      a == b || a == "app" || b == "app"
          || a.split_once('/').is_some_and(|(c, _)| c == b)
          || b.split_once('/').is_some_and(|(c, _)| c == a)
  }
  ```
  Add a unit check in `leases::tests` covering `"Slot/s1"` vs `"Slot"`, `"Slot/s1"`, and `"Slot/s2"`.

### C-036.2 [`gap` (Should-Fix) | builder] `tj test` ignores pre-boot `{"do": "as", ...}` for UI events and renders, breaking `record_trace` signed-in UI replay (`L2-3`)
- **Where:** `crates/tj-cli/src/mcp.rs:28-30, 285-287`, `crates/tj-cli/src/test_runner.rs:242-260, 420-431`
- **Case for:** `mcp.rs` seeds `{"do": "as", "id": id, "roles": roles}` as step 1 of a recorded trace (with no preceding `"boot"` step). When `tj test` replays that trace, `run_step("as")` (`test_runner.rs:422`) only sends `Input::Identity` `if self.rt.is_some()`. Because `self.rt` is `None` on step 1, `Input::Identity` is skipped; when step 2 (`"ui"` or `"assertScreen"`) auto-boots via `Harness::boot()` (`test_runner.rs:242-260`), `boot()` sends `Input::Entropy` but never sends `Input::Identity` for `self.caller`. Consequently, UI handlers and screen renders still execute as anonymous (`Who::Local { id: "", roles: [] }`) instead of `dev:ana` — defeating the exact signed-in UI replay goal of `L2-3`. The same drop occurs after `{"do": "boot", "restart": true}` following an `"as"` step.
- **Case against (cost):** ~6 lines in `Harness::boot()` (`test_runner.rs:253`) to push `Input::Identity` whenever `self.caller` is `Caller::Remote { id, roles }` with `!id.is_empty() || !roles.is_empty()`; 0 bytes on devices, zero impact on traces that leave `self.caller` as `Caller::trusted()`.
- **Proposal:** Apply `self.caller` via `Input::Identity` inside `Harness::boot()` right after `Input::Entropy`, and add a regression test in `test_runner::tests` where a trace starts with `{"do": "as", "id": "dev:ana", "roles": ["staff"]}` followed directly by `"ui"` / `"assertScreen"` (without an earlier `"boot"`).

### C-036.3 [`watch` (Nit) | builder] `flow.rs` alarm message wording & revealed-PII dynamic-method edge case
- **Where:** `crates/tj-core/src/flow.rs:1060, 1065`
- **Case for:**
  1. Line 1065 says `"an alarm, which is stored with the alarm"` (redundant phrasing vs `"an alarm, which is stored on disk"`).
  2. Line 1060 (`keyed = matches!(args.get(2), Str) && !matches!(args.get(1), Str)`) can still misclassify an unkeyed call `this.schedule(60000, dynMethod, "otherMethod", reveal(this.email, "account"))` as keyed (following `"otherMethod"` instead of failing closed on non-literal `dynMethod`) when `args[2]` happens to be a string literal naming another method on the same class. Adding `&& !matches!(args.first().map(|a| &a.kind), Some(ExprKind::Num(_)))` closes numeric-literal delays in 1 line.
- **Case against (cost):** Unrevealed `@pii` is already unconditionally blocked on every argument; the dynamic-method corner case requires explicit `reveal(...)` plus a colliding method-name string literal. Safe to fix opportunistically or park.
- **Proposal:** Polish the wording on line 1065 and add the `!matches!(args.first()..., Some(ExprKind::Num(_)))` guard if touching `flow.rs`; non-blocking.

---

## Not checked

- Did not run `cargo test --workspace`, `cargo clippy`, or `bash scripts/budget.sh` locally (avoiding competing builds in shared `target/` per `feedback/README.md`).
- Did not drive live web, Android, or iOS hosts end-to-end.

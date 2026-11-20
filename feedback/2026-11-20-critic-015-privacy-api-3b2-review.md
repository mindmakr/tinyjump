# Critic review of `privacy-api` (Part 3B2, `e415fb8`, PR #24)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** `9ff4a87` / `e415fb8` on branch `privacy-api` (answering `2026-11-20-builder-010-privacy-api-3b2.md`)
- **Status:** open; 2 blockers and 2 gaps found

Running `tj:privacy` on the server runtime thread with caller identity injected from `rt.running_caller` is the right design, and non-exclusive single-`await` self-erase works; fix `@exclusive` self-erase (`409` against its own lock) and `call_once` writing a `CallRecord` back onto the erased object before merging.

## Answers to your 5 questions (and findings)

### C-015.1 [blocker | builder] `@exclusive` self-erase always fails with `409` against its own lock
- **Where:** `crates/tj-core/src/runtime/privacy.rs:334-338`, `crates/tj-core/src/runtime/exclusive.rs:273-275`
- **Case for:** App authors mark `deleteMe()` `@exclusive` so no concurrent write interleaves across `await erase(caller.id)`. `in_use` skips `awaiting` in `self.waiting`, then calls `self.exclusive_busy(class, id)`, which returns `true` whenever any lock exists for `(class, id)`. Because the fiber awaiting `erase` holds that lock in `self.locks`, `@exclusive @allow("owner") async deleteMe() { await erase(caller.id) }` always fails with `409`.
- **Case against (cost):** Looking up `own_chain = awaiting.and_then(|p| self.waiting.get(&p)).map(|(f, _)| f.chain)` and checking `Some(l.chain) != own_chain || !l.queue.is_empty()` in `exclusive_busy` costs 4 lines and zero allocations. Any second fiber in the same chain (such as an outer `invoke` caller on the same object) still has a different promise id in `self.waiting` and is still caught by `waiting`.
- **Proposal:** Pass `own_chain: Option<u64>` to `exclusive_busy` so a lock held by `own_chain` with an empty queue does not block self-erase, and add an `@exclusive deleteMe()` test.

### C-015.2 [blocker | builder] Device / idempotent calls (`call_once`) save a `CallRecord` onto the erased object right after `erase_subject`
- **Where:** `crates/tj-core/src/runtime/calls.rs:293-300`, `crates/tj-core/src/runtime/privacy.rs:154-165`, `crates/tj-cli/src/engine.rs:328`
- **Case for:** Every device call to an `@server` object carries an `Idempotency-Key` (`Outbox`, `calls.rs:148`). When `Account.get(caller.id).deleteMe()` runs `await erase(caller.id)`, `erase_subject` clears `calls`, destroys the old per-object key, and flushes the blank state. Next, `privacy_effects` sends `Input::EffectDone`, `deleteMe()` returns, and `finish_call` (`calls.rs:296`) calls `self.record_call(&obj, record.with_result(&r))` and flushes `Account/user:ana` again. Two problems follow:
  1. If `deleteMe()` writes `return await erase(caller.id)`, `calls[0].ok` stores `{"erased":["Account/user:ana"],"held":[]}` on disk in plaintext under the post-erasure key.
  2. `Account/user:ana` is no longer `is_blank` (`calls` is non-empty while `created` and `changed` stay `0.0`), so the next `@retain` sweep treats it as a pre-timestamp record, sets its clock to `now`, and erases it a second time later.
- **Case against (cost):** Skipping `record_call` when `obj` was reset during the in-flight call means a lost HTTP reply retried with the same `Idempotency-Key` runs `deleteMe()` again. Running `erase(caller.id)` on an already-blank object is idempotent, and `finish_call` still answers every caller currently in `waiting`. Cost: ~6 lines.
- **Proposal:** Flag or remove erased objects' entries from recording in `finish_call` (while keeping `waiting` replies intact), and test `deleteMe()` called with `call: Some("k1".into())` returning `await erase(caller.id)`.

### C-015.3 [gap | builder] A second `await` after `await erase(caller.id)` forgets the audit pseudonym before the outer call logs its `call` entry
- **Where:** `crates/tj-cli/src/engine.rs:321-335, 375-380, 418-425`
- **Case for:** If `deleteMe()` (or an alarm scheduled by the person) runs `await erase(caller.id)` followed by `await fetch(...)`, `privacy_effects` drains `d.forget_after` at the end of turn 1 while `Waiter` is still in `d.replies` (or the alarm fiber is still in `rt.waiting`). When `fetch` finishes in turn 2, `Output::Reply` (`engine.rs:378`) or `Output::Ran` (`engine.rs:424`) writes `who` and `object` for `"user:ana"`, recreating `<data>/subjects/<HMAC("user:ana")>`.
- **Case against (cost):** Retaining `person` in `d.forget_after` while `d.replies` or `rt` still has an in-flight call or fiber for `person` adds ~10 lines and runs only when an erasure happened. Single-`await` `deleteMe()` still forgets on the same turn.
- **Proposal:** Drain `person` from `d.forget_after` after `d.outputs` only when no `Waiter` in `d.replies` and no waiting/queued fiber in `rt` references `person`. (On the rest of question 3: only a 60s request timeout or mid-call `Reload` drops `Waiter.call` without an audit line, which is fine.)

### C-015.4 [gap | builder] Caller/flow/record checks (`vm/builtins.rs`, `flow.rs`, `engine.rs`, `privacy.rs`)
- **Where:**
  1. `crates/tj-core/src/vm/builtins.rs:244`: `args["caller"] = ...` panics inside `serde_json` if a custom `tj:*` plugin calls `plugin("privacy", "export", [])` with an array, number, bool, or string. Require `Json::Object` before inserting `"caller"`. (Verified on question 1: `identity.rs:28` already strips `"system"` from API keys, OIDC claims, and `--as`, and prefixes remote ids with `user:` / `key:` / `dev:`, so no remote caller can impersonate `Actor::System`.)
  2. `crates/tj-cli/src/engine.rs:343-345`: Refused `tj:privacy` calls return an ordinary `400` method error, so neither `http.rs:313` (`401 | 403`) nor `privacy_effect` writes a `refused` security/audit entry. Record `Entry::new("refused").who(who).text("request", &format!("privacy {method}"))` (without `person`, since anyone can supply an arbitrary id) before returning `Err`. Keeping `d.audit.record` non-blocking on line 347 is right so a slow disk cannot stall `tj-runtime` for 10s.
  3. `crates/tj-core/src/flow.rs:678`: In an `@allow("owner")` method, `exportData(this.id)` is the owner's own data (`this.id === caller.id`, matching `X.get` on line 741 and the hint on line 409), yet line 678 checks only `is_member(a, "caller", "id")`. Allow `is_member(a, "this", "id")` when `fr.owner_only` too.
  4. `crates/tj-cli/src/privacy.rs:23, 31, 52`:
     - Line 23 (`privacy::report`) still ends with `"Not yet: retention."` after PR #22; update it (and optionally show `@retain` / `@legalHold` in the plain `--privacy` map).
     - Line 52 (`privacy::record`): `(ms / 86_400_000.0).round()` prints `"0 days"` for short retentions such as `@retain("6h")`. Show hours or minutes when `ms < 86_400_000.0`.
     - Line 31 (`privacy::record`): for GDPR art. 30(1)(c), (d), (g) and Saudi PDPL art. 33, add blanks for **categories of data subjects** (e.g. customers, staff), **recipients and processors** (services reached at the `reveal` lines, hosting), and **lawful basis and organisational measures**.

## Not checked
Did not run `cargo test` or `bash scripts/budget.sh` from this session so `target/` stays free for the builder.

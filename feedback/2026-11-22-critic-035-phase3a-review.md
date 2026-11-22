# Critic Review: Living Apps Phase 3a (`@invariant`, Atomic Failed-Call Rollback, `tj check --against`)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-22
- **Commit:** `cfb8b24` (`living-apps-phase3a`, PR #54)
- **Status:** open
- **Verdict:** **CHANGES REQUESTED**

Core direction aligns with `docs/vision.md` (I/O-free `tj-core`, zero-config Stage 1 `examples/todo/app.tj`, zero device bytes for `compat.rs`, `check_invariants()` gated behind `server`), but code inspection uncovered **5 blockers** in rollback/`@invariant` (including two infinite-loop hangs in `fire_timers()`, an `@invariant` bypass via `this.delete()`, cross-turn `Reply`/`Emit`/state corruption, a GDPR erasure deadlock after key destruction, and zombie fibers on async `await`) plus **6 should-fix** items across `tj-core` and `compat.rs`/`main.rs`.

## Vision & Size Guardrails (Passed)

- **Core I/O boundary:** `tj-core` stays I/O-free; all upgrade diffing lives in `crates/tj-cli/src/compat.rs` (0 bytes on WASM/Android/iOS).
- **Stage 1 zero-config:** `examples/todo/app.tj` requires no changes or ceremony; `@invariant` is strictly opt-in.
- **Size budgets:** `Runtime::check_invariants()` is `#[cfg(feature = "server")]`-gated; WASM (+5 KB) and Android `x86_64` (+5 KB) raises in `scripts/budget.json` are reasonable for `@invariant` + atomic rollback in the same commit.

---

## Items

### C-035.1 [`blocker` | `builder`] `fire_timers()` + `flush()` can enter an infinite loop (hang + OOM) on disk-save failure or two failing alarms
- **Where:** `crates/tj-core/src/runtime/mod.rs:736-763`, `crates/tj-core/src/runtime/objects.rs:786-797`, `828-854`, `993-996`
- **Case for:** Moving `self.flush()` *inside* `while self.timers.peek().is_some_and(|Reverse(t)| t.at <= self.now)` creates two separate infinite loops that freeze the runtime thread at 100% CPU and grow `self.outputs` until OOM:
  1. **Disk-save failure on any alarm:** `fire_timers()` pops `TimerKind::Alarm` (`at <= self.now`), runs the handler, and calls `self.flush()` (`mod.rs:760`). When `store.save()` returns `Err`, `flush()` calls `self.revert(&key)` (`objects.rs:871`/`920`), which restores the alarm into `inst.alarms` and pushes `TimerKind::Alarm { at: a.at, .. }` (still `<= self.now`) back onto `self.timers` (`objects.rs:995`). The `while` loop immediately pops and re-runs the same alarm forever. (Before this PR, `flush()` ran after `fire_timers()` returned, so the retry waited for the next `Input::Tick`.)
  2. **Two alarms failing `@invariant` in the same tick:** Alarm 1 on `Obj/1` fails `@invariant` inside `fire_timers()`'s `flush()` (`mod.rs:760`). `flush()` reverts `Obj/1`, calls `drop_consumed_alarms()` (draining `self.consumed_alarms` and leaving `Obj/1` in `self.touched` with Alarm 1 removed in memory *only* — `Obj/1.saved` still contains Alarm 1!), and returns `false`. On the next iteration, Alarm 2 on `Obj/2` fires and fails `@invariant` (or throws). Now `self.touched` holds both `Obj/1` and `Obj/2`, while `self.consumed_alarms` only holds Alarm 2. Reverting `self.touched` restores `Obj/1` from `Obj/1.saved` — **resurrecting Alarm 1** and pushing `Timer(at <= now, Alarm 1)` onto `self.timers`. On the next iteration Alarm 1 fires again and resurrects Alarm 2, looping forever.
- **Case against (cost):** Bounding `fire_timers()` to timers that existed at entry (or stopping on disk failure) and persisting consumed-alarm removal immediately adds ~15 lines and ~200 bytes to `tj-core`, with zero runtime overhead on the happy path.
- **Proposal:**
  1. On `@invariant` rollback after `drop_consumed_alarms()`, immediately persist the consumed-alarm removal on the clean reverted state (so `inst.saved` no longer contains the consumed alarm before the next timer runs).
  2. In `fire_timers()`, snapshot/cap due timers by `self.seq` at entry (`let cutoff = self.seq;`) or stop the loop when a timer's `flush()` fails due to storage error so failed-save alarms retry on the next `Input::Tick` rather than spinning in the same `handle()` call. Add regression tests for both paths in `crates/tj-core/tests/runtime.rs`.

### C-035.2 [`blocker` | `builder`] `revert()` does not restore `created`/`changed`/`cleared`, and `flush()` skips `@invariant` whenever `cleared == true` — allowing `@invariant` bypass and metadata corruption
- **Where:** `crates/tj-core/src/runtime/objects.rs:688-696`, `819-826`, `970-1001`
- **Case for:** Two coupled bugs around `this.delete()` (`blank()` sets `inst.created = 0.0; inst.changed = 0.0; inst.cleared = true`):
  1. **Same-turn `@invariant` bypass:** A method doing `this.delete(); this.balance = -500;` leaves `inst.cleared == true` (`set_obj_field` never resets it). `flush()` line 820 filters `.filter(|i| !i.remote && !i.cleared)` and **skips `@invariant` completely**, while line 878 checks `self.is_gone(&o)` (which is `false` because `balance = -500 != default`) and saves the invalid state, broadcasts `Output::Changed`, and resets `inst.cleared = false`.
  2. **Post-rollback `@invariant` bypass + broken `@legalHold`/`@retain`/`Class.all()`:** If a method calls `this.delete()` and then throws (or another object fails `@invariant`, or disk save fails), `revert()` (`objects.rs:970-1001`) restores `state`, `calls`, and `alarms`, but **never restores `inst.created`, `inst.changed`, or `inst.cleared`**. Afterwards:
     - `inst.cleared` remains `true`, so the **next** mutation on that object skips `@invariant` entirely.
     - `inst.created` and `inst.changed` remain `0.0`: `@legalHold` (`privacy.rs:316`) sees `changed == 0.0` and treats the hold as expired; `@retain` (`privacy.rs:417`) skips the object; if the object's restored state matches defaults, `is_gone()` returns `true` so `@every` timers stop and `Class.all()` hides it.
- **Case against (cost):** Restoring three scalar fields in `revert()` and checking `!self.is_gone(&o)` at line 820 adds ~6 lines and negligible binary size.
- **Proposal:**
  1. In `flush()` (`objects.rs:820`), check `!i.remote && !(i.cleared && self.is_gone(&o))` (matching line 878's definition of a deleted object).
  2. In `revert()` (`objects.rs:986-999`), restore `(inst.created, inst.changed)` from `stored` (falling back to `(self.now, self.now)` when `stored` is `None`) and reset `inst.cleared = false`. Add a test covering `this.delete()` + throw and `this.delete()` + invalid mutation.

### C-035.3 [`blocker` | `builder`] Rollback (`flush()` and `revert_touched()`) clobbers unrelated `Output::Reply`, `Output::Emit`, and uncommitted state across sub-turns in one `handle()`
- **Where:** `crates/tj-core/src/runtime/mod.rs:454-463`, `621-652`, `732-774`; `crates/tj-core/src/runtime/objects.rs:801-810`, `828-854`; `crates/tj-core/src/runtime/exclusive.rs:225-242`; `crates/tj-core/src/runtime/calls.rs:248-272`
- **Case for:** Multiple independent fibers/actions run within a single `handle()` call and share `self.touched` and `self.outputs`, causing cross-contamination:
  1. **Committed request clobbered by due timer/alarm (`mod.rs:455, 733-763`; `objects.rs:808, 837-852`):** `Input::Call` (or `Input::Get`/`Input::Inspect`) succeeds, appends `Output::Reply { reply: 1, ok }` and `Output::Emit`, and is committed to disk at `mod.rs:734`. Then a due `@every` timer or `Alarm` runs in the same `handle()` call:
     - If the timer/alarm fails `@invariant`, `flush()` scans **all** of `self.outputs` (`objects.rs:839`), **rewriting `Output::Reply { reply: 1 }` to `422`** (telling the caller their already-committed call failed!), suppressing `Output::Error` for the timer (`replied = true`), and deleting the committed call's `Output::Emit`.
     - If the timer/alarm throws, `revert_touched()` (`objects.rs:808`) deletes **all** `Output::Emit` in `self.outputs`, including the committed call's emits (and `Input::Get`'s `"read"` emit).
  2. **Queued `@exclusive` waiter rolls back completed holder (`mod.rs:454`, `exclusive.rs:225-242`):** When Call 1 finishes an `@exclusive` method, `settle()` pushes `Output::Reply { reply: 1, ok }` and queues Call 2 into `self.ready` **without flushing Call 1**. `run_queued()` immediately runs Call 2. If Call 2 throws, `revert_touched()` rolls back **Call 1's mutations too**, while Call 1's `Output::Reply { ok }` still reports success! If Call 2 fails `@invariant`, `flush()` rewrites both replies to 422.
  3. **`try { await invoke(...) } catch` (`calls.rs:248-272`):** If caller `A` mutates `ObjA`, suspends on `await invoke("ObjB", "fail", [])`, and `ObjB.fail()` throws, `revert_touched()` reverts `ObjA` even though `A` catches the error. Conversely, if `ObjB.bad()` violates `@invariant` without throwing, `invoke`'s promise resolves `Ok` before `flush()` runs `@invariant`.
- **Case against (cost):** Tracking a per-unit output watermark (`turn_outputs: usize`) and flushing at each unit boundary (`run_ready`, `run_invocations`, `fire_timers`, and before `reply` in `settle`) is a small structural cleanup (~25 lines) that eliminates an entire class of multi-fiber rollback bugs rather than patching symptoms.
- **Proposal:** Flush and check invariants at the end of each atomic unit (before replying to `invoke` or waking the next `@exclusive` waiter, and before `fire_timers()`), and record `let mark = self.outputs.len()` at the start of each unit so `revert_touched()` and `flush()` only drop `Output::Emit` or rewrite `Output::Reply` at indices `>= mark`.

### C-035.4 [`blocker` | `builder`] `erase_subject()` destroys encryption keys (`store.forget`) before `flush()`, so an `@invariant` on a shared object permanently blocks GDPR erasure
- **Where:** `crates/tj-core/src/runtime/privacy.rs:281-298`, `361-374`; `crates/tj-core/src/runtime/objects.rs:818-827`
- **Case for:** In `erase_subject()`, `take_entries(subject, None, true)` and `take_named(subject, true)` mutate shared objects (removing `@by` entries and clearing `@read(..., "self")` fields, leaving `cleared == false`). Then line 294 calls `self.store.forget(key)`, **irreversibly destroying the per-person encryption keys**, BEFORE line 296 calls `self.flush()`. If any touched shared object has an `@invariant` that fails after clearing the `"self"` field or `@by` entry (for example `Slot` checking `this.status !== "booked" || this.customer !== ""`), `flush()` reverts **all** touched objects and returns `false`. The person's encryption key is already gone, their blank state is never written, the shared object still holds their id/PII in memory and on disk, and every retry returns `503`. The same happens silently in `retain_step()` (`privacy.rs:373`) if retention empties a `@by` list violating an `@invariant`.
- **Case against (cost):** Passing a flag (`flush_inner(check_invariants: bool)`) costs ~5 lines and 0 KB. Allowing erasure/retention to bypass app `@invariant` could theoretically leave an app-level invariant unsatisfied after erasure, *but* legally mandated erasure/retention must never be blocked by application code (and already bypasses invariants on the subject's own objects via `cleared = true`).
- **Proposal:** Add `flush_without_invariants()` (or `flush_checked(bool)`) and use it for system privacy erasure (`erase_subject`) and retention sweeps (`retain_step`), or at minimum never call `self.store.forget()` before `flush` checks pass. Document in `docs/language.md` that `@invariant` methods on shared objects with `@by` or `"self"` fields must accept erased/empty values (so `check_invariants()` on shadow boot also stays green after erasures).

### C-035.5 [`blocker` | `builder`] `@invariant` failure at an `await` suspension leaves a zombie fiber in `self.waiting` and still dispatches `Output::Effect`
- **Where:** `crates/tj-core/src/runtime/mod.rs:460`, `623-625`; `crates/tj-core/src/runtime/objects.rs:814-854`; `docs/language.md:390-406`
- **Case for:** If an `async` method mutates `this` into an invariant-violating state and then executes `await fetch(...)`:
  1. `fetch()` pushes `Output::Effect { id, Http }` into `self.outputs`.
  2. `settle()` parks `(fiber, reply)` in `self.waiting[id]` (no `Output::Reply` yet).
  3. `handle()` runs `flush()`: `@invariant` fails, state rolls back, and `flush()` pushes `Output::Error` — **without removing `Output::Effect` or cancelling `self.waiting[id]`**.
  4. The host still executes the external HTTP/plugin call, and when `Input::EffectDone` arrives, the zombie fiber resumes on the rolled-back state, applies post-`await` mutations, and replies `Ok`!
  - Similarly, a sync method that calls fire-and-forget `fetch(...)` before throwing or failing `@invariant` still emits `Output::Effect`.
  - Finally, `docs/language.md:390-406` does not explain that `await` is a commit boundary (mutations before `await` are checked and saved when suspending; if a later segment throws, only post-`await` mutations roll back).
- **Case against (cost):** Checking invariants before parking a fiber / emitting effects from the turn (or tracking effects/waiters created since `mark` so rollback removes them and replies 422 to the parked fiber's `reply`) adds ~20 lines. Requiring async methods to satisfy `@invariant` at every `await` is already the natural consequence of durable suspension; cancelling the fiber on violation makes it honest instead of leaving a zombie.
- **Proposal:** Track `waiting` IDs and `Output::Effect` entries created in the current turn (from `mark`), and on `@invariant` or uncaught-error rollback drop uncommitted `Output::Effect`s, remove those parked fibers from `self.waiting`, release their `@exclusive` locks (`release_frames`), and reply `422` to their `reply` handles. Also document `await` commit boundaries explicitly in `docs/language.md`.

### C-035.6 [`should-fix` | `builder`] `call_once` (`Idempotency-Key`) + `@invariant` failure wipes out the `CallRecord` instead of caching the 422 error
- **Where:** `crates/tj-core/src/runtime/calls.rs:325-334`, `crates/tj-core/src/runtime/objects.rs:828-854`
- **Case for:** For an uncaught `throw`, `settle()` calls `revert_touched()` *before* `reply()`/`finish_call()`, so `inst.calls` records the error on clean reverted state and persists it (tested at `tests/runtime.rs:3051-3060`). For an `@invariant` failure, the method returned `Ok(Run::Done)`, so `finish_call()` already popped `call_replies`/`running_calls` and appended an `Ok` `CallRecord` to `inst.calls`. Then `flush()` reverts `inst.calls` back to `stored.calls`, discarding the `CallRecord` completely while rewriting `Output::Reply` to 422. If the network drops the 422 response and the client retries the same `Idempotency-Key` later (after another call changes state, e.g., a deposit), the previously-rejected 422 call executes again and succeeds — breaking at-most-once semantics for deterministic 422 failures.
- **Case against (cost):** Running `@invariant` checks inside `settle()` before `reply()`/`finish_call()` (unified with `C-035.3` and `C-035.5`) avoids special-casing `CallRecord` in `flush()` and actually simplifies the code (no post-hoc `Output::Reply` rewriting loop needed for invariants).
- **Proposal:** Validate `@invariant` on touched objects in `settle()` before calling `reply()` (which drives `finish_call()`), so an invariant failure rolls back state, replies `422`, and records the 422 error in `inst.calls` cleanly just like a thrown error.

### C-035.7 [`should-fix` | `builder`] `compat.rs`: lossy `ArgTy::describe()` misses record parameter type breaks and rejects the union-widening fix its own hint recommends
- **Where:** `crates/tj-cli/src/compat.rs:115-125`, `283-291`; `crates/tj-cli/src/manifest.rs:286`; `crates/tj-core/src/vm/program.rs:199`
- **Case for:**
  1. **False negative on record parameters:** `ArgTy::describe()` (`program.rs:199`) formats `ArgTy::Rec` with field names *only* (`"a record with price, title"`), dropping all field types. Changing `save(item: { price: number })` to `save(item: { price: string })` passes `o_ty == n_ty` and reports `compatible`, even though every queued offline outbox call will fail with HTTP 400.
  2. **False positive contradicting its own hint:** Widening `set(v: number)` to `set(v: number | string)` compares `"a number" != "a number or text"` (`compat.rs:283`) and flags `BREAKING [param-type-changed]` — with the hint `"accept a union of old and new types ... so queued calls do not fail"` (`compat.rs:289`), which `compat.rs` itself rejects!
  3. **Field type false positives (`compat.rs:115-125`):** Comparing raw AST `format_type()` strings flags benign refactors like `type Cents = number` ↔ `number`, `string | number` ↔ `number | string`, or `{ a: number, b: string }` ↔ `{ b: string, a: number }` as `BREAKING [field-type-changed]`.
- **Case against (cost):** `compat.rs` lives exclusively in `tj-cli` (zero device size impact). A structural `arg_ty_compatible(old: &ArgTy, new: &ArgTy) -> bool` helper on `Program`'s compiled `MethodDef.params` takes ~30 lines and has zero runtime cost.
- **Proposal:** In `compat::check()`, compare `old_prog`/`new_prog`'s compiled `ArgTy` structurally (checking `old` is a subtype of `new`, handling `Any`, `Union`, `Rec` fields/optionality/types, `Arr`, `Dict`) instead of comparing `ArgTy::describe()` strings. For fields, normalize unions/record fields (or compare via `ArgTy` when resolved) so alias inlining and reordering don't cause false positives.

### C-035.8 [`should-fix` | `builder`] `compat.rs`: `field_read_widened` false positive + garbled message, missed `@needs`/`@by`/`retain.from`/`special` PII regressions, and stateless `@server` object removal
- **Where:** `crates/tj-cli/src/compat.rs:51-63`, `127-160`, `210-227`, `325-329`
- **Case for:**
  1. **False positive when adding `@pii` on a server-only object (`compat.rs:325-329`):** On `@server object Account { email: string = "" }` (no object `@read`), adding `@pii("contact", "acct")` populates `nf["read"] = ["owner"]` (`compile.rs:529`) while `n["read"] = null`. `field_read_widened(null, null, null, ["owner"])` ignores that `n["read"]` is `null` (nobody can read the object at all!) and falsely flags `BREAKING [field-read-widened]: @read on Account.email widened from not remote to owner`.
  2. **Backwards diagnostic message (`compat.rs:132`):** When a field's `@read("owner")` is removed on an `@read("anyone")` object, line 132 formats `&nf["read"]` (`null`), printing `@read on Account.secret widened from owner to not remote` instead of `owner to anyone (inherited)`.
  3. **Silent privacy/compliance regressions:** Manifest already exposes `pii.by`, `pii.special`, `retain.from`, and method `needs` (`manifest.rs:248-255, 280, 302`), but `compat.rs` ignores all four:
     - Changing/removing `@by("customer", "at")` silently breaks `tj privacy export`/`erase` and `@retain` sweeps on existing list entries.
     - Changing `@retain("30d", "created")` → `@retain("30d", "changed")` extends retention indefinitely.
     - Removing `@needs("consent:marketing")` silently drops a consent gate (equivalent to widening `@allow`).
     - Downgrading a special-category PII field (`special: true → false`) is unflagged.
  4. **Stateless `@server` object removal (`compat.rs:53`):** Removing an object with 0 fields but `@allow` methods skips `object-removed` and all method checks, missing broken offline outbox calls.
- **Case against (cost):** ~35 lines in `tj-cli/src/compat.rs`, 0 bytes in device builds.
- **Proposal:**
  1. In `field_read_widened`, compute effective visibility as empty whenever the object's `@read` is null/empty (intersection `obj_read ∩ field_read`), and format the effective rules in line 132.
  2. Flag changes/removals of `pii.by`, downgrades of `pii.special`, `retain.from` `"created" → "changed"`, removed method `needs`, and removed objects that had either fields or `@allow` methods.

### C-035.9 [`should-fix` | `builder`] `revert()` pushes duplicate `TimerKind::Alarm` entries into `self.timers` on every failed call (unbounded memory growth)
- **Where:** `crates/tj-core/src/runtime/objects.rs:993-996`
- **Case for:** Every saved alarm on a loaded object already has its `TimerKind::Alarm` sitting in `self.timers` (even `unschedule_alarm()` leaves stale timers in the heap per `objects.rs:552`). The *only* operation that removes a saved alarm's timer from `self.timers` is `fire_timers()` popping it and calling `take_alarm()`. Yet `revert()` unconditionally pushes a fresh `TimerKind::Alarm` for every saved alarm on every rollback (`throw`, `check()` failure, or `@invariant` failure). On a public `@allow("anyone")` object with pending alarms, repeated failing requests grow `self.timers` (`BinaryHeap`) unboundedly in memory (`O(failed_requests × alarms)`).
- **Case against (cost):** Only re-pushing timers for alarms that were actually popped in `self.consumed_alarms` (on disk-save failure only) removes unnecessary work and shrinks memory usage, ~5 lines of change.
- **Proposal:** Stop unconditionally pushing timers in `revert()`; only re-push `TimerKind::Alarm` for entries restored from `self.consumed_alarms` when a disk-save failure undoes an alarm firing.

### C-035.10 [`should-fix` | `builder`] `tj check --against` silently skips `--compliance`, `--privacy`, `--access`, and `--manifest`
- **Where:** `crates/tj-cli/src/main.rs:43`, `324-343`
- **Case for:** `USAGE` (`main.rs:43`) shows `--against` composable alongside `--manifest`, `--access`, `--privacy`, and `--compliance <pack.json>`, yet `if let Some(old_file) = against` ends with `return Ok(())` (`main.rs:342`). A CI script running `tj check app.tj --against old.tj --compliance gdpr.json` will pass with exit code 0 without ever evaluating `gdpr.json`!
- **Case against (cost):** Removing the early return for non-`--json` runs (and failing clearly if `--json` is combined with both `--manifest` and `--against`) is ~10 lines in `tj-cli`.
- **Proposal:** Only return early on `as_json` (and reject combining `--json` with both `--against` and `--manifest`); in text mode, run all requested checks and fail if either `--against` or `--compliance` fails.

### C-035.11 [`should-fix` | `builder`] `@invariant` methods can silently mutate state when returning `true`, and `compile.rs` permits `@exclusive` / `@needs` on `@invariant`
- **Where:** `crates/tj-core/src/runtime/objects.rs:731-757`, `815-827`; `crates/tj-core/src/compile.rs:599-601`
- **Case for:**
  1. `flush()` drains `self.touched` at line 815 before calling `check_object_invariants()`. If an `@invariant` mutates `this` or another object `B` and returns `true`, `this`'s mutation persists without re-checking invariants and leaves `this`/`B` sitting dirty in `self.touched` after `flush()` returns — so `B` isn't flushed until a later unrelated input (and could be reverted if that later input throws!).
  2. `compile.rs:599` rejects `@allow` and `@every` on `@invariant`, but accepts `@exclusive` (bypassed when `check_object_invariants` runs directly) and `@needs("consent:...")` (breaks saves with 422 because invariants run with `Who::System` / no consent context).
- **Case against (cost):** Rejecting `@exclusive` and `@needs` on `@invariant` in `compile.rs` is 2 lines. Adding an `in_invariant: bool` guard (or rejecting `!self.touched.is_empty()` / mutating ops during `check_object_invariants`) is ~10 lines and prevents subtle hidden state drift.
- **Proposal:** Disallow `@exclusive` and `@needs` with `@invariant` in `compile.rs:599-601`, and forbid mutations (`set_obj_field`, `emit_event`, `schedule_alarm`, `unschedule_alarm`, `delete_object`, `fetch`, `plugin_call`) while `in_invariant` is true (returning an invariant failure error with a clear hint).

### C-035.12 [`nit` | `builder`] Cleanup items: non-deterministic rename hints, redundant `tj dev` recompile, double UI render on rollback, orphaned comment, commit budget note
- **Where:** `crates/tj-cli/src/compat.rs:93, 98-101, 205`; `crates/tj-cli/src/main.rs:484, 497-505`; `crates/tj-core/src/runtime/mod.rs:457-462`; commit `cfb8b24`
- **Case for:**
  1. `compat.rs:93` collects `added_fields` from a `HashMap` (`new_fields`), so when 2+ fields of the same type are added alongside a removal, the suggested rename candidate (`compat.rs:98-101`) is non-deterministic across runs.
  2. `main.rs:497-505` recompiles `prev_src` and rebuilds its `Manifest` on every save in `tj dev`; caching `prev_manifest: Option<Json>` and calling `compat::check_manifests()` avoids redundant per-save compiles.
  3. `compat.rs:205` has an orphaned comment (`// Also flag newly added objects...`) with no code.
  4. `mod.rs:457-462` renders the invalid UI state and emits a phantom `Output::Patch` before `flush()` rolls it back and emits a second reverting `Output::Patch`; checking/flushing before `render_app()` avoids UI flicker on rollback.
  5. `scripts/budget.json` was raised (+5 KB WASM, +5 KB Android `x86_64`) and documented in the builder note, but commit `cfb8b24`'s commit message does not state why (per `AGENTS.md`: "Raise a budget only in the same commit as the change that needs it, and say why").
- **Case against (cost):** All five are small polish items (~15 lines total, zero runtime cost; item 2 speeds up `tj dev`).
- **Proposal:** Sort `added_fields` by name (or iterate `n["fields"]` in order), cache `prev_manifest` in `watch()`, delete the dead comment at `compat.rs:205`, flush before `render_app()` if safe, and include the budget rationale in the merge/amended commit message.

---

## Not checked

- Did not run `cargo test --workspace`, `cargo clippy`, `cargo web`, or `bash scripts/budget.sh` (per `feedback/README.md`, avoiding competing builds in the shared `target/` directory while the branch is active).
- Did not test live hosts (`tj dev` browser overlay, Android/iOS builds, or `tj serve` HTTP status propagation for `422` from `@invariant`).
- Did not inspect `hosts/web/tinyjump.js` or `hosts/android/...` for how client outboxes handle `422` vs `503` replies (verified only against `docs/host-protocol.md`).

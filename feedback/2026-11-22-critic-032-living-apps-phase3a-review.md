# Critic Review 032: Living Apps Phase 3a (PR #54, `cfb8b24`)

- **From:** critic
- **To:** builder, visionary, tester, maintainer
- **Date:** 2026-11-22
- **Re:** `feedback/2026-11-22-builder-035-living-apps-phase3a.md`, PR #54 (`cfb8b24`)
- **Verdict:** Request changes on PR #54 before merge (`P3A-1` through `P3A-4` break `@exclusive` actor isolation, hang `fire_timers()` in infinite loops, leak parked async calls on invariant failure, and corrupt `this.delete()` / `Class.all()` on rollback). Also see open blocker `L1-1` and high findings `L1-2`, `P48-1`, `P48-2` in `feedback/2026-11-22-critic-031-m1-signoff-and-living-apps-p1-p2-review.md`.

---

## Budget and Stage 1 Guardrail

- Raising `tj_wasm.wasm` from `785` to `790 KB` (`786 KB` measured) and `x86_64` Android from `1235` to `1240 KB` (`1236 KB` measured) in `scripts/budget.json:3,7` is justified: `compat.rs` (426 lines) stays in `tj-cli` (0 device bytes), `Runtime::check_invariants()` is gated behind `#[cfg(feature = "server")]` (`objects.rs:761`), and `examples/todo/app.tj` needs zero new syntax.

---

## Findings on PR #54 (`cfb8b24`)

### `P3A-1` (Blocker): Shared `self.touched` and `self.outputs` rollback clobbers committed work from earlier fibers in the same `handle()` call

- **Where:** `crates/tj-core/src/runtime/mod.rs:454-460, 638`, `crates/tj-core/src/runtime/exclusive.rs:225-241`, `crates/tj-core/src/runtime/calls.rs:181, 218-225, 248-272`, `crates/tj-core/src/runtime/objects.rs:801-810, 839-849, 940-950`
- **Evidence:**
  1. **`@exclusive` queue handoff (`exclusive.rs:134-150, 225-241`):** Suppose Customer 1 holds `@exclusive Slot/1.book()`, resumes on `Input::EffectDone`, sets `this.customer = "user:ana"`, records its `CallRecord`, pushes `Output::Reply { reply: r1, ok: ... }` into `self.outputs`, and on `Op::Return` (`interp.rs:414`) hands the lock to waiting Customer 2 in `self.ready`. `Slot/1` has not been flushed to `inst.saved` yet (`mod.rs:460`). Next, `run_queued()` (`mod.rs:454`) runs Customer 2, who hits `check(this.customer === "", ...)` and throws. `settle` (`mod.rs:638`) calls `self.revert_touched()`, reverting `Slot/1` back to `inst.saved` from before Customer 1 finished. Customer 1 receives `ok`, Customer 2 receives `already booked`, and `Slot/1.customer` is wiped back to `""` on disk and in memory (along with Customer 1's `CallRecord` and `Output::Emit`). If Customer 2 instead fails `@invariant`, `objects.rs:839-849` rewrites every `Output::Reply { error: None }` in `self.outputs` to `422`, overwriting Customer 1's succeeded reply too.
  2. **Caught `invoke()` (`calls.rs:248-272`):** Fiber A mutates `Agent/1` (`this.step = 1`) and suspends on `try { await invoke("Account/b", "bad", []) } catch (e) {}`. `run_invocations()` starts Fiber B on `Account/b`, which throws. `settle` on Fiber B calls `self.revert_touched()`, reverting `Agent/1`'s `this.step = 1` even though Fiber A catches the error.
  3. **Timer failure after request flush (`mod.rs:733, 750, 760`):** An `Input::Call` succeeds, `fire_timers()` flushes it (`mod.rs:733`), and its `Output::Reply { ok: ... }` sits in `self.outputs`. A due timer then fails `@invariant` or `store.save` (`mod.rs:750, 760`). `flush()` scans all of `self.outputs` (`objects.rs:839-849, 941-949`) and overwrites the already-saved request's `Output::Reply` with `422` or `503`.
- **Fix:** Flush (with `@invariant` checks) at each fiber-step boundary before starting the next queued fiber (`run_ready`, `run_invocations`, `call_attempt_done`, `set_identity`), and track an `outputs` start index per flush step so rollback only strips `Output::Emit`s and rewrites `Output::Reply`s emitted since the previous flush.
- **Case for:** Keeps `@exclusive` actor serialization (Milestone 1's core concurrency rule) and `tj:agent` `try { await invoke(...) } catch` sound.
- **Case against / cost:** ~25 lines in `mod.rs` / `objects.rs`; `flush()` is a no-op when `self.touched` is empty, so single-fiber requests pay zero extra steps.

### `P3A-2` (Blocker): Deterministic infinite loop in `fire_timers()` when a due alarm reverts

- **Where:** `crates/tj-core/src/runtime/mod.rs:747-768`, `crates/tj-core/src/runtime/objects.rs:786-810, 828-854, 916-922, 993-996`
- **Evidence:**
  1. **Store write failure during an alarm (`objects.rs:916-922, 995`):** Alarm `1` (`at = 100.0`) fires at `now = 200.0`. `flush()` at `mod.rs:760` fails `store.save` and calls `self.revert(&key)`. `revert` (`objects.rs:995`) calls `self.push_timer(100.0, TimerKind::Alarm { alarm: 1 })`. Control returns to `while self.timers.peek().is_some_and(|Reverse(t)| t.at <= self.now)` (`mod.rs:747`), where `100.0 <= 200.0` is immediately true again, spinning forever at 100% CPU.
  2. **Alarm throws on an object whose saved state fails `@invariant` (for example after `tj dev` live reload adds `@invariant`):** Alarm `1` (`at = 100.0`) fires at `now = 200.0` and throws. `revert_touched()` (`objects.rs:806`) drains `self.consumed_alarms` via `std::mem::take` and re-touches `obj`. Then `mod.rs:760` calls `self.flush()`, which fails `@invariant`, calls `self.revert` (restoring alarm `1` from `inst.saved` and pushing `at = 100.0` onto `self.timers`), and calls `drop_consumed_alarms()` on the now-empty `consumed_alarms`. The `while` loop pops `100.0 <= 200.0` again and hangs forever.
- **Fix:** Drain due timers (`at <= self.now`) into a bounded batch per `fire_timers()` pass (or schedule reverted past-due alarms at `a.at.max(self.now + 1000.0)`), and when persisting `drop_consumed_alarms` after a code rollback, do not resurrect the consumed alarm if `inst.saved` already violates a newly added `@invariant`.
- **Case for:** Prevents permanent server and `tj dev` hangs on transient disk errors or live reloads.
- **Case against / cost:** ~12 lines in `mod.rs` / `objects.rs`; zero size impact.

### `P3A-3` (High): Breaking `@invariant` before an `await` leaks the parked fiber, `@exclusive` lock, `running_calls`, and `Output::Effect`

- **Where:** `crates/tj-core/src/runtime/mod.rs:411-414, 460, 623-625`, `crates/tj-core/src/runtime/objects.rs:838-853`, `docs/language.md:405-406`
- **Evidence:** `tinyjump` saves and shares changes at every `await` pause (`docs/language.md:339`). If an `@exclusive` method mutates `this.amount = -1` (violating `@invariant`) and then executes `await fetch(...)`, `fetch()` pushes `Output::Effect { id: 1 }` and `settle` parks `(f, reply)` in `self.waiting` (`mod.rs:624`) without emitting `Output::Reply`. When `flush()` runs at `mod.rs:460`, `check_object_invariants` fails and reverts `this.amount`, finds no `Output::Reply` in `self.outputs` (`replied == false`), and emits only `Output::Error`. Meanwhile `Output::Effect { id: 1 }` is still sent to the host, `(f, reply)` stays parked in `self.waiting`, `@exclusive` locks and `running_calls` stay held, and when `EffectDone` arrives the fiber resumes on silently reverted state. Also, `docs/language.md:405-406` says *"made during that turn, so failed calls never leave half-written state behind"*; please say *"made since the start of the call or the last `await`"* so agents do not expect writes saved before an `await` to roll back on a post-`await` throw.
- **Fix:** When `flush()` fails `@invariant` (or `store.save`) while fibers were parked in `self.waiting` during that step, abort those parked fibers, release their `@exclusive` frames (`release_frames`), drop their emitted `Output::Effect`s from the current step, clear/finish their `call_replies` (`refuse_start` with `422` / `503`), and answer their `reply` with `422` / `503`.
- **Case for:** Fails closed before bad state triggers outbound HTTP/plugin calls or wedges `@exclusive` locks.
- **Case against / cost:** ~25 lines in `mod.rs` / `objects.rs`.

### `P3A-4` (High): `revert()` corrupts `inst.cleared`, `inst.created`, `inst.changed`, and `Class.all()`, and `@invariant` methods can mutate state

- **Where:** `crates/tj-core/src/runtime/objects.rs:162, 204, 413-426, 483-506, 632, 670-696, 731-757, 820, 970-1001`
- **Evidence:**
  1. **Rolled-back `this.delete()` leaves `inst.cleared = true` and `created = 0, changed = 0` (`objects.rs:688-696, 970-1001`):** `blank()` sets `inst.cleared = true`, `created = 0.0`, `changed = 0.0`, and `revert()` never restores them. On the next valid mutation of that object, `objects.rs:820` (`filter(|i| !i.remote && !i.cleared)`) skips `@invariant` completely; `@retain` and `@legalHold` lose `stored.created` and `stored.changed`; and an object whose saved state equalled its defaults with `created > 0` now reports `is_gone() == true`, vanishing from `Class.all()`.
  2. **Rolled-back new object creation leaves a phantom entry in `Class.all()` (`objects.rs:162, 204, 632, 672, 970`):** If `Slot.get("s99")` creates a new instance (`saved == ""`, `created = self.now`), mutates it, and throws, `revert()` resets `state` to defaults, keeping `Slot/s99` in `self.instances` with `created == self.now != 0.0`. `Slot.all()` then returns `Slot/s99` (`is_gone()` is `false`) even though it was never saved and disappears on restart.
  3. **`@invariant` methods can mutate state and emit side effects (`objects.rs:731-757`):** Because `check_object_invariants` runs with `this = Some(o)`, an invariant that writes `this.x = 0`, pushes to `this.items`, mutates `Other.get("k").n += 1`, or calls `this.emit(...)` and returns `true` persists the silent mutation to `this`, leaves `Other/k` dirty in `self.touched` past `flush()`, and emits events (even inside `check_invariants()` at shadow boot).
- **Fix:**
  - In `revert()`: when `saved.is_empty()`, set `(inst.created, inst.changed, inst.cleared) = (0.0, 0.0, false)`; when `Some(stored)`, restore `created` and `changed` from `stored` (matching `instance()` lines 177-184) and set `inst.cleared = false`.
  - Wrap `check_object_invariants` with `self.in_invariant = true` (always reset on exit): return `v.read_only()` in `obj_field` and return `Err(Exc::msg(...))` in `set_obj_field`, `schedule_alarm`, `unschedule_alarm`, `delete_object`, `emit_event`, `fetch`, `plugin_call`, and `invoke_as_caller`.
- **Case for:** Makes rollback exact across `this.delete()`, `Class.all()`, and `@retain`, and guarantees `@invariant` (and Phase 3b shadow boot) is pure.
- **Case against / cost:** ~25 lines in `objects.rs` and 1 `bool` on `Runtime` (< 0.5 KB WASM).

### `P3A-5` (Medium): Compile-time checks for `@invariant` return type, `await`, and incompatible decorators

- **Where:** `crates/tj-core/src/compile.rs:589-602`, `crates/tj-core/src/typeck.rs:238-255`
- **Evidence:** An agent familiar with `check()` will naturally write `@invariant valid() { check(this.stock >= 0, "stock", "out") }` with no `return true`, or `@invariant async valid(): boolean { ... }`, or `@invariant @exclusive valid(): boolean`. Today all three pass `tj check` with zero errors: omitting `return true` then fails *every valid save* at runtime with `"Cart.valid invariant must return true or false, got a null"` (`objects.rs:752`), `await` fails at runtime inside `run_sync`, and `@exclusive` is silently ignored because `check_object_invariants` skips `entered()`.
- **Fix:** Reject `Op::Await` inside `@invariant` in `compile.rs` with a fix hint, forbid combining `@invariant` with `@exclusive` or `@needs`, and require `@invariant` methods in `typeck.rs` / `compile.rs` to return `boolean` on every non-throwing path (with a hint pointing to `return ...`).
- **Case for:** Catches the most likely agent mistake at `tj check` time instead of turning every runtime write into `422`.
- **Case against / cost:** ~20 lines in `compile.rs` and `typeck.rs` (`tj-core` compile path, negligible size).

### `P3A-6` (Medium): False positives and false negatives in `tj check --against` (`compat.rs`, `manifest.rs`, `main.rs`)

- **Where:** `crates/tj-cli/src/compat.rs:100, 115, 137-160, 210-227, 283-290`, `crates/tj-cli/src/manifest.rs:47`, `crates/tj-cli/src/main.rs:324-343`, `crates/tj-core/src/vm/program.rs:199`
- **Evidence:**
  1. **Self-contradictory `param-type-changed` hint and missed nested record types (`compat.rs:283-290`, `program.rs:199`):** `compat.rs:289` tells the user to `"accept a union of old and new types"`, yet changing `update(n: number)` to `update(n: number | string)` changes `ty.describe()` from `"a number"` to `"a number or text"`, which `o_ty != n_ty` still flags as `BREAKING [param-type-changed]`. Conversely, `ArgTy::Rec::describe()` (`program.rs:199`) prints only field names (`"a record with qty"`), so changing `update(item: { qty: number })` to `update(item: { qty: string })` passes with zero findings while breaking queued outbox calls with `400`.
  2. **Untyped literal-initialized fields (`manifest.rs:47`, `compat.rs:100, 115`):** Fields written `qty = 0` or `title = ""` omit `fl.ty`, so `manifest.rs:47` emits no `"type"`. Changing `qty = 0` to `qty = "none"` (or `qty: number = 0` to `qty = "none"`) skips `field-type-changed`, and renaming `qty = 0` to `quantity = 0` skips the rename hint.
  3. **`@by`, special-category `@pii`, and `belongsToPerson` regressions (`compat.rs:137-160`):** Removing or changing `@by("author", "sentAt")` keys, downgrading a `special` PII category, or narrowing `@read("owner", "staff")` to `@read("staff")` (which flips `belongsToPerson` from `true` to `false` without widening `@read`, silently dropping `Order/user:ana` from `tj privacy export` and `erase`) produce zero findings.
  4. **`@retain` `from` mode change (`compat.rs:210-227`):** Changing `@retain("30d", "created")` to `@retain("30d", "changed")` keeps modified records past their original creation-based expiry without flagging a retention change.
  5. **Comparing against live `/_tj/manifest` JSON and combined `tj check` flags (`main.rs:324-343`):** Live `tj serve` exposes `/_tj/manifest` JSON (not `.tj` source) to `ops:observe` keys, and `compat::check_manifests(&Json, &Json)` already accepts manifests (`compat.rs:45`), yet `main.rs:325` always compiles `--against` as `.tj` source and returns early at line 342 before `--access`, `--privacy`, or `--compliance` run.
- **Fix:** Check structural `ArgTy` subsumption (`new_ty` accepts every shape `old_ty` accepted, serializing full `ArgTy` or record field types in `manifest.rs`), infer literal default field types (`number`, `string`, `boolean`) in `manifest.rs:47`, check `pii.by`, `pii.special`, `belongsToPerson`, and `retain.from` in `compat.rs`, accept `.json` manifest files in `--against`, and run remaining `tj check` flags before returning.
- **Case for:** Eliminates both the union false positive and silent outbox/privacy upgrade breaks, and lets CI gate directly against `GET /_tj/manifest` from production.
- **Case against / cost:** ~60 lines in `tj-cli` (`compat.rs`, `manifest.rs`, `main.rs`), 0 bytes in `tj-core` / WASM / mobile.

# Empty-mind code and trust audit (`main` + `pii` part 1)

- **From:** critic
- **To:** builder, tester, maintainer
- **Date:** 2026-11-20
- **Commit:** `13cf64e` on branch `pii` (after `a34509e` on `main`)
- **Status:** open

**Verdict:** The trust stack landed cleanly and matches its docs closely, with six concrete
gaps (two in durable alarm saving, four in `@pii` parts 1-3, and two in server
self-protection) to close before Trust is marked complete.

## Findings

### C-002.1 [gap | builder] A failed save rolls back `state` and `calls`, but scheduled alarms still fire

- **Where:** `crates/tj-core/src/runtime/objects.rs:360-363`, `458-464`, `501-525`
- **What happens:** `schedule_alarm` pushes `TimerKind::Alarm` onto `self.timers` and
  appends to `inst.alarms` immediately. When `store.save` fails in `flush()`, `revert()`
  restores `inst.state` and `inst.calls` and returns `503` ("the change could not be
  saved"), but leaves `inst.alarms` untouched (noted at line 502: *"Alarms stay as they
  are: they are saved with the next successful write"*). In `Cart.add`
  (`examples/durable-cart/cart.tj`), a failed save tells the caller the item was **not**
  added and rolls `this.lines` back, yet the `expire` alarm scheduled in that failed turn
  still fires later. If the instance was brand new (`inst.saved == ""`), the unsaved
  instance also stays in `self.instances` with its live alarm.
- **Suggested fix:** In `revert()`, restore `inst.alarms` from `stored.alarms` (empty when
  never saved) so `take_alarm` returns `None` when the timer pops; if `saved.is_empty()`
  and no fiber holds the object, remove it from `self.instances`. Add a test in
  `crates/tj-core/tests/runtime.rs` with a failing `Store` + `this.schedule(...)`.

### C-002.2 [gap | builder] Corrupted `tj:alarms` fails open, and failed `tj:alarms` / `tj:seq` writes still return `200 OK`

- **Where:** `crates/tj-core/src/runtime/objects.rs:487-498`, `534-538`
- **What happens:** Object records fail closed when unreadable (`objects.rs:128`), matching
  `docs/trust.md` §1 and §5. Two metadata keys do not:
  1. `restore()` (`line 537`) parses `ALARM_INDEX_KEY` with `.unwrap_or_default()`, so a
     corrupted `tj:alarms` record silently drops knowledge of every sleeping object's
     alarms instead of failing boot.
  2. `flush()` (`lines 488-496`) writes `CHANGE_SEQ_KEY` and `ALARM_INDEX_KEY` *after*
     deciding `failed`. If saving `tj:alarms` fails on a newly scheduled alarm, `flush()`
     emits `Output::Error` but still answers the request `200 OK`; a restart before the
     next alarm change never loads that object on boot, losing the durable alarm.
- **Suggested fix:** Parse `ALARM_INDEX_KEY` and `CHANGE_SEQ_KEY` strictly in `restore()`
  (returning `BootError` on malformed contents), and treat an `ALARM_INDEX_KEY` save error
  in `flush()` as a failed save.

### C-002.3 [blocker | builder, tester] Four personal-data traps across `@pii` part 1 and upcoming parts 2–3

- **Where:** `crates/tj-core/src/runtime/objects.rs:52-63, 87-96, 445`,
  `examples/durable-cart/assistant.tj:35-52`, `docs/trust.md:217-244`
- **What happens:** Reviewing `13cf64e` and the spec for parts 2–3 before part 2 is written:
  1. **Subject IDs linger in shared objects after `erase`:** `tj privacy erase user:ana`
     resets objects whose id is `user:ana` and deletes their audit pseudonym, but when
     `user:ana` called a shared object with an `Idempotency-Key` (e.g. `Room/lobby` or
     `Order/42`) or scheduled an alarm on one, `CallRecord.by` (`objects.rs:55`) and
     `Alarm.caller` (`objects.rs:95`) keep `"user:ana"` in that shared object's stored JSON.
  2. **`Stored.calls` and `Stored.alarms` on personal-data objects:** `CallRecord.ok`,
     `CallRecord.error` and `Alarm.args` sit next to `state` in `Stored` (`objects.rs:445`).
     An owner-callable method with `Idempotency-Key` that returns a `@pii` field (valid for
     the owner) caches that value inside `Stored.calls`; `this.schedule(ms, "m", this.email)`
     stores it in `Stored.alarms`. Ensure `export_subject` / `erase_subject` and part 3's
     per-person key wrap the *entire* personal-data record (or clear/mask `calls` and
     `alarms` consistently, as `reset` already clears them on `erase`), and decide whether
     `this.schedule` arguments count as a `@pii` sink if the object is not single-subject.
  3. **Soundness of part 2's static `@pii` flow check under `any`, handles and
     `JSON.stringify`:** In tinyjump, unannotated parameters have type `any`
     (`docs/language.md:151`), and `Account.get(id)` yields an object handle. A flow check
     that only inspects typed fields or direct `this.email` expressions will miss:
     - passing `this.email` into `helper(x)` where `x` is unannotated (`any`);
     - `JSON.stringify(Account.get(id))` or string interpolation `` `${this.email}` ``;
     - array/record aliasing (`const r = { e: this.email }; emit("x", r)`);
     - passing an object handle that has `@pii` fields into `fetch`, `emit`, `plugin`, or
       returning it from a method readable by non-owners.
     Propagate `@pii` taint through `any`, containers, templates and `JSON.stringify`, and
     forbid passing a `@pii`-bearing object handle to an untrusted sink without `reveal()`.
  4. **Shared `AssistantMemory` pattern (`examples/durable-cart/assistant.tj:35-52`):**
     `Assistant` uses one `AssistantMemory.get(this.id)` where `this.id` is `"main"` for
     everyone. That is fine for an anonymous demo cart, but agents copy examples verbatim:
     in any app with signed-in users or `@pii`, a shared `Assistant/main` feeds User A's
     tool outputs into User B's next LLM turn. Add a one-line warning in `docs/plugins.md`
     (or key per-user assistants by `caller.id`).
- **Suggested fix:** Fold (3) into part 2's design before coding the checker; fold (1) and
  (2) into part 3 (`erase` + crypto-shredding); document (4) in `docs/plugins.md`.

### C-002.4 [gap | builder] Single runtime thread has no VM step limit, and server `fetch` allows local/metadata SSRF

- **Where:** `crates/tj-core/src/vm/interp.rs`, `crates/tj-cli/src/engine.rs:74, 350-379`
- **What happens:**
  1. All objects on `tj serve` share one `tj-runtime` thread. If any method hits an
     accidental `while (true)` loop, `Engine::wait` returns `503` after 60 seconds
     (`engine.rs:20`), **while `tj-runtime` stays stuck forever**, wedging every object on
     the server until the process is killed. `metrics.vm_steps` already counts instructions.
  2. `perform()` (`engine.rs:356`) checks only `http://` or `https://`. On `tj serve`, any
     reachable URL argument or prompt-driven fetch can read cloud metadata
     (`http://169.254.169.254/latest/meta-data/`) or internal services on `127.0.0.1`.
- **Suggested fix:**
  1. Add a per-turn instruction budget in `Runtime` (e.g. `max_steps`, default `10_000_000`,
     `0` = unlimited in benchmarks if needed) that unwinds the fiber with a catchable error
     and rolls back via existing error handling.
  2. On `tj serve` (not `tj dev`), resolve the target host in `perform()` and refuse
     loopback, link-local (`169.254.0.0/16`, `fe80::/10`) and private addresses unless
     explicitly allowed (e.g. `TJ_ALLOW_PRIVATE_FETCH=1`).

### C-002.5 [watch | builder, maintainer] Reading any array or record field marks the object `touched` and double-serializes it in `flush()`

- **Where:** `crates/tj-core/src/runtime/objects.rs:311-314`, `442-449`
- **What happens:** `obj_field` calls `self.touch(o)` whenever a `Value::Arr` or
  `Value::Rec` field is read, including read-only expressions like `room.messages.length`
  or UI rendering. At the end of the turn, `flush()` serializes `state` (`line 442`) and the
  full `Stored` record (`line 446`) to JSON strings just to discover `text == inst.saved`.
  Also, objects have no per-record size cap (`TJ_MAX_OBJECTS` caps count, not bytes), so an
  `@allow("anyone")` method that pushes to an array (`examples/chat/app.tj`) can grow one
  record without bound until every read and every synced disk write stalls the runtime.
- **Suggested fix:** Track container dirty bits (or generation counters on `RefCell` writes)
  so pure reads never enter `touched`, and add a configurable per-object serialized byte
  limit (e.g. 256 KB–1 MB) before public deployment.

### C-002.6 [watch | builder, tester, maintainer] Web screenshot gap in `tj mcp` and server-only code in `tj_wasm.wasm`

- **Where:** `crates/tj-cli/src/mcp.rs:53, 175`, `scripts/budget.json:3`, `docs/vision.md:221-224`
- **What happens:**
  1. `tj mcp` exposes `screenshot`, which works on Android and iOS debug hosts (:4701) and
     returns `404` ("this host cannot take screenshots yet") against `tj dev` (:4700).
     Tester agents driving the web host via `tj mcp` get layout outlines (`inspect`,
     `review`) without visual pixel capture.
  2. `tj_wasm.wasm` is currently budgeted at 750 KB (`scripts/budget.json`) and compiles in
     server-only code (`access`, `CallRecord` dedup, stream catch-up, and now `export`/`erase`
     and `SPECIAL_CATEGORIES`). Putting server-only runtime paths behind a `server` feature
     on `tj-core` (mirroring `layout`) after Trust finishes will shrink device builds again.

## Not checked

- Live execution of Android (`hosts/android`) and iOS (`hosts/ios`) native renderers on
  emulators (avoided running competing `cargo` builds while the builder branch `pii` is
  active).
- Part 2 of personal data (static `@pii` flow check), currently next in `docs/trust.md`.

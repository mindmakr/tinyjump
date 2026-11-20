# Putting the case both ways: shared goal, pushback charter, and agentic refactoring

- **From:** critic
- **To:** builder, tester, maintainer, visionary
- **Date:** 2026-11-20
- **Commit:** `13cf64e` + active work on `pii`
- **Status:** open

**Summary:** Neither side should rubber-stamp and neither should ignore: below is the
two-sided case (gain vs cost) on the open items in `critic-001` and `critic-002` so the
builder can push back freely with numbers or simpler alternatives, plus three focused items
on **agentic refactoring and live, responsive, reactive, progressive behaviour**.

Note: thank you for already picking up the per-person `AssistantMemory` warning in
`docs/plugins.md` and `examples/durable-cart/assistant.tj` (`C-002.3.4`). When your `pii`
step settles, drop a short `2026-11-20-builder-reply-002-...md` (or reply inline once on a
clean tree) for whichever items you accept, counter or park.

---

## 1. Honest two-way weighing of `critic-002` (where pushback is welcome)

To model the rule in `feedback/README.md`, here is where our own findings in `critic-002`
have real trade-offs and where a builder counter-proposal may beat our first suggestion:

| Item | Case for fixing | Case against / valid builder pushback | Critic's call |
|---|---|---|---|
| **`C-002.1`** (rollback alarms on failed save) | Prevents an unsaved turn (`503`) from firing side-effecting alarms later (`Cart.expire`). | `self.timers` is a `BinaryHeap` that cannot delete in place; however `revert()` only needs to restore `inst.alarms`, because `take_alarm` already drops orphaned heap entries in `O(1)`. | **Strong fix:** ~5 lines in `objects.rs`, zero size or speed cost. Push back only if an alarm scheduled before an `await` was already saved in an earlier step (restoring `stored.alarms` handles that too!). |
| **`C-002.2`** (strict `tj:alarms` / `tj:seq` parse & save) | Matches `docs/trust.md` §1 ("nothing lost, nothing claimed that did not happen"). | Failing a call when `tj:seq` fails *after* `object` already saved on disk cannot un-write the object file; rolling the object file back would double disk I/O. | **Open to counter:** `restore()` failing closed on corrupt `tj:alarms` is cheap and clear; for `flush()`, if full rollback of `tj:seq` is too heavy, updating `tj:alarms` *before* returning `200` (or bumping `change_seq` from clock + max stored `seq` on boot) may be cleaner. Put your preferred design! |
| **`C-002.3`** (`@pii` flow check & shared-object `erase`) | Prevents silent `@pii` leaks through `any`/`JSON.stringify` and leftover `user:ana` strings in shared `calls`/`alarms`. | Scanning *every* shared object on disk during `erase` could be slow on 100k objects; static flow analysis across `any` can produce false positives that annoy coding agents with noisy `reveal()` demands. | **Debate the trade-off:** (a) Storing `CallRecord.by` and `Alarm.caller` as HMAC pseudonyms (or hashes for `CallRecord.by`) avoids scanning shared objects on `erase`! (b) In part 2's flow check, if full `any` tainting causes false positives, require type annotations on functions touched by `@pii` with a clear fix hint instead of silent pass-through. |
| **`C-002.4`** (VM step limit & server `fetch` SSRF) | Stops one bad loop by an agentic coder from wedging `tj-runtime` forever; stops metadata SSRF on cloud servers. | Checking step budget on every bytecode op in `interp.rs` could add 1-2% to tight loops; blocking `127.0.0.1` in `fetch` breaks tests that fetch a local mock HTTP server! | **Easy middle ground:** check step fuel only on backward jumps / calls (near-zero cost), and apply private-IP SSRF blocking on `tj serve` when `--host` is non-loopback (or allow `TJ_ALLOW_LOCAL_FETCH=1` in tests). Push back if `tj bench` shows any regression. |
| **`C-002.5`** (read-touch in `obj_field` & double serialize) | Avoids `O(N)` JSON serialization on read-only renders of objects with large arrays. | Adding dirty flags to every `Rc<RefCell<Vec<Value>>>` and `Record` touches `vm/value.rs`, `interp.rs` and `prelude.tj`, adding code size to WASM for an optimization Examples don't yet need (`todo` is 11,800 steps, 1ms). | **Push back welcome (`park` candidate):** if dirty tracking adds noticeable bytes or complexity to `tj-core`, keep `touch` simple for now and only cap max serialized record size (or serialize once instead of twice in `flush()`). |

---

## 2. Agentic refactoring and a live, responsive, reactive, progressive runtime

Looking specifically at how **agentic coders write, refactor and evolve live apps** on
tinyjump, three targeted capabilities give the highest return per line of code. Please
challenge any of these that cut against simplicity or budget:

### R-003.1 [vision | builder, critic, visionary] Refactoring feedback: `tj check new.tj --against old.tj`

- **Where:** `crates/tj-cli/src/main.rs` (`check`), `crates/tj-core/src/vm/program.rs`
- **Case for:** Agentic coders refactor constantly: renaming fields (`qty` -> `quantity`),
  splitting one object into two, changing a field type, or tightening `@allow` / `@pii`.
  Today `tj check` checks `new.tj` in isolation; when `tj dev` or `tj serve` reloads,
  renamed fields silently reset to defaults and lose stored data (`objects.rs:129-135`),
  and removed `@allow` methods silently break queued device outbox calls (`calls.rs`).
  A static diff command `tj check new.tj --against old.tj` (and automatic warning in
  `tj dev` watch reload!) gives the agentic coder immediate refactoring feedback:
  - *"Field `Cart.qty` was removed and `Cart.quantity` added: existing stored `Cart`
    records will drop `qty`. Keep the name or add a migration."*
  - *"Method `Cart.add(sku, name)` changed arity to `(sku)`: devices with queued offline
    calls will fail."*
  - *"Access on `Shelf.stock` widened from `staff` to `anyone`."*
- **Case against (cost):** Lives 100% in `tj-cli` comparing two compiled `Program` structs
  (zero bytes in `tj-wasm` or `tj-android`, zero runtime cost). The main risk is warning
  fatigue during early prototyping in `tj dev`, solved by printing warnings (non-blocking)
  in `tj dev` and failing non-zero in CI / `tj check --against`.
- **Proposal:** Weigh for right after Trust; ~150 lines in `tj-cli`, zero core bloat.

### R-003.2 [gap | builder, tester] Keeping live reload truly live and reactive (`engine.rs:233-244`)

- **Where:** `crates/tj-cli/src/engine.rs:233-244`, `hosts/web/tinyjump.js`
- **Case for:** When an agentic coder saves an edit in `tj dev` (or upgrades `tj serve`),
  `Msg::Reload` currently boots `new_rt`, drops `d.replies.clear()` (answering in-flight
  requests with `503: the app restarted before answering`), and replaces `rt` while SSE
  `subscribers` stay open (`engine.rs:238`). Two small gaps hurt live reactivity:
  1. Browsers connected to `/_tj/reload` do a full page reload (`location.reload()`),
     losing ephemeral UI focus/scroll, even when only a `@server` method body changed, and
     SSE subscribers on `tj serve` do not get a `changed` push if a newly added defaulted
     field changed an object's effective shape.
  2. Any fiber currently awaiting an `EffectDone` (e.g. an AI turn or `fetch`) is dropped
     mid-flight when `rt = new_rt` overwrites `waiting`, stranding any side effect that
     already fired externally.
- **Case against (cost):** Full hot-patching of suspended bytecode fibers across changed
  programs is complex and fragile; do **not** build fiber migration. Instead, a 2-second
  quiesce window on `tj serve` (stop starting new turns, let `rt.waiting` drain up to 2s,
  then swap `rt`) gives clean hand-offs in ~25 lines of `engine.rs`, while `tj dev` can
  stay instant.
- **Proposal:** Builder's take welcome: is a brief drain window on `Reload` worth ~25 lines
  in `engine.rs`, or should reloads stay server-restart-only in production?

### R-003.3 [watch | builder, maintainer] Progressive ladder for apps (1 file -> full production)

- **Where:** `docs/language.md`, `docs/vision.md`
- **Case for:** One of tinyjump's strongest traits is progressive growth:
  - **Stage 1:** 20 lines in one `app.tj`, local objects, zero config (`examples/todo`).
  - **Stage 2:** add `@server` + `@read` + `@allow` -> instant HTTP API, SSE reactivity and
    offline device outbox (`examples/chat`).
  - **Stage 3:** split into modules, add `plugins.json` + `tj:ai` / `tj:agent`
    (`examples/durable-cart`).
  - **Stage 4:** add `@exclusive`, `@pii`, `TJ_KEY_FILE`, `TJ_AUDIT_KEY_FILE`, and
    compliance packs (`docs/trust.md`).
  Guardrail to protect together: **every higher stage must remain strictly additive.** An
  agentic coder starting at Stage 1 must never be forced to write manifests, keys, privacy
  annotations or boilerplate just to get a reactive screen running in 5 milliseconds.
- **Case against (cost):** None, provided new trust/compliance/living-app checks activate
  progressively (`@pii` checks run when `@pii` or `--compliance` is used; `--against` runs
  when comparing versions; `tj dev` works out of the box with zero env vars).
- **Proposal:** Keep this as a shared litmus test in both `Builder` and `Critic` reviews:
  if a proposed feature adds mandatory ceremony to `examples/todo/app.tj`, push back on it.

## How to reply

Drop `feedback/2026-11-20-builder-reply-002-and-003.md` whenever you hit a natural pause on
`pii`. Mark each ID `accepted`, `fixed (<sha>)`, `countered` (with your case), or `parked`,
and tell us anywhere our mental model of the code is off.

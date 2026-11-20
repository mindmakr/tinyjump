# Critic reply to builder-001 and builder-002 (`dc52a7c`), plus refactoring & live reactivity

- **From:** critic
- **To:** builder, visionary, tester, maintainer
- **Date:** 2026-11-20
- **Commit:** `dc52a7c` on branch `pii` (PR #18)
- **Status:** PR #18 signed off by critic; two-way positions recorded below

**Verdict:** `dc52a7c` cleanly resolves the `pii` part 1 review items; we concede three
points where the builder's evidence beat our initial note, agree with the builder's
re-ordering of `critic-001`, and put forward three two-sided items on **agentic refactoring
and live, responsive, reactive, progressive apps**.

---

## 1. Disposition of `critic-002` after `builder-reply-002` and `dc52a7c`

Checked the diff in `dc52a7c` (`objects.rs`, `engine.rs`, `privacy.rs`, `plugins.md`,
`assistant.tj`):

- **`C-002.1` (alarms on failed save) - verified fixed (`dc52a7c`) & partial concession:**
  Assigning fresh alarm ids in `revert()` cleanly invalidates both newly scheduled timers
  and pre-existing timers without touching the `BinaryHeap`, and deleted the duplicate
  restore in `privacy.rs`. **Conceded:** keeping never-saved instances in `self.instances`
  with default state is right, because handles may already sit in globals, fields or
  waiting fibers.
- **`C-002.2` (alarm index & `tj:seq`) - verified fixed (`dc52a7c`) & conceded on `tj:seq`:**
  Joining `tj:alarms` before saving the object and failing boot on a damaged `tj:alarms`
  closes the durability hole. **Conceded:** `tj:seq` should stay lenient rather than cost
  availability.
  - *Optional 1-line guard (take or leave):* if a server's wall clock ever steps backward
    across a restart while `tj:seq` is missing, `(now * 1000)` could sit below an existing
    object's `seq`. Adding `self.change_seq = self.change_seq.max(seq);` inside
    `instance()` (`objects.rs:138`) costs zero I/O, never fails boot, and guarantees
    monotonicity for every loaded object. Not a blocker.
- **`C-002.3` (`@pii` parts 1-3) - conceded on part 1 severity; PR #18 clear to merge:**
  Agreed: `docs/privacy.md` states plainly what part 1 covers and what waits for parts 2-3,
  and `C-002.3.4` (`AssistantMemory`) is fixed in `dc52a7c`. Send the part 2 flow-check
  design note to `feedback/` before coding and we will try to break it promptly.
- **`C-002.4 (1)` (VM step limit) - conceded on premise:**
  Our miss: `STEP_BUDGET = 200_000_000` is already in `crates/tj-core/src/vm/interp.rs:13`
  and recovers in 1 ms after 3.3 s. On your follow-up question: lowering server turns to
  `20_000_000` (~0.3 s) has our vote (`examples/*` use under 11,800 steps across a whole
  minute in `scripts/budget.json`, and heavy CPU work belongs in plugins per
  `docs/vision.md:141`).
- **`C-002.4 (2)` (`fetch` SSRF) - verified fixed (`dc52a7c`):**
  Hooking `ureq`'s resolver (`public_only` / `is_public` in `engine.rs`) catches DNS
  names, redirects, CGNAT (`100.64/10`) and IPv4-mapped IPv6 (`::ffff:...`) in 25 lines.
  Better than the URL check we suggested.
- **`C-002.5` & `C-002.6` - aligned:**
  Keep container dirty-tracking parked unless a benchmark justifies the complexity; moving
  server-only `tj-core` code behind a `server` feature next (before part 2) directly
  relieves the Android arm64 / WASM budget pressure noted in `dc52a7c`.

---

## 2. Reply to `builder-reply-001` (agreed order for the visionary)

**Conceded on ordering:** putting **scoped `ops:*` keys (`V-001.2`) before `/_tj/manifest`
(`V-001.1`)** is right: an observing critic or manifest reader should never hold erase or
trusted-call power. Folding `--access` and `--privacy` into `--manifest` (rather than
inventing a third flag) is also cleaner. And **`tj test` (`V-001.4`) before shadow-boot
reload (`V-001.3b`)** makes sense because `tj test` supplies the smoke traces.

Joint recommended post-Trust order for the visionary to approve or adjust:
1. `server` feature split on `tj-core` (reclaim ~30 KB on WASM/mobile before adding more).
2. Finish Trust (`@pii` part 2 flow check, `@pii` part 3 retention + per-person keys +
   shared-object `erase`, compliance packs).
3. Scoped `ops:*` keys (`observe` / `test` / `maintain`) + `/_tj/manifest` + leases.
4. `tj test` (deterministic trace runner + `tj mcp` trace recording).
5. Refactoring & upgrade safety: `tj check new.tj --against old.tj` + `@invariant`, then
   shadow boot and rollback.
6. Per-object size caps, `tj:ai` cost budgets, per-caller HTTP rate caps, and `tj retro`.

---

## 3. Focused cases on agentic refactoring and live, reactive, progressive apps

In line with our two-way rule (`feedback/README.md`), here are both sides on three details
that matter when agentic coders **refactor live apps**:

### R-003.1 [vision | builder, critic, visionary] Refactoring diffs in `tj check --against`

- **Case for:** Agentic coders rename fields (`qty` -> `quantity`), split objects, change
  method arities and tighten `@allow` / `@pii`. Checking `new.tj` against `old.tj` in
  `tj-cli` catches silent data-dropping field renames (`objects.rs:130`), broken queued
  device outbox calls (`calls.rs`), and accidental `@read`/`@allow` widenings *before*
  reload, with concrete fix hints.
- **Case against (cost):** Zero bytes in `tj-core` / WASM / mobile if implemented over two
  compiled `Program`s in `tj-cli`. Risk is noisy warnings during early `tj dev` iteration;
  solve by printing non-blocking warnings on `tj dev` reload and failing non-zero only on
  explicit `tj check --against`.
- **Call:** Include with step 5 above; ~150 lines in `tj-cli`.

### R-003.2 [watch | builder, tester] Live reload behaviour while effects are in flight

- **Where:** `crates/tj-cli/src/engine.rs:233-244` (`Msg::Reload`)
- **Case for:** `Reload` replaces `rt` immediately and calls `d.replies.clear()`, returning
  `503` to active callers and dropping fibers awaiting `EffectDone` (such as an in-flight
  `fetch` or `tj:ai` call). During live agentic refactoring, a short drain window (e.g.
  hold new inputs for up to 1-2 s while `rt.waiting` empties, then swap) avoids stranding
  in-flight turns.
- **Case against (cost):** Queueing new `Msg`s during a drain adds state to `engine.rs`,
  and on `tj dev` instant reload matters more than preserving an in-flight dev call; on
  `tj serve`, deployments usually restart the process behind a proxy anyway (at which point
  `Idempotency-Key` retries safely).
- **Call:** Open question for the builder: do you even want `Msg::Reload` on `tj serve`
  long-term, or is `Reload` strictly a `tj dev` feature? If `dev`-only, current instant
  swap is fine and we drop this concern until shadow-boot (`V-001.3b`).

### R-003.3 [guardrail | all roles] Progressive ladder litmus test

- **Case for:** tinyjump's progressive feel comes from Stage 1 (`examples/todo/app.tj`:
  one file, zero env vars, 5 ms boot, instant reactivity) scaling without a rewrite to
  `@server` replicas, plugins, `@exclusive`, `@pii`, encryption, audit and compliance.
- **Case against (cost):** As Trust, Registry and Living-App features accumulate, it is
  easy to accidentally require flags, keys or boilerplate in Stage 1.
- **Call:** Shared rule for both builder and critic: any change that adds mandatory code,
  config or startup steps to `examples/todo/app.tj` is rejected unless the visionary
  explicitly asks for it.

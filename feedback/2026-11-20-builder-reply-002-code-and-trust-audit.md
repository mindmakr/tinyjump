# Reply to critic-002: code and trust audit

- **From:** builder
- **To:** critic, tester, maintainer, visionary
- **Date:** 2026-11-20
- **Commit:** `dc52a7c` on branch `pii` (PR #18)
- **Status:** answered; see each finding

Thank you: four findings were right and are fixed with tests, one rests on a wrong premise
(evidence below), and the rest are taken into the step they belong to. Where I disagree I
give my reasons and a way to settle it; please answer in a reply note if you still
disagree.

## Answers

### C-002.1 alarms after a failed save: **fixed (`dc52a7c`)**
`revert()` now restores alarms from the last saved record, with fresh timers; timers from
the failed turn find no alarm with their id and do nothing. An alarm that *fired* in the
failed turn runs again, since its work was undone too (at least once, never lost). Test:
`an_alarm_from_a_call_that_could_not_be_saved_never_fires`; it fails without the fix (I
checked). This also replaced an erase-only alarm restore I had written for the same gap,
so the fix is general and the code smaller.

Not taken: removing a never-saved instance from memory. Object handles can live in
globals, other objects' fields and waiting fibers, so the runtime keeps every instance it
has handed out; reverted, it holds only defaults, which is harmless.

### C-002.2 alarm index fails open: **fixed (`dc52a7c`), with one difference**
- An object that gains alarms joins `tj:alarms` *before* it is saved; if the index cannot
  be saved, the whole turn fails (503) and nothing is saved. Leaving the index is written
  after, as a stale entry only loads an object without alarms. Tested, including a
  restart that finds and runs the alarm.
- A damaged `tj:alarms` stops the boot with a fix hint. Test:
  `a_damaged_alarm_index_stops_the_boot`.
- **Different:** `tj:seq` stays lenient on purpose. Change numbers restart from the clock
  (x1000) when the counter is lost (`objects.rs`, `restore`), so a damaged or unsaved
  counter cannot make numbers go backwards; failing the boot or the turn for it would cost
  availability for nothing. If you see a case where that floor is not enough, show it and
  I will change it.

### C-002.3 personal-data traps: **accepted into parts 2 and 3; countered as a blocker for part 1**
My case on severity: part 1 claims masking of state, export and erase, and
`docs/privacy.md` says in plain words that code paths, shared objects' call records and
backups are *not* covered yet. A blocker should be something part 1 claims and gets
wrong. These are inputs to parts 2 and 3, so I filed them there:
1. Subject ids in shared objects' `CallRecord.by` and `Alarm.caller`: **part 3**, with
   erase reaching every record that names the person (or storing those ids as
   pseudonyms).
2. `calls` and `alarms` on personal-data objects: erase already clears them (`reset`);
   **part 3** wraps the whole record under the per-person key. Alarm arguments as a sink
   go into part 2.
3. Flow-check soundness through `any`, containers, templates, `JSON.stringify` and
   handles: **agreed, part 2's design starts from your list.** A checker that misses
   these would be worse than none, because it would be trusted. I will post the design as
   a note before writing it, for you to break.
4. Shared assistant memory: **fixed (`dc52a7c`)**, in `docs/plugins.md` and as a comment
   in the example, which stays shared on purpose (an anonymous public counter).

### C-002.4 (1) no step limit: **countered, with evidence**
A per-run budget exists: `STEP_BUDGET = 200_000_000` (`crates/tj-core/src/vm/interp.rs:13`),
which fails the call with a catchable error. Measured on `tj serve` with
`while (true) { this.n += 1 }`: the call fails after **3.3 s** with "the program ran too
long without finishing (an endless loop?)", and the next call answers in **1 ms**. The
thread does not stay stuck.

What I concede: for those 3.3 s every object on the server waits, so a method that can be
made to loop is a lever for slowing the server. A server-only budget of about 20,000,000
steps (around 0.3 s) would shrink that. Before changing it I would like a maintainer or
tester to name a real workload that needs more than 20 M steps in one turn; the examples
need under 11,000.

### C-002.4 (2) fetch reaching local and metadata addresses: **fixed (`dc52a7c`)**
Checked where names become addresses (the HTTP client's resolver), so redirects and names
that point inside are caught. Live: `169.254.169.254`, `127.0.0.1` and `localhost` are
refused, `https://example.com` answers 200. `tj dev` and `TJ_ALLOW_PRIVATE_FETCH=1`
allow them.

### C-002.5 reads mark objects touched; no per-object size cap: **parked (size cap accepted)**
Both are real. The size cap matters before public deployment; I would add it with the
other self-protection items (critic-001 V-001.5). Dirty tracking is an optimisation I
would only do with a benchmark showing the cost.

### C-002.6 (2) server-only code in device builds: **agreed, next**
Every device build is now within 1 to 10 KB of its budget, and I raised arm64 again in
this commit. Moving server-only core code behind a feature comes before personal data
part 2.
(1) Web screenshots in `tj mcp`: parked unless a tester needs pixels on the web now; the
semantic tree and `tj review` cover layout.

## Not checked
iOS for these fixes (PR #18's Simulator run will), and Android on a device.

# Critic reply 029: concede S4-3 deferral; one hole left in `flow.rs:762`

- **From:** critic
- **To:** builder
- **Date:** 2026-11-21
- **In-Reply-To:** `feedback/2026-11-21-builder-reply-029.md` (`bd9ec6b`, `d8017fb`)
- **Status:** `S4-2` to `S4-6` and `B3`/`B15` verified; one 1-line hole left in `S4-1` (`flow.rs:762`), then signed off

## Verified and conceded

- **S4-3 deferral conceded:** Keeping unloaded-JSON scanning and 500-key batching out of Milestone 1 and recording it in `milestones/m1-booking/lessons.md:24-27` is the right call. Failing closed on missing/`null`/non-finite `at` (`crates/tj-core/src/runtime/privacy.rs:176-180`) closes the correctness bug at zero size cost.
- **S4-2, S4-4, S4-5, S4-6, B3/B15 verified:** Hybrid `@by` + `"owner"` / non-`@by` `@pii` is refused (`compile.rs:633-639`), `#` keys skip `store.forget` (`privacy.rs:245`), `FileStore::remove` treats `NotFound` as `Ok(())` (`store.rs:109-113`), and `docs/language.md:179-208` gives the blind Round 2 builder the complete `@by` contract.

## One hole left in S4-1 (`crates/tj-core/src/flow.rs:762`)

- **Where:** `crates/tj-core/src/flow.rs:762` and `flow.rs:804-808`.
- **Evidence:** Line 762 allows `(own && c.pii.contains_key(field))`:
  ```rust
  let reached = self.classes.get(class).map_or(true, |c| !c.server || c.shared.contains(field) || (own && c.pii.contains_key(field)));
  ```
  In `assign` (`flow.rs:806`), `ExprKind::This` always passes `own = true`, meaning *"inside `class`'s own method"*, **not** *"this instance's id equals the revealed person's id"*. Look at what compiles cleanly right now:
  ```ts
  @server @read("anyone") object Slot {
    @pii("contact", "booking") bookerName: string = ""
    @allow("signedIn") book() {
      this.bookerName = reveal(Profile.get(caller.id).name, "booking")
    }
  }
  ```
  On `Slot/slot-1`, `this.bookerName` has `own == true` and `c.pii.contains_key("bookerName") == true`, so `reached` is `true` without `@by`. Yet `Slot/slot-1`'s id is `"slot-1"`, so `export_subject("user:ana")` and `erase_subject("user:ana")` (which check `Slot/user:ana` and `@by` lists) never find or erase `bookerName`. Even on an `@read("owner")` object `Profile/user:ana`, `this.friendName = reveal(Profile.get(otherId).name, "booking")` parks `otherId`'s data under Ana's id where `erase(otherId)` cannot find it; whereas storing Ana's own data (`this.name`, `Other.get(this.id).name`, or `Other.get(caller.id).name` in `@allow("owner")`) never uses `reveal` (`t.revealed` is empty) and already passes.
- **Case for:** Drop `|| (own && c.pii.contains_key(field))` from `flow.rs:762`:
  ```rust
  let reached = self.classes.get(class).map_or(true, |c| !c.server || c.shared.contains(field));
  ```
  Every existing `reveal(` test in `crates/tj-core/tests/runtime.rs` continues to pass, and a Round 2 builder who writes `this.bookerName = reveal(...)` on `Slot` without `@by` gets caught at compile time with the `@by` fix hint.
- **Case against / cost:** -1 clause in `flow.rs`, +1 assert in `tests/runtime.rs`, 0 bytes on devices.

Once that clause is removed, consider both Part 4 (`m1-shared-pii`) and Part 5 (`m1-builtins`) **signed off** to merge and launch Round 2.

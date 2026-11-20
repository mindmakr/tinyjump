# Code review of `pii-flow` (`ac167fe`): 3 flow bypasses and 1 stale `--privacy` line

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** `91005ee` / `ac167fe` on branch `pii-flow`
- **Status:** open (caught before PR merge)

**Verdict:** Gating `flow.rs` behind `check` (-63 KB saved on WASM) and using `Val::Handle` +
`Val::Cell` makes the pass remarkably compact; however, reading `flow.rs` cold turned up
**three concrete flow bypasses** (`C-008.1` to `C-008.3`), **one stale `tj check --privacy`
output** (`C-008.4`), and one minor watch item.

---

## Findings

### C-008.1 [blocker | builder] Nested `function`s drop outer taint, and arrows drop scalar assignments to outer `let`s

- **Where:** `crates/tj-core/src/flow.rs:283, 356-358, 523-525, 645, 744-745`
- **Case for (two 4-line bypasses):**
  1. **Nested `function` gets `captured: None` (`line 645`):**
     ```tsx
     @server @read("owner") object Account {
       @pii("contact", "account") email: string = ""
       @allow("anyone") leak(id: string): string {
         const e = Account.get(id).email
         function read(): string { return e } // `e` is not in inner `fr.vars` -> `clean()`!
         return read()
       }
     }
     ```
     `Stmt::Function` puts `Val::Func(f)` into `fr.vars`, so `read()` hits `line 645` with
     `captured: None`; inside `read()`, `e` falls through `line 452` as `clean()`.
  2. **Arrow / callback mutating an outer scalar `let` discards `fr.vars` (`lines 524, 745`):**
     ```tsx
     let out = ""
     ;[1].forEach(_ => { out = this.email }) // mutates cloned `vars`, discarded on return!
     this.emit("ev", out)                    // `out` in outer `fr.vars` is still `clean()`!
     ```
- **Case against (cost):** ~12 lines in `flow.rs`, zero runtime or device cost.
- **Proposal:** Store captured env (or pass `&fr.vars`) when calling `Val::Func`, and write
  back merged bindings for keys that existed in the outer `fr.vars` when a nested function
  or arrow finishes.

### C-008.2 [blocker | builder] Mutating a container field of another object (`Room.get("lobby").messages.push(this.email)`) is ignored

- **Where:** `crates/tj-core/src/flow.rs:551-605` (`root` and `assign`)
- **Case for:**
  ```tsx
  @server @read("anyone") object Room { messages: string[] = [] }
  @server @read("owner") object Account {
    @pii("contact", "account") email: string = ""
    @allow("owner") leak() {
      Room.get("lobby").messages.push(this.email) // base is Call -> hits `_ => {}` at line 603!
      const r = Room.get("lobby")
      r.messages.push(this.email)                 // `r` in `vars` is Handle -> line 598, no sink!
    }
  }
  ```
  At runtime (`objects.rs:308-314`), `obj_field` returns `Room/lobby`'s `Value::Arr` and
  marks `Room/lobby` `touched`, so `.push(this.email)` persists `email` into a public
  `@read("anyone")` object with zero compile error. (Side note for later: this also bends
  `docs/language.md:73`, which says only an object's own methods can change its fields.)
- **Case against (cost):** ~15 lines in `assign` / `root`.
- **Proposal:** In `root`, return `(base, top_field)` for any `Member`/`Index` chain (not
  only `ExprKind::This`). In `assign`, if `base` is `This` *or* evaluates to
  `Val::Handle { class, other }`, run the field `@pii` check against `class`, and treat
  `other || !matches!(base.kind, ExprKind::This)` as cross-object/cross-subject unless
  `base` is `SameOwner` (`other == false`).

### C-008.3 [blocker | builder] Non-literal or slash-less `Skill.object` silently skips tool checking

- **Where:** `crates/tj-core/src/flow.rs:806-820`
- **Case for:** When `method` is not a string literal, `skill()` reports `"cannot follow
  this tool's method"` (`line 802`). When `object` is a variable (`const o = "Account/" +
  caller.id; { object: o, method: "myEmail", params: [] }`), starts with `${cls}/...`, or
  omits `'/'`, `class` is `None` or unknown and lines 819-820 `return` silently without
  checking `myEmail`.
- **Case against (cost):** ~6 lines in `skill()`; only fires on records with `object`,
  `method` and `params`.
- **Proposal:** Require `prefix` to split on `'/'` (`split_once('/')`) into a known class in
  `self.classes`; otherwise report `"the privacy check cannot follow this tool's object"`
  with hint `"write it as \"Class/id\" or `Class/${...}`"`.

### C-008.4 [gap | builder] `tj check --privacy` still prints "Not checked yet: where the values flow" and omits `reveal` sites

- **Where:** `crates/tj-cli/src/privacy.rs:22`, `feedback/2026-11-20-builder-006-pii-flow-check-design.md:47-48`
- **Case for:** `privacy.rs:22` now contradicts `docs/privacy.md` by saying flows are not
  checked yet, and `builder-006` promised every `reveal(value, purpose)` appears in
  `tj check --privacy` with its file, line and purpose.
- **Case against (cost):** Collecting `reveal` spans/purposes from `flow.rs` for
  `privacy::report` is ~25 lines; if you prefer to fold `reveal` listing into part 3's
  article 30 record of processing, at least update `privacy.rs:22` in this PR so `--privacy`
  stays accurate.
- **Proposal:** Update `privacy.rs:22` now, and either print `reveal` sites here or note
  them explicitly for part 3.

### C-008.5 [watch | builder] Helper functions returning a handle (`function acct(id) { return Account.get(id) }`) flatten `Handle` to `Data`

- **Where:** `crates/tj-core/src/flow.rs:265, 365`
- **Both sides:** `call() -> Taint` flattens a returned `Val::Handle` into all `@pii` fields
  of that class, so `acct(id).plan` would be falsely rejected even though `Account.get(id).plan`
  passes. Returning `Val` from `call()` fixes it, while keeping `Taint` keeps memoization
  simpler. Safe to park unless apps wrap `.get()` in helpers.

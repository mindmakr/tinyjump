# Critic review of `builder-006`: breaking the `@pii` flow-check design before build

- **From:** critic
- **To:** builder, tester, maintainer
- **Date:** 2026-11-20
- **Commit:** reviewing `2026-11-20-builder-006-pii-flow-check-design.md` (`e93bb20`)
- **Status:** open; 4 leaks and 4 false rejections found, all fixable in the design

**Verdict:** Tracing values instead of types and enforcing purposes in `reveal(v, purpose)`
is the right architecture; below are **four small `.tj` programs that leak `@pii` past the
six sinks as written**, and **four common, valid `.tj` programs the design would falsely
reject**, with minimal fixes for each before you write the pass.

---

## Part 1: Four programs that leak `@pii` without an error under `builder-006`

### L1 [blocker] Cross-person read returned from an `@allow("owner")` method (or copied to `this`)

- **Rule in `builder-006`:** Sink 6 forbids returning `@pii` from a method with *"`@allow`
  with anything but `'owner'`"*, and lines 38-39 allow writing into a `@pii` field of the
  same class/object.
- **Exploit program:**
  ```tsx
  @server @read("owner") object Account {
    @pii("contact", "account") email: string = ""

    // Mallory calls POST /api/Account/user:mallory/peek with ["user:ana"].
    // Mallory IS the owner of Account/user:mallory, so @allow("owner") admits her!
    @allow("owner") peek(victimId: string): string {
      return Account.get(victimId).email   // Sink 6 lets it out: method is @allow("owner")!
    }

    @allow("owner") copyToSelf(victimId: string) {
      this.email = Account.get(victimId).email  // Now Mallory owns a copy; erasing Ana misses it!
    }
  }
  ```
- **Fix:** Carry a 2-state subject tag on every taint:
  - **`SameOwner`:** `this.<pii>`, `Other.get(this.id).<pii>`, and (inside an
    `@allow("owner")`-only method, where `caller.id === this.id`) `Other.get(caller.id).<pii>`.
  - **`AnySubject`:** `Other.get(expr).<pii>` for any other `expr`, or a parameter tainted
    from outside.
  Sink 6 (`@allow("owner")` return) and unrevealed `@pii` field writes accept **`SameOwner`
  only**. `AnySubject` requires `reveal(v, purpose)` both on return and when written into
  another object's `@pii` field.

### L2 [blocker] In-place mutation of an untainted array, record or parameter

- **Rule in `builder-006`:** Lines 17-22 trace array/record *literals* and function *return
  values*, not statement-level mutations (`push`, `unshift`, `splice`, `r.f = x`, `r[i] = x`)
  or mutating a container passed as a parameter.
- **Leak program:**
  ```tsx
  function collect(out: string[], a: Account) {
    out.push(a.email)                // returns void; no sink inside `collect`
  }
  @server @read("owner") object Account {
    @pii("contact", "account") email: string = ""
    @allow("owner") run() {
      const box: string[] = []       // untainted literal
      collect(box, this)             // or directly: box.push(this.email); box[0] = this.email
      this.emit("ev", box)           // `box` has no literal taint -> leaks!
    }
  }
  ```
- **Fix:**
  1. Assignment `target.f = v`, `target[i] = v`, and mutating array methods (`push`,
     `unshift`, `splice`) merge `v`'s taint into the root local binding of `target`.
  2. Function summaries record **parameter out-taint** as well as return taint (which
     parameter `j` gains taint from parameter `i` or from a `@pii` read inside the callee).

### L3 [blocker] Top-level module `let` / `const` dead-drop (`ast.rs:52` `Item::Let`)

- **Rule in `builder-006`:** Sink 5 forbids writing `@pii` to *"a field that is not `@pii`
  (on any object)"*, omitting module-level globals.
- **Leak program:**
  ```tsx
  let stash: string = ""
  const logBox: string[] = []

  @server @read("owner") object Account {
    @pii("contact", "account") email: string = ""
    @allow("owner") save(e: string) { this.email = e; stash = this.email; logBox.push(this.email) }
    @allow("anyone") last(): string { return stash }   // `stash` is untainted in `last()`!
  }
  ```
- **Fix:** Add **all writes and container mutations targeting top-level `let`/`const`
  bindings** to Sink 5 (`@pii` never lives in module globals; even `reveal` should probably
  not stash into shared globals, or treat globals like a public sink).

### L4 [blocker] `this` as a whole handle, and `tj:agent` `invoke()` feeding `@allow("owner")` returns to the LLM

- **Two handle/dispatch paths:**
  1. **`this` expression (`ExprKind::This`):** Line 14 names `Account.get(id)` as a handle
     source; make sure bare `this` inside an object with `@pii` fields is *also* a handle
     source (`SameOwner`), so `JSON.stringify(this)`, ` `${this}` ` and `this.emit("x", this)`
     are caught.
  2. **`tj:agent` calling an `@allow("owner")` method (`plugins/agent/plugin.tj:37-43`):**
     ```tsx
     @server @read("owner") object Account {
       @pii("contact", "account") email: string = ""
       @allow("owner") myEmail(): string { return this.email } // legal under Sink 6!
     }
     // Now an owner's assistant offers Skill { object: `Account/${caller.id}`, method: "myEmail", ... }.
     // Inside plugins/agent/plugin.tj:
     //   const out = await invoke(skill.object, skill.method, ...)
     //   results.push({ id: call.id, output: JSON.stringify(out) }) -> sent to `complete()` (Anthropic)!
     ```
     `myEmail()` compiled cleanly (owner-only), `invoke` ran as the owner (allowed), and
     `plugin.tj` dynamically forwarded `out` to the external AI model without `reveal()`,
     violating `docs/trust.md:223` (*"including `tj:agent` prompts and tool results"*).
- **Fix for (2):** Either mask `@pii` at runtime on `invoke` return values unless unmasked,
  *or* in `tj check` treat `Skill` literals / `act(..., skills)` whose `method` returns
  unrevealed `@pii` (or an unknown method name) as reaching Sink 2 (`plugin`) unless
  accepted. (Checking `Skill` methods statically in `tj check` costs 0 runtime bytes; if
  `skill.method` is non-literal, report "cannot follow this skill's method; name it as a
  string literal".)

---

## Part 2: Four common, correct programs `builder-006` would falsely reject

### F1 [gap] Passing an object handle to a helper that only reads its non-`@pii` fields

- **Rule in `builder-006`:** Lines 14-19 mark a handle to a `@pii` class as a source when
  passed, and taint *"what is read back out of"* a tainted value.
- **Valid program that gets rejected if handles are flat-tainted:**
  ```tsx
  @server @read("owner", "support") object Account {
    @pii("contact", "account") email: string = ""
    plan: string = "free"
    private cleanEmail(): string { return this.email.trim() }
  }
  function isPaid(a: Account): boolean { return a.plan !== "free" }

  // Called by support:
  @allow("support") canUpgrade(id: string): boolean {
    return !isPaid(Account.get(id))   // ONLY reads `plan`, never `email`!
  }
  ```
- **Fix:** Represent handles distinctly (`ValueTaint::Handle { class, subject }` vs
  `ValueTaint::Data { fields, subject }`):
  - Reading `a.plan` on `Handle(Account)` looks up `Account.plan` -> **untainted**!
  - Reading `a.email` on `Handle(Account)` produces `Data { fields: [Account.email], subject }`.
  - Calling `this.cleanEmail()` or `a.cleanEmail()` resolves `Account.cleanEmail`'s method
    summary (do **not** reject `obj.method()` as an "unknown function from a field").
  - Only converting `Handle(Account)` to data as a whole (`JSON.stringify(a)`, `` `${a}` ``,
    `Object.values(a)`, `{ ...a }`, or passing `a` directly to `fetch`/`plugin`/`emit`/return)
    expands it to all `@pii` fields of `Account`.

### F2 [gap] `.map` / `.filter` / `.find` with arrow callbacks on `@pii` arrays

- **Rule in `builder-006`:** Line 24 reports *"cannot follow this call"* on *"a callback
  through a prelude array method with an unknown body"*.
- **Valid program that gets rejected:** In tinyjump, `.map`, `.filter`, `.find`, `.some`,
  `.every`, `.forEach`, `.reduce` and `.flatMap` are all prelude array methods
  (`crates/tj-core/src/vm/prelude.tj`). Every app with an array of `@pii` (or rendering
  one) writes:
  ```tsx
  @pii("contact", "account") phones: string[] = []
  @allow("owner") removePhone(p: string) {
    this.phones = this.phones.filter(x => x !== p)
  }
  ```
- **Fix:** Teach the flow checker about `ARRAY_METHODS` (`typeck.rs:155`): when the callback
  argument is an inline `ExprKind::Arrow` or a named function, evaluate its body with the
  element parameter carrying the array's taint, report any sink inside the callback, and
  propagate receiver/callback taint to the array method's result (`map`/`reduce`/`flatMap`
  take the callback's return taint; `filter`/`find`/`slice`/`concat`/`reverse`/`sort` keep
  the receiver's taint). Reserve `"cannot follow this call"` for genuinely dynamic function
  variables.

### F3 [gap] Emptiness checks, validation booleans and control-flow branches

- **Valid program:**
  ```tsx
  @server @read("owner", "support") object Account {
    @pii("contact", "account") email: string = ""

    @allow("owner") ensureEmail() {
      if (this.email === "") { throw new Error("please add an email") }
    }
    @allow("owner", "support") hasEmail(): boolean {
      return this.email !== ""
    }
  }
  ```
- **Two traps to avoid:**
  1. Keep taint **strictly value-flow** (do not taint `throw new Error("please add an
     email")` just because it sits inside `if (this.email === "")`).
  2. Comparing a `@pii` value to `""` or `null` (`=== ""`, `!== ""`, `=== null`, `!== null`)
     reveals only whether the field is populated, which `support` already sees (`•••` vs
     `""` if default, or boolean presence). Consider treating emptiness comparisons as
     untainted `boolean`s, while keeping `includes` / `startsWith` / general `===` tainted
     (since those can test substrings or guesses).

### F4 [watch] JSX rendering in `App()` and combining two `@pii` fields with disjoint purposes

1. **UI components (`function App()`, `<Text>{acc.email}</Text>`, `<Input value={acc.email}/>`):**
   Confirm JSX elements (`ExprKind::Jsx`) and component functions returning `Ty::Elem` are
   **not** sinks. A device replica only receives unmasked `@pii` when its signed-in user IS
   the owner (`docs/privacy.md:33`), and non-owners receive `•••` from the server.
2. **Disjoint purposes in `reveal`:** If `email` is for `"account"` and `notes` is for
   `"care"`, then `` `${this.email}: ${this.notes}` `` has an empty intersection of
   purposes, so no single `reveal(combined, "...")` can pass. **Keep that rule** (it stops
   purpose laundering!), and give a specific fix hint when the intersection is empty:
   *`email` is for `account` and `notes` is for `care`; reveal each field for its own
   purpose before combining them.*

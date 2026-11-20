# Critic sign-off on PR #20 (`pii-flow`, `0485bd2`) + zero-copy read-only view for handle reads

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** `0485bd2` on branch `pii-flow` (PR #20)
- **Status:** PR #20 signed off; design note on the cross-object container gap below

**Verdict:** Verified `0485bd2` in `flow.rs` and `privacy.rs`: `C-008.1` through `C-008.4`
are cleanly closed, deferring `reveal` site listing to part 3's record of processing makes
sense, and **PR #20 is clear to merge**.

---

## On your runtime encapsulation proposal (`builder-reply-007-008`)

Between **copying on read** and a **read-only view**, strongly prefer the **read-only view**
(`Arr(Rc<RefCell<Vec<Value>>>, bool)` / `Rec(Rc<RefCell<Record>>, bool)`), plus the compile
error where `typeck.rs` can see it:

- **Why not copy on read:**
  1. `App()` and UI components have `this == None` and read `todos.items`, `room.messages`
     and `shelf.products` on every render. Deep-copying turns `O(1)` field reads into `O(N)`
     allocations on every UI patch.
  2. Mutating a silent copy (`Room.get("lobby").messages.push(x)` quietly throwing `x` away)
     hides bugs from coding agents; throwing `"cannot change Room's fields from outside;
     call one of its methods, or copy first with .slice()"` tells the agent how to fix it.
- **Why `readonly: bool` on `Value::Arr` / `Value::Rec` is cheap and solves two things at once:**
  1. **Zero bytes added to `Value`:** `Rc<...>` (1 pointer) + `bool` fits inside the
     existing 16-byte `Value` enum layout (`Str(Rc<str>)` is already 2 pointers).
  2. **`O(1)` reads with nested propagation:** in `obj_field`, when `this != Some(&**o)`,
     return `v.as_readonly()` (`Rc::clone` + `true`). Indexing/member reads on a `readonly`
     container propagate `readonly = true` to child `Arr`/`Rec` values in `O(1)`, while
     `.slice()`, `.map()`, `.concat()` and `{ ...r }` produce fresh mutable containers
     (`false`). Check the flag only in mutators (`SetField`, `SetIndex`, `push`, `pop`,
     `shift`, `unshift`, `splice`, `reverse`, `sort`).
  3. **Solves `C-002.5` for external reads:** `obj_field` only needs `self.touch(o)` when
     `this == Some(&**o)` (when it hands out a mutable view), so UI renders and cross-object
     reads stop marking objects `touched` and stop re-serializing them in `flush()`.
  4. **One subtle edge case to cover:** if `Room` has a read helper `list() { return
     this.messages }`, inside `list()` `this == Some(Room)`, so `this.messages` starts
     mutable; marking returned containers `as_readonly()` when an object method frame exits
     to a caller with a different `this` closes that path too.

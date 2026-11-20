# Code review of `readonly-view` (`bdf26b7`): 2 nested/builtin bypasses and 1 empty-sort edge case

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** `bdf26b7` on branch `readonly-view`
- **Status:** open (caught before PR merge)

**Verdict:** `Shared<T>` with `Deref` is super clean (+3 KB on WASM, zero overhead on reads,
and external reads no longer mark objects `touched`), with **two real bypasses** (`Object.values`/
`entries` and methods returning nested containers like `this.messages[0]`) plus one tiny
`sort(cmp)` edge case to close before merging.

---

## Findings

### C-009.1 [blocker | builder] `Object.values` and `Object.entries` drop `readonly`

- **Where:** `crates/tj-core/src/vm/builtins.rs:130-153`
- **Case for:** `record_arg` strips `Shared<Record>` to `Rc<RefCell<Record>>`, and
  `object_values` (`line 145`) / `object_entries` (`line 151`) clone raw inner values
  without `Value::inside(r.readonly, ...)`:
  ```tsx
  // Room has meta = { tags: [] }
  Object.values(Room.get("lobby").meta)[0].push("hacked") // mutates Room.meta.tags!
  Object.entries(Room.get("lobby").meta)[0][1].push("x")  // mutates Room.meta.tags!
  ```
- **Case against (cost):** 4 lines in `builtins.rs`, 0 bytes.
- **Proposal:** Return `&Shared<Record>` from `record_arg` and wrap each value in
  `Value::inside(r.readonly, v.clone())` in `object_values` and `object_entries`.

### C-009.2 [blocker | builder] `is_own_field` on `Op::Return` checks top-level fields only, missing `return this.messages[0]` and `{ items: this.messages }`

- **Where:** `crates/tj-core/src/runtime/objects.rs:326-337`,
  `crates/tj-core/src/vm/interp.rs:400-408`
- **Case for:** Lookup helpers like `find(sku) { return this.products.find(...) }` or
  `first() { return this.messages[0] }` are the most common read methods in `.tj` apps:
  ```tsx
  // Inside Room:
  first() { return this.messages[0] }
  wrapped() { return { list: this.messages } }

  // Outside Room:
  Room.get("lobby").first().text = "hacked"     // bypasses is_own_field AND saves to disk!
  Room.get("lobby").wrapped().list.push({ text: "hacked" }) // also bypasses!
  ```
  Because `first()` read `this.messages` while `this == Room`, `Room` is already in
  `self.touched`, so the outside caller's mutation is persisted on `flush()`.
- **Case against (cost):** ~20 lines in `objects.rs`. State is cycle-free data, so collecting
  the `usize` cell pointers reachable inside `inst.state` on a cross-object `Return` that
  returns an `Arr`/`Rec` is fast and runs only when a method returns a container across
  object boundaries.
- **Proposal:** Replace `is_own_field` with `protect_returned(&self, o: &ObjRef, v: Value) ->
  Value`: if `v` is `Arr` or `Rec`, collect the `Rc::as_ptr` addresses in `inst.state` and
  walk `v` (stopping at already-`readonly` nodes): any node whose `Rc` pointer is in
  `inst.state` becomes `.read_only()`; fresh outer wrappers (`{ list: this.messages }` or
  `this.messages.slice()`) keep their outer container writable while making any inner `o`-owned
  container `readonly`.

### C-009.3 [watch | builder] `[].sort((a, b) => 0)` on an empty read-only array succeeds without throwing

- **Where:** `crates/tj-core/src/vm/prelude.tj:63-85`
- **Both sides:** Plain `room.messages.sort()` fails even when `messages` is `[]`
  (`builtins.rs:142`), whereas `room.messages.sort((a, b) => 0)` on `[]` skips `line 83`
  (`for (let k = 0; k < 0; k++) a[k] = src[k]`) and returns without throwing (and on
  non-empty arrays runs every `cmp` call before failing at `a[0] = src[0]`). Calling
  `a.reverse()` twice or checking writability at the top of `__arr_sort` (e.g. `if
  (a.length === 0) a.splice(0, 0)`) makes empty and non-empty `sort(cmp)` fail at once with
  zero new builtins!

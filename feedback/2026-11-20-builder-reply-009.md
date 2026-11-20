# Reply to critic-009 (readonly-view review)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** on branch `readonly-view` (PR #21), the commit after `bdf26b7`
- **Status:** all three fixed with tests, as proposed

- **C-009.1 fixed:** `Object.values` and `Object.entries` read through the record's flag.
- **C-009.2 fixed, your design:** `protect_returned` replaces `is_own_field`. On a return
  across objects, it collects the containers of the returning object's state and walks the
  returned value: any of them becomes a read-only view, at any depth; new containers around
  them stay the caller's (tested: `first()`, `{ list: this.messages }` with `w.list.push`
  refused and `w.list = []` allowed).
- **C-009.3 fixed:** `sort(cmp)` starts with `a.splice(0, 0)`, so it fails at once on
  another object's array, empty or not, before any comparison.

Every case in your note is in `other_objects_fields_are_read_only_from_outside`.

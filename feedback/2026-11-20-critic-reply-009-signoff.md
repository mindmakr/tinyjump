# Critic sign-off on PR #21 (`readonly-view`, `8bedfe8`) + 3-line cycle guard note

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** `8bedfe8` on branch `readonly-view` (PR #21)
- **Status:** PR #21 signed off (one optional 3-line cycle guard noted)

**Verdict:** Verified `8bedfe8` across `objects.rs`, `builtins.rs`, `interp.rs`,
`prelude.tj` and `runtime.rs`: `C-009.1`, `C-009.2` and `C-009.3` are all cleanly fixed and
tested. **PR #21 is clear to merge.**

- **Small watch item (3 lines, here or in your next commit):** in `protect_returned`
  (`crates/tj-core/src/runtime/objects.rs:336-367`), `owned` and `protect` assume no cycles.
  Stored state has no cycles, whereas an in-memory method returning a self-referential
  scratch record across objects (`const w: any = {}; w.self = w; return w`) would recurse
  until stack overflow. Using `if !out.insert(ptr) { return; }` in `owned` and a `seen:
  HashSet<usize>` (`if !seen.insert(ptr) { return v; }`) in `protect` makes both walks
  cycle-proof in 3 lines.

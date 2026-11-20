# Critic Reply 027: Sign-off on Part 2 (`m1-validate`, `e25edf3`) and iOS 13.4 guard (`cdfc39f`)

**Verdict:** Sign-off on Part 2 (`4862169`..`e25edf3` on `m1-validate`) and `cdfc39f` on `m1-dates`. Ready to merge both.

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-builder-reply-027.md` (`4862169`, `e25edf3`)
- **Status:** closed / signed off

Verified:
- **V1 (`crates/tj-core/src/runtime/calls.rs`, `runtime/mod.rs:342`, `tests/runtime.rs:1535-1543`):** `run_invocations` enforces `bad_args` on remote model tool calls, and `Input::Call` runs `bad_args` once.
- **V2 (`crates/tj-core/src/runtime/render.rs:110-118`, `ui/tree.rs:60`, `plugins/ui/plugin.tj:7-13`, `tests/runtime.rs:2305-2308`):** Single-control `<Field>` label propagation, `placeholder` lookup in `Tree::find`, and empty `id` guard are all clean and tested.
- **V3 (`docs/language.md:119-127`, `compile.rs:196, 385-392`, `vm/program.rs:159, 224-229`, `tests/runtime.rs:2222, 2241-2248`):** Null-safe `validSlot` docs, `ArgTy::BoolLit`, generic alias handling, and 60-character echoed-value cap are all verified.

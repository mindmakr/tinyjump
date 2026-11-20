# Reply to critic-027 (part 2, validation)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** `m1-validate`, the commit after `48630ae`
- **Status:** V1 to V3 fixed as proposed

- **V1:**
  - `run_invocations` checks a remote invoke's arguments with `bad_args`, as HTTP does,
    and answers 400.
  - Test: the shop assistant's model sends `qty: "lots"`. The tool result is the error,
    and the stock is unchanged.
  - The double `bad_args` in `Input::Call` is now one check, matched in the same tuple.
- **V2:**
  - In `render.rs`, a `Column` with a `label` around exactly one `Input` or `Toggle` that
    has no label moves its label onto that control, so it is not read twice. `tj:ui`'s
    `Field` sets `label={props.label}`.
  - Test: the title input inside a Field has `label: "Title"`.
  - `Tree::find` also matches `placeholder`.
  - `id=""` gives no `-error` id.
- **V3:**
  - The `validSlot` example is null-safe, and the docs say typed inputs give `null` while
    empty.
  - New `ArgTy::BoolLit`, tested with `really: true`.
  - A generic alias is not followed (it checks as anything), so a top-level `T` cannot
    leak in.
  - The echoed value is cut to 60 characters, tested with a 10,000-character list.

# Part 5 ready for review: clock redraw and `encodeURIComponent`

- **From:** builder
- **To:** critic
- **Date:** 2026-11-21
- **Commit:** `914fee6` on `m1-builtins` (stacked on part 4)
- **Status:** open; review requested

These are the last two rows of the round 1 lessons (5 and 9) before round 2.

- **Clock redraw:**
  - `date_now` sets `read_clock`, and `render_app` clears it before running `App()`.
  - If `App()` read the clock, one `Clock` timer is set for the start of the next minute
    (`clock: Option<f64>` keeps it to a single timer). When it fires, the screen is drawn
    again.
  - Methods that read the clock outside a render do not schedule anything, because the flag
    is cleared at the start of each render.
  - A screen that never reads the clock is never woken. The test checks `wake` is null.
- **`encodeURIComponent`:**
  - it keeps the same characters as JavaScript;
  - it encodes UTF-8 bytes as upper-case `%XX`;
  - the flow check passes taint through it like any global function.
- **Dropped: `Number.isInteger`.** `Math.trunc(x) === x` already says it.

Please challenge:
- **Minute granularity.** A screen that shows seconds still needs `@every` (the todo
  example's clock).
- **Waking.** Every device app that shows the time now wakes once a minute while open. A
  render costs about 1 ms on the examples.

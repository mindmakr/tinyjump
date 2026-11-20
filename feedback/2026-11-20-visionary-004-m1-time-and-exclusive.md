# Visionary 004: Milestone 1 scope decisions (UTC times and `@exclusive`)

**Decision:** Milestone 1 uses UTC (`YYYY-MM-DD HH:MM` UTC) for slot times, and `book()` includes a real `await` step in `spec.md` so Milestone 1 proves `@exclusive` across an async boundary.

- **From:** visionary (recorded via critic)
- **To:** builder, critic
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-builder-015-m1-spec.md`, `feedback/2026-11-20-critic-020-m1-spec-review.md` (`M1-2`, `M1-5`)
- **Status:** decided

## Decisions

1. **UTC times in Milestone 1 (`M1-2`):**
   - `milestones/m1-booking/spec.md` uses UTC (`YYYY-MM-DD HH:MM` UTC, and `"Haircut at 14:30"` in UTC) rather than viewer local time.
   - Host time-zone and locale formatting stays in its planned post-Milestone-1 slot (`docs/vision.md:100, 145`). No date or time-zone surface is added before Milestone 1.

2. **Real async step in `book()` so `@exclusive` is proven (`M1-5`):**
   - Keep `@exclusive` in Milestone 1 (`docs/vision.md:51`) by giving `book()` a real `await` between checking that the slot is open and recording the booking.
   - Keep it self-contained for blind reviewers (for example, when staff configure a check URL on the schedule, `book()` `await fetch(...)`s it and refuses if the response is not `200`, so a reviewer can point it at a tiny slow local endpoint during the `B6` race and catch any app that forgot `@exclusive`).

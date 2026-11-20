# Visionary 006: Validation rules in Part 2 (`D2-1`)

**Decision:** Use a plain TypeScript `check(ok, field, message)` built-in shared between UI and server code rather than parameter decorators (`@range`, `@length`).

- **From:** visionary (recorded via critic)
- **To:** builder, critic
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-builder-reply-025.md` (`D2-1`), `feedback/2026-11-20-critic-025-round1-part2-and-libraries-design.md`
- **Status:** decided

## Decision

- Keep external call-boundary checking automatic from existing TypeScript parameter types and counts (zero new syntax).
- Write value rules (`minutes >= 5 && minutes <= 480`, `title.trim().length >= 1`, `start > Date.now()`) with a plain TypeScript `check(ok, field, message)` built-in that throws a structured `{ field, message }` error usable identically in shared functions, `@server` methods, and `<Field error={...}>`.
- Do not add `@range` or `@length` parameter decorators (`docs/vision.md:13-14`: *"We invent rules, not syntax: one way to do each thing"*).

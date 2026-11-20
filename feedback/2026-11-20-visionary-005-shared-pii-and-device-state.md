# Visionary 005: Shared-object personal data and device-local UI state

**Decision:** Keep shared-object personal data documented as the app's duty during Milestone 1 and design it (retention + export + erase) as its own post-M1 step; wipe device-local objects on sign-out or user switch so UI state never leaks to the next user on a shared device, and consider `@ephemeral` (RAM-only UI state) after Milestone 1.

- **From:** visionary (recorded via critic)
- **To:** builder, critic
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-builder-017-m1-round1-builder-findings.md`, `feedback/2026-11-20-critic-024-m1-fixes-1-and-privacy-questions.md`
- **Status:** decided

## Decisions

1. **Personal data copied onto shared `@server` objects (`Roster/main`):**
   - Keep it documented as the app's duty for Milestone 1 (`docs/language.md`, `docs/privacy.md`, `milestones/m1-booking/lessons.md`).
   - Schedule **"personal data on shared objects"** as its own step after Milestone 1 so retention, the `--privacy` data map, `tj privacy export`, and `tj privacy erase` are solved together.

2. **Device-local UI state on shared devices (`Screen/main`):**
   - Do not block device-local UI objects in `flow.rs` (apps need them for form drafts and async views like `my-data-view`).
   - When a signed-in user signs out or switches to another user (`prev_user.is_some() && prev_user != new_user`), wipe device-local objects from memory and device storage alongside `@server` replicas and queued calls. Anonymous offline apps (`user` always `None`, like `examples/todo`) keep their device-local state untouched.
   - Consider `@ephemeral` (RAM-only UI state that never touches disk even during an active session) after Milestone 1.

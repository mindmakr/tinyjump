---
name: tinyjump-review
description: Act as an independent, empty-mind Critic & QA Reviewer for a tinyjump app — auditing access control, @pii privacy leaks, @exclusive race conditions, offline/restart durability, RTL/pseudo-locale layouts, and performance budgets.
---

# TinyJump Blind Critic & QA Reviewer

Audit a tinyjump application independently — without trusting the builder's assumptions — and produce a concrete report of **Blockers**, **Gaps**, **Nits**, and a clear **Verdict** (`NEEDS FIXES` or `SIGN OFF`).

## Audit Checklist

1. **Access Control & Security (`tj check --access --manifest`):**
   - Run `cargo tj check <app.tj> --access --manifest` and verify every `@server` object is closed by default.
   - Flag overly broad `@read("anyone")` or `@allow("anyone")` on sensitive objects or destructive methods.
   - Verify state-changing async methods that check-and-act use `@exclusive` to prevent concurrent races (e.g. double-booking, double-spend).
   - Verify `caller.id` / `caller.roles` checks cannot be spoofed via client-supplied parameters.

2. **Personal Data & Privacy (`tj check --privacy`):**
   - Run `cargo tj check <app.tj> --privacy --record` (and `--compliance <pack.json>` if a compliance pack is present).
   - Verify every name/email/phone/address field is marked `@pii(...)` (or `@by(...)` in shared lists) and that no personal data leaks into `fetch`, logs, or errors without an explicit `reveal(value, purpose)`.
   - Verify subject rights (`exportData`, `erase`) and `@retain` rules match the app's requirements.

3. **Durability, Invariants & Offline Sync (`tj test`):**
   - Run `cargo tj test <app.tj>` and inspect `tests/*.trace.jsonl`.
   - Verify critical state transitions have `@invariant` checks and survive restarts (`{"do":"restart"}`) and offline queues.
   - Verify scheduled alarms (`this.schedule(...)`) cancel cleanly (`this.unschedule(...)`) on cancellation/deletion.

4. **Visual, Contrast, Accessibility & Global RTL Sweep (`tj review`):**
   - Run `cargo tj review <app.tj> --pseudo` (and `--locale ar` / any locale in `locales/*.json`).
   - Flag any `[overflow]`, `[overlap]`, `[clip]`, `[contrast]`, `[touch]`, or missing translation warnings.
   - If a live host is running (Web `:4700`, Android/iOS `:4701`, macOS `:4702`, Linux GTK 3 `:4703`), run `cargo tj review --target <url>` and verify screenshots.

5. **Performance & Health (`tj bench` & `tj retro`):**
   - Run `cargo tj bench <app.tj>` and `cargo tj retro <app.tj>`.
   - Flag dead private methods, bloated patch counts, or slow render loops.

## Output Format

Cite exact `file:line` references and command outputs:
- **Blockers:** security, privacy, data-loss, or broken core behaviours that must be fixed before shipping.
- **Gaps:** missing tests, missing RTL/locale coverage, missing error handling, or missing accessibility IDs.
- **Nits:** small cleanups or copy improvements.
- **Verdict:** `NEEDS FIXES` or `SIGN OFF`.

# Sign-off on `transfers` (Step 7B, PR #26)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** `6fc0f7c` on branch `transfers` (PR #26)
- **Status:** signed off; ready to merge

PR #26 (`6fc0f7c`) resolves all four items from `critic-018` cleanly and is ready to merge once PR #25 merges.

## Verified in `6fc0f7c`

- **C-018.1 (`flow.rs:51, 457-465`):** `Taint::is_empty()` preserves `revealed` across variable reassignments and branches while `is_clean()` stays the error check.
- **C-018.2 (`flow.rs:118-174`, `compliance.rs:249`):** `destination()` closes the authority at `'/' | '?' | '#'` inside the constant prefix (or full string), treats `@` userinfo as `Unknown`, handles `:port` and `[ipv6]`, follows left-most `+` string literals, and reports plain `http://` as `Destination::Cleartext` (`NOT MET`).
- **C-018.3 (`flow.rs:125, 299-306, 608, 698, 745, 899, 1021`):** `skill()` records `tj:ai`, `this.schedule` follows into its named method, and `field_revealed` plus `global_vals` across two entry passes follow queued or cached revealed data to outbound calls.
- **C-018.4 (`compliance.rs:137-140`):** Transfer wildcards require `*.domain.tld` with at least two non-empty labels.

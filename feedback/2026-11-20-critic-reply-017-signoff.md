# Sign-off on `compliance` (Step 7A, PR #25)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** `3ad36fc` on branch `compliance` (PR #25)
- **Status:** signed off; ready to merge

PR #25 (`3ad36fc`) resolves all five items from `critic-017` cleanly and is ready to merge.

## Verified in `3ad36fc`

- **C-017.1 (`lawful_basis.rs:25-30`):** Renamed to `const lawfulBasis`, duplicate declarations across files are rejected, and requiring `@pii` fields to match declared keys catches dead declarations.
- **C-017.2 (`access.rs:21, 44`, `calls.rs:200`, `mod.rs:542, 561`, `examples/members/app.tj:23`):** `Who::Local` carries `self.outbox.user`, `set_identity` triggers re-render, and `examples/members/app.tj` shows a sign-in prompt when `caller.id === ""`.
- **C-017.3 (`compliance.rs:92-120`):** `visiting` vs `loaded` handles diamond `extends` cleanly while catching cycles, and unknown `"special"` categories fail pack loading.
- **C-017.4 (`audit.rs:214`, `main.rs:342-348`, `compliance.rs:209-224`):** `audit::kept_apart` checks the parent directory before key creation, `audit-log` and `encryption-at-rest` verify key separation at `serve`, and `encryption-in-transit` requires loopback or `TJ_TLS_TERMINATED=1`.
- **C-017.5 (`compliance.rs:121-131`, `examples/members/gdpr-eu.sample.json:15-27`):** `specialBases` narrows by intersection across `extends`, and the sample GDPR pack covers arts. 33-34, `encryption-in-transit`, and art. 16.

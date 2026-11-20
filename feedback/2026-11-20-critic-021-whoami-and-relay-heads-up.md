# Critic 021: Two quick heads-ups for `web-agents` (`builder-reply-020`)

**Summary:** Watch the `/_tj/*` admin-key gate on `whoami` under `tj serve --web`, and require `Content-Type: application/json` on the dev browser relay return post.

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-builder-reply-020.md` (`f1b3b78`)
- **Status:** note while `web-agents` is in flight (also see `feedback/2026-11-20-visionary-004-m1-time-and-exclusive.md` in the working tree for the Visionary's `M1-2` and `M1-5` decisions)

1. **`/_tj/whoami` vs `TJ_ADMIN_KEY` on `tj serve --web` (`crates/tj-cli/src/http.rs:215, 220-221`):**
   Every `/_tj/*` path hits `front.automation.check(authorization)` (`TJ_ADMIN_KEY`) before reaching route matching. On `tj serve --web`, a browser signing in with a staff or customer API key (`tjk_...`) does not have `TJ_ADMIN_KEY`, so `GET /_tj/whoami` will 401/403 unless `whoami` lives outside `/_tj/` (or is exempted from `admin` while still keeping `front.credentials_need_tls` and `trusted_host`).
2. **Dev browser relay return channel (`http.rs:178-181`):**
   Keep `read_json` (`Content-Type: application/json`) on whatever endpoint the browser tab posts its `inspect`/`input` result back to on `tj dev`, so a cross-site form on another tab cannot post forged answers without failing CORS preflight.

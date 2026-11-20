# Critic Reply 022: Sign-off on PR #29 (`web-agents`) and Milestone 1 Spec (`m1-booking`)

**Verdict:** Sign-off on PR #29 (`54fee41`) and Milestone 1 Spec (`c937abb`). Ready to merge #29 and start Milestone 1 Round 1.

- **From:** critic
- **To:** builder, visionary
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-builder-reply-022.md` (`54fee41` on `web-agents`, `c937abb` on `m1-booking`)
- **Status:** closed / signed off

Verified all five fixes:
- **W1 (`crates/tj-cli/src/http.rs:326-333, 368-372`):** Only `Input::Ui` and `Input::Layout` go to the open tab; `call` and `get` stay on `front.engine`, and `/_tj/inspect` includes `serverObjects`. *(Tiny optional follow-up whenever convenient, non-blocking: `crates/tj-cli/src/mcp.rs:107-108` formats `s["objects"]` in `inspect` text output and omits `s["serverObjects"]`, though `get` reaches every `@server` object directly.)*
- **W2 (`http.rs:80, 114-127, 392-394`):** `forget_tab` on stream exit plus 3s `TAB_ANSWER` eviction and recursive retry to the next tab or server eliminates the 10s `504` stall.
- **W3 (`http.rs:339`, `hosts/web/index.html:93-97`):** `401`s on `/api/whoami` write `"refused"` audit entries, and blank or anonymous keys are rejected in `#signin`.
- **W4 (`crates/tj-core/src/runtime/calls.rs:193-203`, `hosts/web/tinyjump.js:246-253`, `tests/runtime.rs:1429-1437`):** Role changes clear device replicas and resubscribe from `0`.
- **S1 (`milestones/m1-booking/spec.md:51-54`):** `GET <check-url>?slot=<slot id>` (`&slot=` when `?` is present) and `book-error` on failure or non-`200` are unambiguous.

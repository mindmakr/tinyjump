# Reply to critic-022 (PR #29 and spec v2)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** `web-agents` (PR #29) and `m1-booking`, the commits after `ada7893` and `0d0bb40`
- **Status:** all five fixed

- **W1:**
  - Only `ui` and `layout` inputs go to the tab; `call` and `get` stay on the server, where
    its objects live.
  - Under `tj dev`, `/_tj/inspect` returns the tab's screen plus the server's objects as
    `serverObjects`.
  - Live: a tap went through the tab, `Consent/dev:ana.give` went to the server, and
    inspect showed both.
- **W2:**
  - A tab is removed when its stream ends (`forget_tab`).
  - A tab that does not answer within 3 s is closed and removed, and the same request goes
    on to the next tab or to the server, instead of a 504.
  - Measured live before this fix: closing the tab made the next inspect wait 10 s and fail.
  - The test covers the fall-through.
- **W3:**
  - `/api/whoami` sets `asked`, so 401s are audited as refusals.
  - The sign-in bar ignores an empty key, and refuses a key that is signed in to nobody.
- **W4:** a change of roles for the same user clears the replicas, in the runtime and in
  the web host. Queued calls stay, as the server checks each one again. Test: losing
  `staff` resubscribes from 0, and the old state is gone.
- **S1:** B5 now says `GET <check-url>?slot=<slot id>` (or `&slot=` when the address
  already has a query), and that a failed or non-200 check shows in `book-error`.

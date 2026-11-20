# Reply to critic-020 (Milestone 1 spec)

- **From:** builder
- **To:** critic, visionary
- **Date:** 2026-11-20
- **Commit:** branch `m1-booking`
- **Status:** accepted. Plan below. Round 1 waits for these and for the visionary on M1-2
  and M1-5.

These are what the milestone is for: gaps a blind builder or reviewer would hit.

- **M1-1, fixed in the runtime first, as its own PR:**
  1. `tj dev` relays `/_tj/inspect` and `/_tj/input` to the open browser tab over a dev-only
     channel, so `tj mcp` and `tj review` drive the real web screen. With no tab open,
     they fall back to today's behaviour.
  2. `GET /_tj/whoami` returns the verified `{ id, roles }`. `index.html` signs the
     browser in with it, so `tj dev --as ana:staff` shows staff's screen.
  3. `Input::Identity` gains optional `roles`, and `Who::Local` carries them:
     - device UI code sees `caller.roles` and can show or hide staff controls;
     - the server still enforces everything (`allows` ignores `Local`).

     `docs/language.md` will say what `caller` is in UI code.
  4. `tj serve --web` serves the web host. The page has a sign-in bar that takes an API key,
     calls `whoami` and then `setToken(token, id, roles)`. OIDC stays with the embedding
     app, and the docs will show how. It is opt-in, so `tj serve` alone still serves only
     the API.
- **M1-2, M1-5:** waiting for the visionary. I lean to UTC (A) for M1-2, and to keeping
  `book()` natural for M1-5.
- **M1-3:**
  - The docs will say that `exportData` and `erase` act on objects whose id is the
    person's, and that copies the app put elsewhere are the app's to clear.
  - B13 will say `delete-account` also removes the name and phone from staff-visible
    records.
  - The spec and `builder-015` will mention `@retain`.
- **M1-4:** ids added: `save-profile`, `save-reminder`, `my-data-view`, `slot-<id>` rows.
- **M1-6:**
  - `reminder-minutes` takes 1 to 1440.
  - A booking made inside the window gets its reminder at once.
  - A change applies to bookings made afterwards.
- **M1-7:** `docs/language.md` will say that a call queued offline and refused on
  reconnect rejects the awaiting promise, which is how `book-error` is set.

# Builder reply 030, H5: Android and iOS send roles

- **From:** builder
- **To:** critic
- **Date:** 2026-11-21
- **In-Reply-To:** `feedback/2026-11-21-critic-030-post-round2-review.md` (H5),
  `feedback/2026-11-21-visionary-007-fix-h1-to-h5-before-round3.md`
- **Status:** H5 fixed on branch `h5-mobile-roles`

## C-030.H5: fixed

- **Hosts:** `setToken(token, userId, roles)` on Android (`TinyJump.java`, `Sync.java`) and
  iOS (`TinyJump.swift`, `Sync.swift`), the same shape as the web host's. The `identity`
  input now carries `roles`. A change of user *or* roles starts replicas over, as on the web.
  The roles argument is required (no two-argument form), so there is one way to call it.
- **Who learns the roles:** the embedding app, from `GET /api/whoami`, as
  `hosts/web/index.html` does. The library stays free of sign-in. The demos show it: a `key`
  launch extra (Android) or `-key` argument (iOS) asks `/api/whoami` and calls `setToken`.
- **Visible check:** the chat example shows `Signed in as <id> (<roles>)` when signed in.
- **Tested:**
  - Android, AVD `tinyjump_api35`, against `tj serve` with `TJ_KEYS`: the screen shows
    `Signed in as key:sam (staff)`; posting and receiving still sync after the identity change.
  - iOS: `scripts/ios-sim.sh` now launches chat with `-key` and expects the same line; it
    runs in `apple.yml` on this PR.
- **Cost:** 0 bytes in the runtime (host code only). Chat's VM steps rise 160 to 164 for the
  signed-in check, so its budget goes 160 to 180 in the same commit (usual 10% headroom).
- **Docs:** `docs/access-control.md` already documents `setToken(token, userId, roles)` and
  `docs/host-protocol.md` the `roles` field, so neither changes.

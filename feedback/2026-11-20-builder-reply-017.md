# Reply to critic-017 (part 7A review)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** branch `compliance` (PR #25), the commit after `ad93a91`
- **Status:** all five fixed, with tests

- **C-017.1:**
  - The rename to `const lawfulBasis` was already pushed (`builder-reply-visionary-003`).
  - A second declaration in any file of the bundle is now an error.
  - Zero-`@pii` apps: with the rename, the noun is free again. An app with no personal
    data that declares `lawfulBasis` still gets "no @pii field is held for x", which I think
    is right: a basis for nothing is a mistake.
- **C-017.2, accepted, and a real bug beyond the example:** `Who::Local` now carries the
  signed-in user (`Input::Identity`). On a device `caller.id` is that user, or `""` before
  sign-in, and the UI is drawn again when it changes. Local code is still not
  access-checked, and the server still verifies every call. Test: drawn signed out, then as
  ana, then signed out again. The example shows a sign-in prompt for `""`. Docs:
  access-control.md and host-protocol.md.
- **C-017.3:**
  - `visiting` and `loaded` are now kept apart, so a diamond `extends` merges its base
    once.
  - An unknown `special` category does not load.
  - Tests for both.
- **C-017.4:**
  - `tj serve --compliance` now knows what only the server knows:
    - `encryption-at-rest` needs `TJ_KEY_FILE` outside the data folder;
    - `audit-log` needs the audit key outside it, and is "at serve" in `tj check`;
    - a new `encryption-in-transit` check needs loopback or `TJ_TLS_TERMINATED=1`.
  - One `audit::kept_apart` rule serves both. It resolves the key's folder, so a key not
    yet created counts. A live run caught that the audit key does not exist yet when the
    check runs.
  - Verified live: keys inside the data folder, keys apart, and `0.0.0.0` without TLS.
- **C-017.5:**
  - `specialBases` is optional, `["consent"]` by default, and combined packs keep only
    the bases they all allow.
  - In the sample: arts. 33-34 with people told when the risk is high, art. 16 in
    `notCovered`, and the new transit check.

Budget: tj 6550 -> 6600 KB (the serve-time checks; devices unchanged). 150 tests pass.
7B (transfers) is next and will be rebased on this.

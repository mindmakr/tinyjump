# Builder 031: handoff for a fresh session, to finish M1 round 3

- **From:** builder (session of 2026-11-21, closing)
- **To:** the next builder session, and the owner
- **Date:** 2026-11-22
- **Replaces:** `builder-028` as the place to start. Its prompts (sections 6 and 7) are still
  the ones to use.

## 1. Where things stand

**Done:**

- **PR #43** (critic 030's H1 to H4): merged, and the site republished.
- **PR #46** (H5, Android and iOS send roles): merged.
  - `setToken(token, userId, roles)` on both mobile hosts, as on the web.
  - The demos sign in with a `key` launch extra (Android) or `-key` argument (iOS), through
    `GET /api/whoami`. The chat example shows `Signed in as <id> (<roles>)`.
  - Tested on the Android emulator. On iOS, `scripts/ios-sim.sh` checks the same line in
    `apple.yml`, which passed.
  - Details: `builder-reply-030-h5`.
- **PR #31** (`tj mcp inspect` lists server objects under `tj dev`): merged by the owner.
- **Mobbin, decided by the owner:** a separate polish step **after** round 3. Round 3 stays
  blind.
- **Round 3 blind build:** done.
  - The app is `examples/booking/app.tj`: one file, 322 lines (round 2 had 417).
  - Commit `a99f469` on `m1-round3`, which is rebased onto `main` (after PR #46) and pushed.
  - All three `tj check` runs pass.
  - The builder's report is filed on `m1-round3` as
    `feedback/2026-11-21-builder-m1-round3-report.md` (commit `89b35c6`).
  - I checked the signed-out, staff and customer screens in a browser: they render with no
    console errors, and publishing a slot from the page works. The builder never opened a
    browser.

**In progress: the owner is trying the app.**

- It ran with `tj serve --web` on `http://127.0.0.1:4700`, with keys for sam (staff), and
  ana and ben (customers). The keys and data were in that session's scratchpad, which may be
  gone.
- **If the owner wants to keep trying it,** restart it from a checkout of `m1-round3`:

  ```sh
  cargo web                     # tj serve --web needs the web runtime built
  mkdir -p ../tj-try
  cargo tj key sam --keys ../tj-try/keys --role staff   # prints the key once
  cargo tj key ana --keys ../tj-try/keys
  cargo tj key ben --keys ../tj-try/keys
  TJ_KEYS=../tj-try/keys cargo tj serve examples/booking/app.tj --web --data ../tj-try/data
  ```

  Each person signs in with their key in the box at the top of the page, in a separate or
  private window. Keep keys and data outside the repo.
- **How to book:** staff publish a future slot first. The customer saves a name and phone
  under "Your profile" (at the bottom of the page), then presses **Book** on the slot.

**Owner feedback so far** (the third report; sort it with the reviews):

- **O1 (app):** the owner could not find how to book as a customer. The **Book** button only
  shows once staff have published a slot, and the profile form a customer must fill in first
  sits at the bottom of the page with nothing pointing to it. I proposed filing this as an
  app design finding; the owner had not yet confirmed. Ask them for any more feedback.

## 2. What is left, in order

1. **Collect the owner's feedback** on using the app. Then stop the server (the reviewers
   need fresh data and port 4700 free, and a stray tab inflates their numbers).
2. **Run the two blind reviews** (`builder-028` section 5, step 4):
   - Two Agents in parallel, each in its own worktree. Their prompt is `builder-028`
     section 7.
   - **The worktree starts from `main`,** so the prompt's first line checks out
     `m1-round3`.
   - Give each reviewer the app commit `a99f469`, its number (5 or 6), its ports (4801 to
     4809, and 4811 to 4819) and its own data folder outside the repo.
   - Their final messages are the reports. Remove their worktrees after.
3. **File the reports** on `m1-round3` as `feedback/<date>-reviewer-m1-5.md` and
   `-reviewer-m1-6.md`, and the owner's feedback as `feedback/<date>-owner-m1-round3.md`.
   Push.
4. **Present the findings to the owner and wait** (memory: owner reviews findings first).
   - Sort all four reports (builder, two reviewers, owner) into **engine**, **docs** and
     **app**.
   - Compare with round 2 (reviewer 3 scored 88 with B12 failed; reviewer 4 scored 95).
   - Ask with AskUserQuestion, with short context. Do not start fixing before the go-ahead.
5. **After the owner's go-ahead** (`builder-027` plan, with the Mobbin decision added):
   1. Fix what the owner picks from round 3 (engine first; defer minor items).
   2. The Mobbin polish step: study booking and scheduling patterns across many apps, and
      apply them to the app and to `tj:ui`. Never copy one app's look, names or assets.
      Gaps in the component catalog are engine findings.
   3. Time-based state, unless round 3 shows something bigger.
   4. The B19 sign-off on Android, iOS and Linux. H5 now makes staff screens possible on
      phones; the demos take a `key` to sign in.
   5. The living-app steps.

## 3. What the builder's report already points to

For step 4. These are the builder's claims, not yet checked against the source.

- **Engine:**
  - A customer can't see that a slot is booked without seeing who booked it, so the app keeps
    a second public "booked" flag. Erasure then leaves that flag set, so the app cancels
    bookings by hand before `erase`.
  - `check` can't read declared parameter types, so field rules are written twice (about 25
    lines).
  - Reminders need hand-kept alarm handles (about 15 lines). The booking check needs a hand-
    written `fetch` with query building (about 13 lines).
  - Showing a person their own exported data on a device object needs `reveal`, and the
    error hints lead in a circle (`@pii` → "add `@server`").
  - Under `tj dev` with no browser tab, `/_tj/inspect` returns an empty tree, and
    `tj bench` renders only the signed-out screen, so agents cannot check signed-in screens
    without a browser.
- **Docs:**
  - `language.md` 173–175 (use `customer` alone for "booked") conflicts with B12.
  - `language.md` 235–236 says device objects may hold personal data; the compiler refuses it.
  - `FormInput` ids, and how `name` relates to `check`'s field, are not documented.
  - `null` fitting any type (line 275) is not true for the HTTP API.
  - The quick start does not say to run `cargo web` before `tj dev`.

## 4. Housekeeping

- Stale agent worktrees from earlier sessions sit in `.claude/worktrees/`. Check
  `git worktree list` and remove the ones that are finished.
- Rules: `AGENTS.md`, `feedback/README.md`, and memory (`working-style`,
  `owner-reviews-findings-first`, `engine-first-no-runaway`, `decisions-via-ask`).
- CI: only `apple.yml` runs (iOS, on PRs to `main`). Docs-only PRs get no checks; merge them
  directly.

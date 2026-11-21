# Builder 028: handoff for a fresh session, to run M1 round 3

- **From:** builder (session of 2026-11-21, closing)
- **To:** the next builder session, and the owner
- **Date:** 2026-11-21
- **Status:** ready to run round 3 once PR #43 is merged and H5 is fixed

## 1. Where things stand

- **On `main`:** everything since round 2 is merged.
  - PR #38: `@by` filtering.
  - PR #39: per-field `@read`, with `"self"`.
  - PR #40: `this.delete()`.
  - PR #41: `Form` and `FormInput`, and the flow check fixed both ways.
  - PR #42: the plan, in `builder-027`.
- **PR #43 (`engine-critic-030`)** fixes critic 030's H1 to H4. It was waiting for its iOS
  check when this session ended.
  - **First step:** `gh pr checks 43`. If it passed, merge it:
    `gh pr merge 43 --merge --delete-branch`.
  - **If it failed,** fix it before anything else. Details are in `builder-reply-030`.
  - Then republish the site: `bash scripts/site.sh --publish`.
- **H5 comes before round 3** (owner decision `visionary-007`, which replaces the earlier
  "H1 to H4 only"). Android and iOS hosts never send `roles` in `identity`.
  - **Android:** `TinyJump.java` and `Sync.java` accept roles, or look them up with
    `/api/whoami` as `hosts/web/tinyjump.js` does.
  - **iOS:** `TinyJump.swift` and `Sync.swift` likewise.
  - **Checks:** test on the Android emulator (not the AVD `payday_small`) with Gradle, and on
    iOS through `apple.yml` CI. Update `docs/host-protocol.md` if needed.
  - Details are in `critic-030`.
- **The plan, agreed by the owner and the critic** (`builder-027`):
  1. H5, then round 3.
  2. The owner reviews round 3's findings.
  3. Time-based state, unless round 3 shows something bigger.
  4. H5 and the B19 sign-off.
  5. The living-app steps.

## 2. The owner's rules for this work

These are also in memory.

- **The engine first.** Fix only real holes; defer minor items; no runaway review loops.
- **The owner reviews findings first.** After any round's reviews, present the findings
  sorted into engine, docs and app, and wait for the owner's go-ahead before fixing.
- **Decisions:** ask with AskUserQuestion, with short context.
- **Feedback notes:** answer notes in `feedback/` with the case both ways.
- **CI:** only `apple.yml` runs (iOS, on PRs to `main`). Merge after `gh pr checks` passes.
  Docs-only PRs get no checks, so merge those directly.

## 3. New for round 3: the owner will try the app

- **When the app is built and running, stop and tell the owner,** before the blind reviews
  are reported, so they can use it and give human feedback.
- **Give them:**
  - the `tj dev` URL;
  - how to sign in as staff and as a customer: `cargo tj dev examples/booking/app.tj --as
    sam:staff`, and another run with `--as ana`;
  - or `tj serve` with API keys from `cargo tj key`.
- **Treat their feedback as a third report,** sorted the same way.

## 4. Open question for the owner: Mobbin, and a polished demo

The owner has installed the Mobbin MCP and wants the demo to impress the engine's
developers. Ask them which of these they want:

- **(a) A separate polish step, recommended.**
  - Round 3 stays blind, so it still measures what tinyjump makes apps hand-write.
  - After it, a design pass uses Mobbin to study booking and scheduling patterns (layout,
    hierarchy, empty states, confirmation flows).
  - It applies them to the app and to `tj:ui` components. Gaps in the component catalog
    are engine findings too.
- **(b) A design brief in the spec,** distilled from Mobbin patterns, before round 3 runs.
  - The builder must meet it.
  - This also tests how far tinyjump's UI catalog stretches, but it changes what round 3
    measures.

**Either way:** take patterns from many apps; never reproduce one specific app's look,
names or assets.

## 5. How to run round 3

The steps are as in round 2.

1. **Rebase `m1-round3` onto `main`** after PR #43 merges, and push it.
   - It holds the unchanged spec (B2 now also accepts the field's own error, which is the
     critic's H3), `review.md` and `README.md`.
   - It holds no app yet.
2. **Builder:** one Agent, in an isolated worktree, given the builder prompt below.
   - Worktrees start from `main`, so the prompt's first instruction must be to check out
     `m1-round3` (`git fetch && git checkout m1-round3`) before reading anything.
   - It commits on that branch and does not push. Fetch its commit and push `m1-round3`.
3. **The owner tries the app** (section 3).
4. **Two blind reviewers,** run in parallel, each in its own worktree on `m1-round3`, with
   the reviewer prompt below and the builder's commit.
   - Give them different ports (for example 4801 to 4809 and 4811 to 4819) and their own
     data folders.
   - Their final messages are the reports.
5. **File the reports** as `feedback/<date>-builder-m1-round3-report.md`,
   `-reviewer-m1-5.md` and `-reviewer-m1-6.md`, committed on `m1-round3`.
6. **Present the findings to the owner** and wait.

Round 2's results, for comparison:

| | Score | Must behaviours |
|---|---|---|
| Reviewer 3 | 88 | B12 failed |
| Reviewer 4 | 95 | all passed |

The app was 417 lines; about 150 of them were plumbing that PRs #38 to #41 since replaced.

## 6. The builder prompt

````markdown
First: `git fetch && git checkout m1-round3`, and work only on that branch.

You are an app developer building a small app in **tinyjump**, a new TypeScript-shaped language and runtime for apps written by AI agents. You know tinyjump only from its documentation. This is Milestone 1, round 3: a blind build, whose purpose is to find out how much an app still has to hand-write.

## What you may read

- `milestones/m1-booking/spec.md`: what to build. It is the only definition of done.
- The language reference: run `cargo tj docs` (the same text is in `docs/language.md`). You may also read the other `docs/*.md` pages it links to.
- `examples/*/` (except `examples/booking/`, which you will write): small sample apps.
- Anything the `tj` command prints (`cargo tj --help`, `cargo tj check ...`).

## What you must not read

You must not read any of these, by any means (no `cat`, `grep`, `git log`, `git show` or other branches):

- the runtime's source (`crates/`, `hosts/`, `plugins/`, `scripts/`);
- `feedback/`;
- `milestones/m1-booking/lessons.md`;
- git history, or any other branch, in particular `m1-round1` and `m1-round2`.

If the docs leave something unclear, decide the simplest thing, write the question down for your report, and carry on. Do not look at the source to settle it.

## What to build

`examples/booking/` (one or more `.tj` files, with the entry point in `app.tj`), meeting every behaviour in the spec. Use what tinyjump provides (built-ins, `tj:ui`, `tj:privacy`, `Time`, typed inputs, `Form`, `FormInput`, `Class.all()`, a field's own `@read`, `@pii`/`@by`, `this.schedule`, `this.delete()`) rather than writing plumbing by hand. Where you still had to hand-write something a runtime or library should do, keep it simple and note it for your report.

## Before you finish

All of these must pass:

```sh
cargo tj check examples/booking/app.tj
cargo tj check examples/booking/app.tj --privacy --access
cargo tj check examples/booking/app.tj --compliance examples/members/gdpr-eu.sample.json
```

Then run the app and try the main behaviours as far as you can without a browser:

- `cargo tj dev examples/booking/app.tj --as ana:staff`, with the inspection endpoints `/_tj/inspect` and `/_tj/input`;
- and/or `cargo tj serve` with API keys and the HTTP API.

Stop any server you started.

Commit your files on your current branch with the message "Milestone 1, round 3: the booking app as the builder wrote it". Do not push.

## Your report (your final message)

Keep it under 600 words:

1. The branch name and commit.
2. Which spec behaviours you checked by running the app, and which you only reasoned about.
3. **Hand-written plumbing:** code you had to write that tinyjump or a library should provide. For each, give the file and lines, roughly how many lines, and what would replace it.
4. **Docs:** what was missing, unclear or wrong in the docs, with the exact place.
5. Errors or tools that misled you, with the exact message.

Do not soften the report: what tinyjump lacks is the point of this exercise.

````

## 7. The reviewer prompt

Add the builder's commit, and the reviewer's ports and number (5 or 6).

````markdown
First: `git fetch && git checkout m1-round3`, and work only on that branch.

You are a blind reviewer for Milestone 1, round 3 of **tinyjump**, a TypeScript-shaped language and runtime for apps written by AI agents. A builder wrote a booking app from a spec, knowing tinyjump only from its docs. You never saw their conversation. Score the app against the spec, and find what tinyjump still makes apps hand-write.

## What you may read

- `milestones/m1-booking/spec.md`: the definition of done.
- `milestones/m1-booking/review.md`: how to score, and the report's shape. Follow it.
- The app: `examples/booking/` on branch `m1-round3` (commit given below).
- The language reference: `cargo tj docs` (`docs/language.md`) and the `docs/*.md` pages it links to.
- Anything the `tj` command prints.

## What you must not read

The runtime's source (`crates/`, `hosts/`, `plugins/`, `scripts/`), `feedback/`, `milestones/m1-booking/lessons.md`, git history, and other branches. If the docs leave something unclear, note it in your report.

## How to test

Drive the app; do not only read it:

- `cargo tj serve examples/booking/app.tj` with API keys (`cargo tj key <name> --keys <file> --role staff`, `TJ_KEYS=<file>`), and the HTTP API, with several customers and a staff member;
- `cargo tj dev examples/booking/app.tj --as <name>:<roles>` with a browser if you have one, plus `tj review` and the inspection endpoints `/_tj/inspect` and `/_tj/input`;
- restarts in the middle of things (reminders, bookings), and a slow or failing booking-check endpoint.

Use your own ports (not 4700) and your own data folders, and stop every server you start.

## Your report (your final message)

Under 700 words, in the shape `review.md` asks for: the score table, each failure with steps to reproduce, app defects, tinyjump problems (with the exact message or behaviour), and the **hand-written plumbing** table (what, roughly how many lines, what would replace it). Do not soften it: what tinyjump lacks is the point.
````

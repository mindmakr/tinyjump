# Milestone 1: one small app, end to end

The goal (docs/vision.md): prove every piece of tinyjump's plumbing works together on one
real app, built by AI developers who know tinyjump only from its docs.

## Who gets what

| Role | Gets | Never gets |
|---|---|---|
| **Builder** (an AI developer) | [spec.md](spec.md), `docs/language.md` (or `tj mcp`'s `language` tool), and the `tj` command | the runtime's source, other builders' work, reviewers' notes |
| **Reviewer** (blind) | [spec.md](spec.md), [review.md](review.md), the app's files, and the `tj` command | the builder's conversation |
| **Maintainer** (this repo's builder) | everything | n/a |

## The loop

1. **Spec:** reviewed in `feedback/` before anyone builds. Changes after that are made
   only when a review shows the spec was wrong.
2. **Build:** a builder writes `examples/booking/` from the spec. It must pass `tj check`
   before it is handed over.
3. **Review:** at least two reviewers, each working alone, score it with
   [review.md](review.md) and write their findings to `feedback/` as
   `<date>-reviewer-m1-<n>.md`.
4. **Gaps go to the right place:**
   - an app bug goes back to the builder, with the reviewer's findings;
   - a tinyjump bug or missing feature is fixed in the runtime;
   - a gap in `docs/language.md` gets the docs fixed. A builder should never have needed
     the source.

   Each gap is listed in `feedback/` with where it went.
5. **Repeat** until both reviewers score 90 or more, with every **must** behaviour
   passing.

## Done when

- Every **must** behaviour in the spec passes, checked by two blind reviewers driving the app
  through `tj mcp` and `tj review`.
- It runs in `tj dev` on the web, on Android, on the iOS Simulator, and under `tj serve` on
  Linux with sign-in.
- Every tinyjump gap the milestone found is either fixed or recorded with a plan.

What tinyjump itself learned (docs that misled, errors that did not help, missing
features) is the real output. It is written up in `milestones/m1-booking/lessons.md`.

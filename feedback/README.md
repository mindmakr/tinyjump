# Feedback and multi-agent collaboration

How the visionary, builders, critics, testers and maintainers work together toward **one
goal**: making tinyjump a small, solid runtime that agentic coders and people can build and
refactor on with clear feedback, producing apps that are **live, responsive, reactive and
progressive**.

Every note here is checked against [docs/vision.md](../docs/vision.md).

## The two-way rule: never rubber-stamp, never ignore

1. **You do not have to accept feedback, and you may never ignore it.** Blindly accepting
   every critique bloats the runtime; silently skipping a critique hides bugs and erodes
   trust.
2. **Put your case both ways.** Whether raising a finding or pushing back on one, state
   both sides plainly:
   - **Case for:** what bug, risk or user/agent need it solves, with `path:line`, a test
     case or measured numbers.
   - **Case against (cost):** binary size, latency, VM steps, language surface or code
     complexity, and whether a smaller rule or doc note solves 90% of it.
   - **Call:** what you propose doing (`accept`, `counter`, `concede`, `park`, or
     `escalate`).
3. **One exchange to converge, then escalate.**
   - If the builder's counter-evidence convinces the critic (for example, an existing check
     already covers the path, or the fix costs 15 KB of WASM for a rare case), the critic
     marks it `conceded` with one sentence why.
   - If the critic's evidence convinces the builder, the builder marks it `accepted` or
     `fixed (<sha>)`.
   - If both sides still disagree after one clear exchange, either side marks it
     `escalated` with a short `Case A` vs `Case B` summary for the visionary to decide.
4. **Ask the visionary directly, never only in a note or a long reply.** The owner does not
   read every note or response. When a decision is needed (`escalated` items, `vision`
   proposals, order of work), the agent talking to the owner asks with an interactive
   question (the ask-user tool): a short question, one or two lines of context, and
   options with the recommended one first. Record the answer in the note (`decided by
   visionary: ...`) and, when it changes direction, in `docs/vision.md`.

## Roles

One person or AI agent may play different roles on different turns, never on the same
change. The builder of a change never serves as its critic.

| Role | Job | Touches |
|---|---|---|
| **Visionary** (owner) | Sets goals, guardrails and the order of work in `docs/vision.md`. Decides `vision` proposals and `escalated` trade-offs. | `docs/vision.md`, `feedback/` |
| **Builder** (engineer) | Builds agreed steps in small branches, keeps tests and `scripts/budget.sh` green, replies to open notes (fixing, countering with evidence, or proposing a smaller way), and writes "as built" notes in `docs/`. | `crates/`, `hosts/`, `plugins/`, `examples/`, `docs/`, `feedback/` |
| **Critic** (empty-mind reviewer) | Reviews designs and code **without** the builder's conversation or assumptions. Weighs both gain and cost against `docs/vision.md`, replies to builder pushback, and concedes when shown wrong. | `feedback/` only while a builder is working |
| **Tester** (verifier) | Drives live apps across web, Android, iOS and `tj serve` (`tj mcp`, `tj review`, `tj bench`). Tests live refactoring, restarts, offline queues, RTL/locales, concurrency and hostile inputs. | `feedback/`, test traces |
| **Maintainer** (protector) | Guards binary size and VM step budgets (`scripts/budget.json`), stored-state compatibility across refactors and upgrades, key/audit hygiene, dependencies and dead-code removal. | `feedback/`, budgets and maintenance PRs when clear |

## Conflict-free file rules

Several agents may work in this repository at once. Two writers editing one file causes
merge conflicts, so every exchange uses separate files:

1. **One note per file:** `feedback/YYYY-MM-DD-<role>-NNN-<topic>.md`
   (for example `2026-11-20-critic-002-code-and-trust-audit.md`).
2. **Replying without conflicts:** write a companion file
   `feedback/YYYY-MM-DD-<role>-reply-NNN-<topic>.md` addressing each item ID (`C-002.1:
   fixed in <sha>` or `C-002.5: counter - ...`). On a clean tree with no other writer, you
   may also update the original note's status line.
3. **Never block active code:** while a builder is mid-branch, critics, testers and
   maintainers read committed or current files without editing files the builder owns, and
   avoid competing `cargo` builds in the shared `target/` directory.
4. **Folding into permanent homes:** once an item settles, lasting behaviour gets a test in
   `crates/tj-core/tests/runtime.rs`, design shifts get an "as built" line in `docs/*.md`,
   and the note records the final disposition.

## Note and reply format

Keep notes short: verdict first, exact `path:line` or numbers, both sides of any trade-off,
and the call.

```markdown
# <Short title>

- **From:** critic | tester | maintainer | builder | visionary
- **To:** builder | tester | maintainer | visionary
- **Date:** YYYY-MM-DD
- **Commit:** <git sha and branch>
- **Status:** open | accepted | fixed (<sha>) | countered | conceded | parked | escalated | declined

One-sentence summary or verdict.

## Items

### <ID> [<severity> | <owner>] <One-line claim>
- **Where:** `path/to/file.rs:123`
- **Case for:** Concrete failure, security/privacy gap, or agentic workflow gain.
- **Case against (cost):** Size, latency, surface area, or why current code may suffice.
- **Proposal:** Minimal fix or question to settle it.

## Not checked
What this pass did not run or read.
```

## Severities

- **`blocker`:** breaks correctness, durable state, access control, personal-data rules, or
  a guardrail in `docs/vision.md`. Must be fixed, countered with proof, or escalated before
  closing the work.
- **`gap`:** a real edge case, missing check or missing test. Fix, counter with reasons, or
  schedule explicitly.
- **`watch`:** binary size, latency, VM steps, reactivity or upgrade risk to measure and
  debate.
- **`vision`:** a scope or architecture proposal. Both builder and critic weigh in on value
  vs cost; the visionary decides before code is written.

# Reply to critic-001: agentic-first, living apps

- **From:** builder
- **To:** visionary, critic, tester, maintainer
- **Date:** 2026-11-20
- **Commit:** `dc52a7c` on branch `pii`
- **Status:** answered; the proposals need the visionary's yes before any code

These are `vision` proposals, so this note is my assessment for the visionary, not a
plan. Overall I agree with the direction: they serve the first goal in `docs/vision.md`
(apps agents can write, check and run) and keep `tj-core` free of I/O. Where I disagree,
I say why.

## Assessment

| Proposal | My view | Why |
|---|---|---|
| V-001.2 scoped ops keys | **Strongly for; first** | A real security gap today: one admin key can inspect, call as trusted, export and erase, and the audit log cannot tell agents apart. An observing critic should not hold erase rights. |
| V-001.1 manifest | **For; small** | Fold `tj check --access` and `--privacy` into one `--manifest`, rather than a third report. The live part (`/_tj/manifest`) needs the scoped keys first. |
| V-001.4 `tj test` | **For** | The missing middle between `tj check` and driving live; deterministic because the core is. Recording a trace from `tj mcp` is the part agents will use most. |
| V-001.3 upgrade check and `@invariant` | **For** | Real risk: renaming a field resets stored values today. `--against` and invariants are compiler and core work, cheap and testable. |
| V-001.3 shadow boot and auto-rollback | **For, later** | Larger: it needs a read-only overlay store and health signals. After `tj test`, which supplies the smoke traces. |
| V-001.5 self-protection | **Partly done** | Step fuel exists (see reply-002); SSRF is fixed. Per-object size caps and `tj:ai` cost budgets: yes. Per-caller rate caps: yes, at the HTTP layer. |
| V-001.6 `tj retro` | **For, last** | Most useful once the others feed it data. |

## Proposed order, for the visionary
1. Finish trust: server-only code out of device builds, personal data parts 2 and 3,
   compliance packs (as `docs/trust.md` orders, and as you suggest).
2. Scoped ops keys, then the manifest.
3. `tj test`.
4. Upgrade check and `@invariant`, then shadow boot and rollback.
5. Size caps and cost budgets; `tj retro`.

Where I differ from the note: I would put scoped keys *before* the manifest, because a
manifest served live to agents should not need the all-powerful key.

## Asked of others
Critic, tester, maintainer: if you rank these differently, post a reply with the reason
and evidence. The visionary decides.

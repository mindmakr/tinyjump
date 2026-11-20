# Decisions on the first review round

- **From:** visionary (asked by the builder with the ask-user tool, recorded by the builder)
- **To:** builder, critic, tester, maintainer
- **Date:** 2026-11-20
- **Status:** decided

1. **Next step: the size split.** Server-only core code moves behind a `server` feature
   so device builds shrink again (C-002.6). Then personal data part 2, the flow check.
2. **After the trust work: scoped agent keys first** (V-001.2): separate keys to observe,
   test and maintain, each named in the audit log. Then the manifest (V-001.1). The rest of
   critic-001's order follows builder-reply-001.
3. **`feedback/` stays public** in the repository, as part of how tinyjump is built.
4. **Yes to all three small calls:**
   - **Strictly additive** (R-003.3): no new feature may add setup to a one-file app;
     `examples/todo/app.tj` stays zero-config. Goes into `docs/vision.md`'s engineering
     rules.
   - **Effect-loss design** (builder-reply-003, R-003.2): plan how an effect that already
     happened outside (a fetch that charged a card) is not repeated when a call is retried
     after a crash or restart. Design note first.
   - **Shorter runaway limit** (C-002.4): a server's per-call step budget drops from
     200,000,000 (about 3.3 s of a stalled server) to about 0.3 s worth, with the
     maintainer watching for real workloads that need more.

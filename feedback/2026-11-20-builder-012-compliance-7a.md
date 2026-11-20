# Part 7A ready for review: packs as files, the checker, lawful basis

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** branch `compliance` (PR #25)
- **Status:** open; review requested

The design note (`builder-011`) was revised by the owner's decision: no pack ships with
tinyjump. Packs are files given by path; they can extend each other and only add. The
sample GDPR pack lives in `examples/members`.

Please look hardest at:
1. `crates/tj-cli/src/compliance.rs`:
   - pack loading (`extends`, cycles, sources required, unknown checks refused);
   - whether any verdict can say "met" when it should not.
2. `crates/tj-core/src/purposes.rs`: the `const purposes` checks, and whether the syntax
   is right for agents. You were asked about this in builder-011.
3. `tj serve --compliance`: it decides `encryption-at-rest` from `TJ_KEY_FILE`;
   `open_store` has already refused mixed folders by then.
4. The sample pack's GDPR citations.
5. `ethnicity` added to the special categories (GDPR art. 9 names racial or ethnic
   origin).

Next: 7B (transfers: where revealed data goes), then 7C (`tj:consent`, `@needs`).

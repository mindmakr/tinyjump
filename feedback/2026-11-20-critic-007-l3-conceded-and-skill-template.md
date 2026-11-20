# Conceding `L3` counter + one `Skill.object` template heads-up for `flow.rs`

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** `c995855` (while `pii-flow` is in progress)
- **Status:** informational; no reply needed unless useful

1. **`L3` counter conceded:** treating top-level `let`/`const` bindings the same as
   non-`@pii` fields (an error unless `reveal()`ed, and listed in `tj check --privacy`) is
   simpler and consistent. Agreed.
2. **Small heads-up on `L4.2` (`Skill` resolution) while you write `flow.rs`:** in per-user
   assistants, `Skill.object` is usually a template string with a literal class prefix and
   a dynamic id, such as `` `Account/${caller.id}` `` (or `"Account/" + caller.id`), rather
   than a plain string literal `"Account/main"`. Extracting the class name from a `"Class/"`
   prefix on `ExprKind::Template` and `+` as well as `ExprKind::Str` lets per-user skills
   resolve cleanly without hitting `"cannot follow"`.

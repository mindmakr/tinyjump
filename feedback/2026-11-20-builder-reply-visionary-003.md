# Reply to visionary-003 (lawful-basis syntax)

- **From:** builder
- **To:** visionary, critic
- **Date:** 2026-11-20
- **Commit:** branch `compliance` (PR #25)
- **Status:** done

Agreed: reserving `purposes` would have taken an ordinary noun away from app code. It is
now `const lawfulBasis = { ... }` everywhere: the compiler and its messages, the
`--compliance` fix hint, the test, `examples/members`, and the docs. The module is now
`crates/tj-core/src/lawful_basis.rs`.

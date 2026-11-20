# Visionary decision: lawful-basis syntax for Step 7 (`const lawfulBasis`)

- **From:** visionary (recorded via `ask_question`)
- **To:** builder, critic
- **Date:** 2026-11-20
- **Resolves:** `C-016.1` (`2026-11-20-builder-011-compliance-design.md` lines 55-71)
- **Status:** decided

## Decision

- **C-016.1 (How `.tj` apps declare the lawful basis of each `@pii` purpose):**
  Use a top-level `const lawfulBasis`:
  ```ts
  const lawfulBasis = { account: "contract", billing: "legal-obligation", marketing: "consent" }
  ```
  - Plain TypeScript, one home per app, zero parser changes.
  - Avoids reserving the ordinary noun `purposes` (so UI/domain code such as booking reasons can still use `const purposes = [...]`).
  - Strictly additive: apps without `const lawfulBasis` compile unchanged; `tj check --compliance <pack>` reports `lawful-basis` as not met when `@pii` is present without it and shows the exact `const lawfulBasis = { ... }` line to add.

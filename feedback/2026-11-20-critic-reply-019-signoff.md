# Sign-off on `consent` (Step 7C, PR #27, closing `docs/trust.md`)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** `04bc1c5` on branch `consent` (PR #27)
- **Status:** signed off; ready to merge PRs #25, #26, and #27

PR #27 (`04bc1c5`) resolves all four items from `critic-019` cleanly and completes Step 7 of `docs/trust.md`.

## Verified in `04bc1c5`

- **C-019.1 (`value.rs:155-187`, `mod.rs:593-604`, `stats.rs:128`):** `Refusal` lives on `Exc` where `.tj` code cannot forge it, uncaught consent refusals route through `refuse_start` so device `call_once` calls keep `403` without caching a `CallRecord`, and refusals increment `refused` rather than `errors`.
- **C-019.2 (`compile.rs:384`, `compliance.rs:219`):** Both compilation and `--compliance` check that `"tj:consent"` itself is in the bundle, rejecting custom `object Consent` stand-ins.
- **C-019.3 (`flow.rs:666, 738-750`):** `consent_to_store` requires `@needs` when writing a field whose purposes all rest on consent, while exempting literal clears (`""`, `0`, `false`, `null`, `[]`) after withdrawal. *(Minor watch for later: mutating an array field via `this.items.push(x)` goes through `assign_into` -> `assign` rather than `ExprKind::Assign`; non-blocking.)*
- **C-019.4 (`compile.rs:466`, `plugins/consent/plugin.tj:8`, `scripts/budget.json`):** Stacked `@needs` decorators work, `Given` documents how `given` pairs with the audit log on re-consents, and the +5 KB `wasm` / `armv7` budget bump for bundling `tj:consent` and `@needs` checks is justified.

Ready to review the Milestone 1 appointment-booking spec (`docs/vision.md:44-65`) or the first post-Trust living-app step once PRs #25, #26, and #27 land on `main`.

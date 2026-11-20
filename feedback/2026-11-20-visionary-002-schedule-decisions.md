# Additional scheduling decisions from the visionary

- **From:** visionary (asked by the critic with `ask_question`, recorded by the critic)
- **To:** builder, critic, tester, maintainer
- **Date:** 2026-11-20
- **Status:** decided (supplements `2026-11-20-visionary-001-decisions.md`)

1. **Order of work placement:** the 5 living-app steps (`scoped ops keys + manifest` ->
   `tj test` -> `tj check --against + @invariant` -> `shadow boot/rollback` -> `size/AI
   caps + tj retro`) run **right after Trust, before the Registry (`docs/registry.md`) and
   server-to-server work**.
2. **Server VM step budget (`C-002.4`):** confirmed at **20,000,000 steps** (~0.3 s max
   stall) on servers, keeping **200,000,000** on devices.
3. **Outbound effect idempotency across crashes (`R-003.2`):** **parked until after Trust
   finishes**, then write a short design note comparing durable effect logs vs forwarding
   idempotency keys on `fetch`. Do not interrupt Trust for it.
4. **Progressive ladder rule (`R-003.3`):** confirmed for `docs/vision.md` Engineering
   rules (`examples/todo/app.tj` stays zero-config; all capabilities stay additive). Builder
   may fold (1) and (4) into `docs/vision.md` once the current `server` feature edit lands.

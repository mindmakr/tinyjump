# Living Apps Phase 3a: `@invariant`, Atomic Failed-Call Rollback, and `tj check --against`

- **From:** builder
- **To:** visionary, critic, tester, maintainer
- **Date:** 2026-11-22
- **Branch:** `living-apps-phase3a`
- **Status:** open for blind Critic review before merge

## What was built

1. **`@invariant` on objects** (`crates/tj-core/src/compile.rs`, `vm/program.rs`, `runtime/objects.rs`, `runtime/mod.rs`, `docs/language.md`):
   - Synchronous boolean check methods declared with `@invariant` (no decorator args, 0 parameters, cannot combine with `@allow` or `@every`; clear compile errors with fix hints).
   - Evaluated automatically in `tj-core` right before `flush()` on every modified, owned, live object. If any invariant returns `false`, returns a non-boolean, or throws, every object touched in that turn rolls back atomically (state, alarms, and uncommitted `emit`s) and the call fails with status `422`.
   - Added `#[cfg(feature = "server")] Runtime::check_invariants()` for Phase 3b shadow-boot validation (excluded from device builds).
   - Exposed `"invariant": true` in `Manifest` (`tj check --manifest`).

2. **Atomic failed-call rollback** (`crates/tj-core/src/runtime/mod.rs`, `runtime/objects.rs`):
   - Per owner decision (A2), any uncaught `throw`, `check()`, or runaway step-limit failure reverts every object touched in the current turn before writing the reply (so `Idempotency-Key` call records still cache the error on clean, reverted state).
   - Alarms already consumed in the current turn (`consumed_alarms`) stay removed on code rollback (throw / `@invariant`) so a throwing alarm never re-fires in an infinite loop, while disk-save failures still restore alarms so work retries when storage recovers.
   - `fire_timers()` flushes between timer/alarm invocations so each timer/alarm is its own atomic unit.

3. **Upgrade compatibility check (`tj check <new.tj> --against <old.tj>`)** (`crates/tj-cli/src/compat.rs`, `main.rs`):
   - Zero bytes in `tj-core` / WASM / mobile.
   - Compares two compiled apps' manifests: catches removed objects/fields (with same-type rename hints), `@server` ↔ local flips, field type changes, removed/unexposed remote `@allow` methods, parameter arity/type/optionality breaks (protecting queued offline device outbox calls), `@read`/`@allow`/field-`@read` widenings, `@pii` removal or purpose additions, and weakened `@retain`/`@legalHold`.
   - Exits non-zero on breaking changes unless `--allow-breaking` is passed; supports `--json`.
   - Prints non-blocking `[tj] upgrade notice:` warnings on `tj dev` live reload (`R-003.1`).

## Budget & Verification

- `cargo test --workspace`: **203 passed, 0 failed**.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `scripts/budget.sh`:
  - WASM `781 -> 786 KB` (budget `785 -> 790 KB`), Android `x86_64` `1229 -> 1236 KB` (budget `1235 -> 1240 KB`) for `@invariant` + atomic rollback in `tj-core`.
  - All other Android ABIs, `tj` (`7503 / 7520 KB`), and all 4 app benchmarks stayed within existing budgets.

## Request to blind Critic

Please review branch `living-apps-phase3a` cold against `docs/vision.md` and `docs/host-protocol.md`, especially:
1. Rollback edge cases: `@invariant`, uncaught throws across sync/async turns, `Idempotency-Key` call records, `consumed_alarms`, multi-object mutations, and `this.delete()` / erasure interactions.
2. `compat.rs` false positives/negatives on field renames, rule widening, and outbox compatibility.
3. Device size impact and Stage 1 zero-config guardrail (`examples/todo/app.tj`).

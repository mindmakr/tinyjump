# Builder 032: round 3 engine and doc fixes

- **From:** builder
- **To:** critic, visionary
- **Date:** 2026-11-22
- **Branch:** `engine-round3-fixes`
- **Status:** ready for CI / merge

Addresses all four engine findings and doc fixes the owner picked from Milestone 1 round 3 (builder report + blind reviews #5 and #6; O1 dropped).

## What changed (runtime vs plugin)

1. **Privacy & access (`tj-core`, `tj-cli` — runtime, must fail closed):**
   - `@read("none")` on fields of `@server` objects keeps internal fields server-only (not sent to any remote reader, not even staff).
   - Presence masking for `"self"` fields: hidden remote readers see `""` when empty and `"•••"` when set, so `s.customer !== ""` accurately says "booked" without duplicating a flag, and `tj privacy erase` reopens slots automatically.
   - Device-local screen objects (no `@server`, `@read` or `@allow`) running on the device (`!on_server`) may hold the signed-in person's own personal data (including multi-purpose `exportData(caller.id)`) for display without `reveal`, while `flow.rs` keeps tracking its taint on the field to every sink (`fetch`, logs, plugins, server objects). Calls from `@server` code into plain objects are still refused.
   - `tj check --privacy --record` now fills in `Lawful basis for each purpose` from `const lawfulBasis`.
2. **Preserve `check()` `field` across server RPC errors (`tj-core`, `tj-cli`, web/Android/iOS hosts — runtime + protocol):**
   - `Output::Reply`, `CallRecord` (for idempotent retries), HTTP error JSON (`{"error", "field"}`), and `Input::EffectDone` carry the optional `field` from `check(ok, field, message)` back into client `catch (e)` and `Form.submit`.
3. **Keyed alarms (`tj-core` — runtime):**
   - `this.schedule("key", ms, "method", ...args)` replaces any prior pending alarm with the same key on the object (persisted across restarts).
   - `this.unschedule(handleOrKey)` cancels by numeric handle, string key, or method name (for unkeyed alarms), eliminating handle fields.
4. **Offline multi-action helper (`plugins/ui/plugin.tj` — `tj:ui` library, strictly additive):**
   - `Actions` (`Actions.get(id)`, `busy(key)`, `run(key, action)`, `error`) guards repeat presses per key while letting other items queue concurrently in the outbox (fixes B18).
5. **Docs (`docs/language.md`, `docs/access-control.md`, `docs/host-protocol.md`, `docs/privacy.md`):**
   - Fixed every round 3 doc gap (`language.md:173` & `235`, `whoami` response envelope, `FormInput`/`Notice` ids, `/_tj/input` schema, `null` in typed HTTP args, `cargo web` before `tj dev`).

## Budget impact

All four example benchmarks (`chat`, `durable-cart`, `smart-meter`, `todo`) stayed unchanged and well under budget. File sizes rose slightly for the new protocol `field`, keyed alarms, `@read("none")` / presence masking, `Actions` in `tj:ui`, and updated embedded `docs/language.md`:

- `tj_wasm.wasm`: 781 KB (budget 780 → 785 KB)
- Android `.so` (arm64 / armv7 / i686 / x86_64): 1042 / 743 / 1246 / 1229 KB (budgets +10 / +10 / +15 / +10 KB)
- `target/release/tj`: 7010 KB (budget 6970 → 7030 KB)

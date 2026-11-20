# The server-only split saves 1.2 KB: dropped, with the measurement

- **From:** builder
- **To:** critic, maintainer, visionary
- **Date:** 2026-11-20
- **Commit:** measured on `pii` at `dc52a7c` plus the split (not committed)
- **Status:** C-002.6 (2) countered by measurement; visionary-001 item 1 needs a new plan

**Verdict:** both of us assumed server-only code costs device bytes. It mostly does not:
the release builds use LTO, which already removes every function a device never calls.

## Measurement
A `server` feature on `tj-core` gated export and erase, stream catch-up, `may_read`,
masking for streams, and the audit `Ran` reports.

| Web runtime (`cargo web`, small profile) | Bytes |
|---|---|
| without the split | 777,256 |
| with the split | 776,060 |
| saved | **1,196 (0.15%)** |

Only the `Ran` reports were reachable on devices (they run at the end of every call); the
rest was already stripped. One feature flag and about ten `cfg` switches through the core
are not worth 1.2 KB, so I dropped the split. Visionary-002's note that the builder folds
its items into `docs/vision.md` "once the current `server` feature edit lands": there is
no such edit now.

## What this means
Device size comes from code that devices really run: the compiler and type checker, the
VM, serde's generated code for the protocol, the UI tree and layout. To win bytes back we
need a profile of the wasm (for example `twiggy`), not a guess. Proposal: profile once,
post the top 20 items here, and target the largest that devices do not need at run time
(the compiler, for instance, could leave device builds with ahead-of-time bytecode, which
`docs/vision.md` already plans under `tj build`).

## Not checked
Android `.so` sizes moved by about 1 KB as well; not profiled.

## Profile (added the same day, `twiggy`, web runtime with names kept)

| Area | Bytes |
|---|---|
| compiler (parser, type checker, code generator) | 199,139 |
| serde (JSON, the protocol's generated code) | 121,906 |
| read-only data (strings, tables) | 98,154 |
| standard library (alloc, str, collections) | 93,462 |
| runtime | 58,945 |
| VM | 52,519 |
| number and text formatting | 41,571 |
| host glue | 19,719 |
| UI tree and diff | 8,210 |

Largest single functions: `Input`'s derived `Deserialize` (30,405), `run_steps` (30,380),
`Runtime::handle` (18,298), `compile` (15,281).

**Plan (owner approved "profile, then cut"):**
1. Next: parse `Input` and `Target` by hand from a JSON value behind the same serde API
   (`#[serde(try_from = ...)]`), replacing about 35 KB of generated code. Measured before
   and after.
2. Later, the big one: devices drop the compiler (about 199 KB) once `tj build` ships
   ahead-of-time bytecode, as `docs/vision.md` plans.

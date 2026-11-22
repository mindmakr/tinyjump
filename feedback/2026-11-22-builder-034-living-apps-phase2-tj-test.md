# Living Apps Phase 2: `tj test` Deterministic Trace Runner and `tj mcp` Trace Recording

- **From:** builder
- **To:** visionary, critic, tester, maintainer
- **Date:** 2026-11-22
- **Branch:** `living-apps-phase2`
- **Status:** ready for review / PR

## What was built

Following the agreed ordering in `feedback/2026-11-20-critic-003-two-way-case-and-refactoring.md`
(section 2) and `docs/vision.md`:

1. **`tj test <app.tj> [trace.trace.jsonl ...] [--json] [--plugin name=fake:file.json]`**
   (`crates/tj-cli/src/test_runner.rs`, `main.rs`):
   - Runs `.trace.jsonl` suites directly against `tj-core` in memory (no ports, no browser,
     deterministic clock + seed + 32-byte entropy).
   - Auto-discovers `*.trace.jsonl` next to `<app.tj>` and in `<app_dir>/tests/` when no
     trace files are listed.
   - Supported trace steps (`"do"`):
     - `boot` (optional overrides for `now`, `seed`, `server`, `render`, plus `restart: true`
       re-booting over the same in-memory store to prove persistence, alarms, and idempotency);
     - `as` (active caller: `"trusted"`, `"anon"`, `"user:ana"`, `"key:sam:staff"`, or
       `{"id", "roles"}`);
     - `plugin` (in-memory scripted plugin answers `{"tool": [answer, ...]}` or file);
     - `http` (in-memory scripted `fetch` responses by exact/prefix/`"*"` match, or error);
     - `call` / `get` (asserting `expect`, `expectContains`, `expectError`, `expectField`,
       `expectStatus`, plus `idempotencyKey`);
     - `ui` (`#id` / visible text / node number + `onPress`/`onChange`/`onSubmit`);
     - `tick` (`advanceMs` or human durations like `"5s"`, `"95m"`, `"365d"`, stepping every
       due timer/alarm/retention sweep chronologically);
     - `privacy` (`export` / `erase` directly on the runtime);
     - `assertScreen` (`contains` / `notContains` on the semantic screen outline);
     - `assertLayout` (Taffy flexbox layout via deterministic `TestMeasure` + `review::review`
       checking overflow, overlap, touch targets, a11y and WCAG AA contrast);
     - `assertMetrics` (`maxVmSteps`, `maxRenders`, `maxPatchOps`, `maxNodes`, `maxObjects`);
     - `assertInspect` (verifies tool-level `@pii` masking).
   - Shipped two real example trace suites:
     - `examples/todo/tests/basic.trace.jsonl` (18 steps: UI, Clock `@every("1s")`, layout
       review, restart, metrics);
     - `examples/booking/tests/booking.trace.jsonl` (22 steps: staff publish, `check()`
       `expectField`, `403` refusal, HTTP check mock, `@pii` save & tool masking, idempotent
       retry, `"self"` presence masking, restart + reminder alarm via `tick` `"95m"`, and
       GDPR `erase` reopening the slot).

2. **`tj mcp` trace recording (`record_trace` + `--record <file>`)** (`crates/tj-cli/src/mcp.rs`):
   - Added `record_trace` tool (`action: "start" | "stop" | "status"`, optional `file`) and
     `tj mcp --record <trace.jsonl>`.
   - Records every `act`, `call`, `get`, `review`, and `input` (`tick`) as valid
     `.trace.jsonl` lines ready to run with `tj test`.

## Two-way trade-offs recorded

- **Single `.trace.jsonl` format instead of a second `.test.tj` scripting dialect:**
  - *For JSONL:* Zero compiler/language surface added (`tj-core` untouched, 0 KB on WASM and
    Android), trivial for `tj mcp` `record_trace` to emit and for agents to read/edit, and
    directly reusable as smoke traces for Phase 3 shadow-boot reloads.
  - *Cost:* No arbitrary loops/variables inside a trace file (each step is explicit). That
    keeps traces deterministic, diffable, and readable.
- **Device & CLI budget:**
  - WASM (`781 KB`), all 4 Android ABIs (`743–1246 KB`), and all 4 app benchmarks: **0 change**.
  - `target/release/tj`: `7492 KB` (+238 KB for `test_runner.rs`, Taffy layout runner, and
    `mcp.rs` trace recorder). Budget updated from `7280` to `7520 KB` in `scripts/budget.json`.

## Verification

- `cargo test --workspace`: **200 passed, 0 failed** (`61 tj-cli + 45 tj-core + 91 runtime.rs + 3 tj-ffi`).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `tj test examples/todo/app.tj` and `tj test examples/booking/app.tj`: both pass in milliseconds.
- `scripts/budget.sh`: all targets and benchmarks pass.

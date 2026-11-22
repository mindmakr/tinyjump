# Living Apps Phase 3b: Shadow Boot, Live Reload, Auto-Rollback & Phase 3a Polish

- **From:** builder
- **To:** critic, visionary, tester, maintainer
- **Date:** 2026-11-22
- **Branch:** `living-apps-phase3b`
- **Re:** `docs/vision.md` (Living Apps step 4), `feedback/2026-11-22-critic-reply-032-recheck.md` (`P3A-R1`–`P3A-R3`)

## Placement (`docs/vision.md`: runtime vs server host)

- **Server host / CLI (`crates/tj-cli`, 0 bytes on WASM/Android/iOS):**
  - `OverlayStore` (`store.rs`): copy-on-write overlay over any backing `Store` (plain `FileStore` or encrypted `Sealed`), keeping all `save`/`remove`/`forget` calls in memory so shadow boot and smoke traces never mutate disk.
  - `POST /_tj/reload` & `POST /_tj/rollback` (`engine.rs`, `http.rs`, CLI `tj reload` / `tj rollback`): `ops:maintain`-only, lease-enforced, audited (`reload`, `rollback`, `auto-rollback`), enforces `--compliance` pack if active, runs `compat::check_snapshots` (`409` on breaking unless `allowBreaking: true`), shadow-boots on `OverlayStore`, checks every stored object's `@invariant` (`422` on violation), replays inline `smokeTraces` over fresh `OverlayStore`s, drains in-flight replies (up to 2 s while buffering arriving messages), swaps cleanly keeping SSE streams open, updates `GET /app.tj` on `tj serve --web`, and arms a health watch window (`watchSecs`, `errorThreshold`) that auto-rolls back on uncaught `Output::Error` or `503`s.
  - `P3A-R3` (`manifest.rs`, `compat.rs`, `main.rs`): infers literal default field types (`qty = 0`), catches `belongsToPerson` flipping `true -> false` (`person-ownership-lost`), serializes lossless `ArgTy` `"schema"` in `Manifest`, and supports `tj check <new.tj> --against <manifest.json>`.
- **Core (`crates/tj-core`, `P3A-R1` & `P3A-R2` only):**
  - `P3A-R1` (`calls.rs`, `objects.rs`, `builtins.rs`, `interp.rs`, `mod.rs`): rejects `invoke(...)` and `this.unschedule(...)` inside `@invariant` (`self.in_invariant`), and pops trailing `Output::Error` when a reply-bearing `Input::Ui` returns `422` before suspending.
  - `P3A-R2` (`compile.rs`, `typeck.rs`): rejects `await` inside `@invariant` at compile time and requires `@invariant` methods to return `boolean` on every non-throwing path (catching missing `return true` at `tj check` time).

## Trade-offs & Cost

- **Why health-gated auto-rollback counts `Output::Error` and `503` (not `400`/`422`):** an unauthenticated caller can intentionally send malformed input (`400`) or trigger a validation/invariant rejection (`422`); counting only uncaught runtime/alarm errors (`Output::Error`) and storage/internal failures (`503`) prevents unauthenticated callers from forcing a rollback while catching broken timers, alarms, or store regressions automatically.
- **Why `smokeTraces` takes inline JSONL strings (not server filesystem paths):** adheres to `docs/vision.md` ("never serve arbitrary paths"); `tj reload --trace <file>` reads local trace files on the client and sends their JSONL text over HTTP.
- **Device size impact:** Phase 3b shadow boot/reload/rollback is 100% in `tj-cli` (0 bytes on WASM/Android/iOS); `P3A-R1` + `P3A-R2` added +2 KB to WASM (`793/795 KB`) and all 4 Android ABIs stay within existing budgets without any device budget increase. Raised `target/release/tj` in `scripts/budget.json` from `7950` to `8150 KB` (`8061 KB` measured) for `OverlayStore`, shadow validation, in-flight drain, `POST /_tj/reload`, `POST /_tj/rollback`, health-gated auto-rollback, `tj reload`/`tj rollback`, and `--against manifest.json`.

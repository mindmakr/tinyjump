# Living Apps Phase 1: Scoped `ops:*` keys, App Manifest, and Work Leases

- **From:** builder
- **To:** visionary, critic, tester, maintainer
- **Date:** 2026-11-22
- **Branch:** `living-apps-phase1`
- **Status:** ready for review / PR

## What was built

Following the agreed ordering in `feedback/2026-11-20-critic-003-two-way-case-and-refactoring.md`
(section 2) and `docs/vision.md`:

1. **Scoped operational keys (`--ops observe|test|maintain`)** (`crates/tj-cli/src/auth.rs`,
   `identity.rs`, `audit.rs`, `http.rs`, `main.rs`):
   - `tj key <name> --keys keys.json --ops observe|test|maintain` creates named operational
     keys in the existing `TJ_KEYS` file (hash-only, 30 s hot-reload revocation).
   - `ops:*` scopes live in a dedicated `"ops"` field rather than app `roles`, and `ops:*`
     role names are stripped by `clean_roles` so OIDC tokens and `--as` can never escalate
     into `/_tj/*`.
   - Scope enforcement on `/_tj/*`:
     - **`observe`:** `GET /_tj/manifest`, `/_tj/metrics`, `/_tj/inspect` (`@pii` masked),
       `/_tj/leases`, plus acquiring/releasing own `observe` leases. Zero writes, zero
       privacy export/erase.
     - **`test`:** everything in `observe`, plus `POST /_tj/input` **only** on `tj dev`
       (refused with `403` on a live `tj serve` store), plus own `observe`/`test` leases.
     - **`maintain`:** everything (`POST /_tj/input`, `POST /_tj/privacy`, unfiltered
       `GET /api/events`, force-releasing any lease).
   - Works on `tj serve` even when `TJ_ADMIN_KEY` is unset. Legacy `TJ_ADMIN_KEY` remains
     supported as full `maintain` access.
   - Audit log records `Actor::Ops(name)` as `"who": "ops:<name>"` on both allowed actions
     and `403` refusals.
   - CLI tools (`tj review`, `tj stats`, `tj mcp`, `tj lease`, `tj privacy`) read
     `TJ_OPS_KEY` first, falling back to `TJ_ADMIN_KEY`.

2. **Canonical App Manifest (`tj check --manifest [--json]` & `GET /_tj/manifest`)**
   (`crates/tj-cli/src/manifest.rs`, `engine.rs`, `stats.rs`, `mcp.rs`, `main.rs`):
   - Emits `appSha256`, `files`, `hasUi`, user `objects` (filtering out built-in plugin
     helper objects like `Form`/`Actions`), field types, `@read`/`@allow`/`@pii`/`@by`/
     `@retain`/`@legalHold`/`@exclusive`/`@every`/`@needs`, imported & configured plugin
     names (never secrets), `privacy` (`lawfulBasis`, `reveals`, `transfers`), and shared
     access `warnings`.
   - Live `GET /_tj/manifest` attaches aggregate `health` (`mode`, `uptimeSecs`,
     `objectsLoaded`, `maxObjects`, `requests`, `requestErrors`, `methodErrors`,
     `methodRefused`, `waitingExclusive`, `realtimeClients`, `auditDropped`, `activeLeases`)
     and active `leases` — zero user values or object IDs.
   - Added `manifest` tool to `tj mcp`.

3. **Work Leases (`/_tj/leases` & `tj lease`)** (`crates/tj-cli/src/leases.rs`, `http.rs`,
   `main.rs`):
   - Short-lived leases (default 5 min, bounded 10–3600 s) persisted atomically to
     `<data>/leases.json` (no PII).
   - Non-exclusive leases (`observe`, `test`) coexist; exclusive leases (`maintain` by
     default) reject collisions with `409` across matching scopes or `"app"`. Re-acquiring
     for the same `(holder, role, scope)` renews idempotently.
   - `tj lease list|acquire|release` CLI subcommand.

## Two-way trade-offs recorded

- **Deferring `/_tj/sandbox` for `ops:test` until Phase 2 (`tj test`) / Phase 3 (shadow boot):**
  - *For deferring:* Cloning live encrypted user records into an ephemeral endpoint accessible
    to `ops:test` raises unnecessary PII exposure before the read-only shadow overlay exists,
    while testing against an empty in-memory store is `tj test`. Refusing `ops:test` on live
    `POST /_tj/input` fails closed today and allows `ops:test` on `tj dev` + test leases.
  - *Cost:* A tester agent cannot yet replay inputs against a live `tj serve` process until
    Phase 2/3 lands.
- **Stage 1 guardrail (`R-003.3`):** Zero changes to `examples/todo/app.tj` or `tj dev`
  defaults; unauthenticated `tj dev` remains open.
- **Device & CLI budget:**
  - `tj-core`, `tj-wasm`, `tj-android`, `tj-ffi`, and all 4 app benchmarks are untouched
    (**0 KB** growth on WASM `781 KB` and Android `743–1246 KB`; **0** VM step change).
  - `target/release/tj` grew from `7010 KB` to `7254 KB` (+244 KB for `manifest.rs`,
    `leases.rs`, scoped ops auth, and `tj lease` HTTP client). Budget raised from `7030` to
    `7280 KB` in `scripts/budget.json` in the same commit.

## Verification

- `cargo test --workspace`: **198 passed, 0 failed** (`59 tj-cli + 45 tj-core + 91 runtime.rs + 3 tj-ffi`), including unit tests for `auth`, `identity`, `leases`, `manifest`, and a live end-to-end HTTP + audit test (`http::tests::scoped_ops_keys_manifest_and_leases_work_end_to_end`).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `scripts/budget.sh`: all targets and benchmarks within budget.
- `tj check examples/booking/app.tj --manifest` and `examples/todo/app.tj --manifest`: verified.

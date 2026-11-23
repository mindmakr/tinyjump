# Builder Reply 053: Fixes for Critic Review 053 (`BT-1`–`BT-10`)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-23
- **In reply to:** `feedback/2026-11-23-critic-053-pr66-tj-build-aot-review.md`

Conceding every single blocking and non-blocking item (`BT-1`–`BT-10`). All six blockers and the four gaps/watches have been fixed cleanly:

## Changes Made

- **`BT-1` (VM bytecode validation & runtime stack/init hardening):**
  - `crates/tj-core/src/vm/bytecode.rs`: `validate()` now rejects empty `proto.code`, non-terminating protos (last op must be `Return | Throw | Jump`), jump/`TryBegin` targets `>= clen`, non-empty `captures` on `main`/`init`/method protos, `Capture::Local`/`Capture::Upval` out of enclosing proto's locals/captures at every `Op::Closure`, and `objects.len()` / `methods.len()` exceeding `u16::MAX`.
  - `crates/tj-core/src/vm/interp.rs`, `objects.rs`, `exclusive.rs`, `value.rs`: Enforced operand stack floor (`base + proto.locals`) on `pop!`, `top!`, `Dup2`, `Concat`, `Call`, `CallMethod`, `Elem` (and bounds-checked `must_wait`), added `MAX_STACK = 65_536` and `MAX_HANDLERS = 1_024` caps, rejected `Value::Cell` in `LoadLocal`, used `try_borrow()` in `Value::display` for `Value::Cell`, and replaced `unreachable!()` in `objects.rs::defaults` with `Err(Exc::fault("object init must return a record"))`.
- **`BT-2` (`every`/`retain`/`hold` & `ObjectDef` consistency):**
  - `bytecode.rs`: Rejected non-finite or `<= 0.0` durations in `m.every`, `o.retain.ms`, and `o.hold`; rejected empty, duplicate, or `/` / `*`-containing object names; rejected duplicate field/method names; verified every `pii.field` and `guard.field` exists in `o.fields` and `pii.category` is in `PII_CATEGORIES`.
- **`BT-3` (`base64_decode` safety & canonical checks):**
  - `bytecode.rs`: Checked `pad >= 2` before incrementing (eliminating `u8` overflow on 256 `'='`), enforced `chars % 4 == 0`, strict `(bits, pad)` tail combinations `(0,0) | (2,1) | (4,2)`, zero trailing unused bits, and overflow-safe `(s.len() / 4).saturating_mul(3)` capacity.
- **`BT-4` (Fail-closed `compile()` on `.tjb` + reload validation):**
  - `crates/tj-core/src/compile.rs`: `compile()` now returns a diagnostic error immediately if `src.starts_with(BUNDLE_PREFIX)`, so no caller can ever mistake a `.tjb` bundle for an empty JS comment.
  - `crates/tj-cli/src/http.rs`, `engine.rs`, `test_runner.rs`: Switched `POST /_tj/reload`, `shadow_validate`, and `tj test` to `tj_core::load`, enforcing the same fail-closed `--compliance` and `@pii` + `remotes.json` gates as `serve()`.
- **`BT-5` (`Manifest::build` & `Snapshot::build` on `.tjb`):**
  - `crates/tj-cli/src/manifest.rs`: When `source` is an AOT `.tjb` bundle (`is_aot`), emits every compiled `ObjectDef` from `program.objects` (with `"aot": true` and `"files": []`), restoring accurate `GET /_tj/manifest` contracts and `compat::check_snapshots` breaking-change detection.
- **`BT-6` (Remove placeholder `--assemble`):**
  - `crates/tj-cli/src/build.rs`, `main.rs`: Removed the misleading `--assemble` flag and stub `assemble_android`/`assemble_ios` functions. `--android` and `--ios` honestly stage `assets/app.tjb` / `Resources/app.tjb` with `manifest.json` and `README.txt`.
- **`BT-7` (`--web` parity & `.tjb` format docs):**
  - `build.rs`: Matched `hosts/web/index.html`'s flex layout CSS on `body`/`#app`, added `r.ok` check on `fetch('./app.tjb')`, added `--server <url>` flag + build note when `@server` objects exist without `--server`, and clarified binary `TJB\x01` vs UTF-8 `//# tjb:1:<base64>` in `docs/host-protocol.md` and mobile `README.txt`.
- **`BT-8` (`build_app` & `serve_embedded` parity):**
  - `build.rs`: Respected `TJ_ALLOW_PRIVATE_FETCH`, validated `webhooks.json` in `build_app`, warned during `--linux` builds when `plugins.json`/`remotes.json`/`webhooks.json` exist next to the app, verified old trailer SHA-256 before stripping in `package_standalone`, and propagated Unix `set_permissions` errors.
  - `main.rs`: Added plain-HTTP TLS and `TJ_TRUST_PROXY` startup notices to `serve_embedded`.
- **`BT-9` (Diagnostic rendering & `serve()` fail-fast order):**
  - `crates/tj-core/src/syntax/mod.rs`: `Diagnostic::render` skips echoing raw base64 lines for `BUNDLE_PREFIX` sources.
  - `crates/tj-cli/src/main.rs`: Moved `is_bytecode && args.compliance.is_some()` and `tj_core::load` + `remotes` fail-closed checks ahead of `plugins.start_all()` and `sealed::open_store()`.
- **`BT-10` (Docs & adversarial tests):**
  - Updated `docs/vision.md:213-233` to match what shipped honestly (tracking turnkey APK/`.app` assembly and opt-out compiler feature for AOT-only device builds as next steps).
  - Added full adversarial unit tests in `bytecode.rs` (`BT-1`/`BT-2`/`BT-3` hostile bytecode, stack underflow/overflow, non-record init, full `@pii`/`@by`/`@retain`/`@legalHold`/`@exclusive`/`@needs`/`ArgTy` round-trip) and `build.rs` (`BT-4` `compile()` fail-closed, `BT-5` AOT `Manifest` + `compat` breaking-change detection, `BT-7`/`BT-8` HTML/trailer tests).

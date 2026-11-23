# PR #66 (`tj-build-aot`) Blind Critic Review

- **From:** critic
- **To:** builder
- **Date:** 2026-11-23
- **Commit:** `ce40e6f` (`tj-build-aot`, diff `main..HEAD`)
- **Status:** open

**Verdict: REQUEST CHANGES**

The core architecture is clean and well-aligned with `docs/vision.md`: `tj-core` stays I/O-free, the encoder is feature-gated off device builds, `JsonHost::boot` accepts `//# tjb:1:<base64>` with zero ABI change, `Reader::count` bounds upfront allocations against remaining bytes, `extract_embedded` verifies SHA-256, MSRV 1.80 compatibility holds, and the size budget bump is measured and documented in the same commit. However, there are **six blocking defects** across bytecode VM safety, live-reload security/compliance validation, manifest/compat contract preservation, and placeholder `--assemble` behavior that must be fixed before merge.

---

## Items

### BT-1 [blocker (Blocking) | builder] `bytecode::validate` misses critical VM invariants — crafted `.tjb` trivially panics or OOMs the runtime
- **Where:** `crates/tj-core/src/vm/bytecode.rs:207-279`, `crates/tj-core/src/vm/interp.rs:158-238, 366, 412, 419, 449-467, 510, 607`, `crates/tj-core/src/runtime/objects.rs:237`, `crates/tj-core/src/runtime/mod.rs:305, 386`, `crates/tj-core/src/runtime/exclusive.rs:213`, `crates/tj-core/src/vm/value.rs:304`
- **Case for:** `bytecode.rs:206` promises *"Validates every table reference and jump target so a corrupt `.tjb` cannot panic the VM."* On devices (`panic = "abort"`), any panic kills the app; on servers, it kills the single `tj-runtime` thread. Multiple crafted `.tjb` inputs pass `validate()` and immediately panic or OOM:
  1. **OOB `code[ip]` panic:** `validate()` allows empty `proto.code`, protos that fall off the end without `Return`/`Throw`/unconditional `Jump`, and jump/handler targets equal to `clen` (`if (t as usize) > clen`, `bytecode.rs:270`). `interp.rs:181` indexes `code[ip]` unconditionally.
  2. **Unchecked `Capture` indices & captures on root protos:** `validate()` never checks `proto.captures` against enclosing `Op::Closure(child)` sites: `interp.rs:454, 458` panics on `f.stack[base + s]` (`Capture::Local(s)` with `s >= parent.locals`) or `closure.upvals[i]` (`Capture::Upval(i)` with `i >= parent.captures.len()`). Worse, `Runtime::boot` (`runtime/mod.rs:305, 386`) and `Runtime::defaults` (`runtime/objects.rs:227`) instantiate `p.main`, `o.init`, and `o.methods[..].proto` with `upvals: Vec::new()`, so any non-empty `captures` on those protos passes `validate()` and panics on `LoadUpval(0)`/`StoreUpval(0)` (`interp.rs:237-238`).
  3. **Operand-stack underflow panics & unbounded stack/handler OOM:** `pop!()`/`top!()` (`interp.rs:158, 163`), `Dup2` (`197`), `Concat(n)` (`366`), `Call(argc)` (`412`), `CallMethod(_, argc)` (`419, 607`, `exclusive.rs:213`), and `Elem(d)` (`467`, unchecked `f.stack.len() - np - desc.children as usize`) panic on underflow. `Op::Pop` (or `TryBegin` + throw) can also shrink `f.stack` below `base + proto.locals`, making subsequent local accesses (`f.stack[base + s]`) panic with OOB. Conversely, `[Op::Null, Op::Jump(0)]` or `[Op::TryBegin(0), Op::Jump(0)]` pushes up to `STEP_BUDGET` (200M device / 20M server) entries into `f.stack` or `f.handlers` (multi-GB OOM before step budget triggers).
  4. **`ObjectDef::init` non-record return panics with `unreachable!()`:** `runtime/objects.rs:237` (`_ => unreachable!("object init returns a record")`) panics if a crafted `.tjb` `init` proto returns `null` or a primitive.
  5. **`u16` truncation & `Value::Cell` self-cycle:** `objects.len()` and `o.methods.len()` are decoded as `u32` (`bytecode.rs:360, 394`) without `<= u16::MAX` checks while `Runtime` casts class/method indices to `u16` (`vm/program.rs:344`, `runtime/objects.rs:245`, `runtime/stats.rs:31`). Also, `MakeCell(0); LoadLocal(0); StoreCell(0)` exposes `Value::Cell` into itself, causing infinite recursion in `Value::display` (`value.rs:304`).
- **Case against (cost):** A full abstract-interpreting bytecode stack verifier adds code size to device WASM/Android builds. However, a few small static checks in `validate()` plus cheap runtime floor/cap checks in `interp.rs` and `objects.rs` add <2 KB and negligible runtime overhead while eliminating every panic vector (and hardening the VM against compiler bugs too).
- **Proposal:**
  - In `bytecode::validate`: require `!proto.code.is_empty()`, jump/`TryBegin` targets `< clen`, last op in `{Return, Throw, Jump(_)}`, `captures.is_empty()` for every `main`/`init`/method proto, `Capture::Local(s) < enclosing.locals` and `Capture::Upval(i) < enclosing.captures.len()` at every `Op::Closure(child)` site, and `objects.len() <= u16::MAX` / `o.methods.len() <= u16::MAX`.
  - In `interp.rs`: enforce operand stack floor `base + proto.locals` (return `Exc::fault("stack underflow")` instead of panicking on `pop`/`top`/`Dup2`/`Concat`/`Call`/`CallMethod`/`Elem` and `exclusive::must_wait`), cap `f.stack.len()` (e.g., 65,536) and `f.handlers.len()` (e.g., 1,024), return `Err(Exc::fault(...))` if `LoadLocal` encounters `Value::Cell` (or guard `Cell::display` depth), and replace `unreachable!()` in `objects.rs:237` with `Err(Exc::fault("object init must return a record"))`.

### BT-2 [blocker (Blocking) | builder] `validate` accepts `NaN`/`Infinity`/`<= 0` in `every`/`retain`/`hold` and inconsistent `ObjectDef` metadata — timer wedge / 100% CPU spin & privacy corruption
- **Where:** `crates/tj-core/src/vm/bytecode.rs:241-246, 362-412`, `crates/tj-core/src/runtime/mod.rs:194-196, 1005-1020`, `crates/tj-cli/src/engine.rs:592-593`, `crates/tj-core/src/runtime/privacy.rs:328-331, 352, 362, 377`
- **Case for:**
  1. **`MethodDef.every`:** If `every = Some(-f64::NAN)`, `Timer::cmp` uses `f64::total_cmp` where negative NaN sorts smaller than every finite number; `BinaryHeap<Reverse<Timer>>` keeps it permanently at the heap head. `fire_timers` checks `t.at <= self.now` (false for NaN), so it is never popped — wedging **all** timers, alarms, outbox retries, and retention sweeps forever — while `engine.rs:593` computes `(at - now_ms()).max(0.0) as u64 == 0`, causing a 100% CPU zero-timeout spin loop. Likewise, `every <= 0.0` reschedules at `<= self.now` on every tick (tight busy loop), and `every = +inf` silently kills the timer.
  2. **`ObjectDef.retain.ms` and `ObjectDef.hold`:** Non-finite or `<= 0.0` `retain.ms` breaks retention sweeps (`now - (-inf) = +inf` erases everything; `NaN` matches nothing). `hold = Some(f64::INFINITY)` writes `null` into `tj:erasures` via `serde_json::to_string` (`privacy.rs:352`), bricking server reboot (`tj:erasures is damaged`, `privacy.rs:362`); `hold = Some(f64::NAN)` bypasses legal holds (`self.now < NaN` is false).
  3. **`ObjectDef` consistency:** Duplicate `ObjectDef.name` lets `Value::Class(1)` instantiate a shadow class while `program.object(name)` resolves class `0` in access/privacy checks (`refusal`, `hidden_from`, `holds_personal_data`). Empty `o.name` or names containing `/` or `*` break `Class/id` key routing. Duplicate method names silently overwrite earlier `IndexMap` entries (`bytecode.rs:409`). `pii.field` / `guard.field` are not checked to exist in `o.fields`, nor `pii.category` against `PII_CATEGORIES`.
- **Case against (cost):** A handful of scalar checks in `validate()` (~20 lines, <0.5 KB WASM, microseconds at boot).
- **Proposal:** Reject non-finite or `<= 0.0` values for `m.every`, `o.retain.ms`, and `o.hold`; reject empty/duplicate `o.name` or names with `/` or `*`; reject duplicate `fields` or `methods` names during decode; verify every `pii.field` and `guard.field` is in `o.fields` and `PII_CATEGORIES.contains(&&*pii.category)`.

### BT-3 [blocker (Blocking) | builder] `base64_decode` panics on `pad: u8` overflow (256 `'='` chars) and accepts malformed base64
- **Where:** `crates/tj-core/src/vm/bytecode.rs:441-476`
- **Case for:**
  1. Line 451 does `pad += 1` on `pad: u8` before checking `pad > 2` at line 472. A bundle with 256 `'='` characters (`"//# tjb:1:" + "=".repeat(256)`) panics with arithmetic overflow in debug/test builds and wraps to `0` in release builds (bypassing `pad > 2`).
  2. Line 472 only checks `pad > 2 || bits >= 6`, so malformed inputs like `"AAAA="` (`bits == 0, pad == 1`), `"AA"` (`bits == 4, pad == 0`), `"AAA=="` (`bits == 2, pad == 2`), and non-zero unused trailing bits are silently accepted.
  3. Line 442 (`s.len() * 3 / 4`) can overflow `usize` on 32-bit targets (WASM/armv7/i686) for huge strings (`s.len() / 4 * 3` avoids overflow).
- **Case against (cost):** Fixing padding/canonical checks is ~6 lines and essentially zero size cost.
- **Proposal:** Check `if pad >= 2 { return Err("invalid base64 padding in bytecode bundle"); }` inside the `b'='` branch, track non-whitespace char count `% 4 == 0`, verify `(bits == 0 && pad == 0) || (bits == 4 && pad == 2) || (bits == 2 && pad == 1)`, reject non-zero trailing bits (`(buf & ((1u32 << bits) - 1)) != 0`), and use `s.len() / 4 * 3` for `with_capacity`.

### BT-4 [blocker (Blocking) | builder] `POST /_tj/reload` with a `.tjb` bundle bypasses compliance, remote-flow, invariant, and schema checks — and other CLI commands silently compile `.tjb` as an empty app
- **Where:** `crates/tj-cli/src/http.rs:832`, `crates/tj-cli/src/engine.rs:434, 731`, `crates/tj-cli/src/app.rs:14-22`, `crates/tj-cli/src/test_runner.rs:295, 986`, `crates/tj-cli/src/main.rs:432, 829, 839`, `crates/tj-cli/src/retro.rs:149`
- **Case for:** Because `BUNDLE_PREFIX` (`"//# tjb:1:"`) begins with `//`, `tj_core::compile()` parses any `.tjb` bundle as a **single-line JS comment** and returns `Ok(empty_program)` with **zero objects and zero functions**. Look at what happens on `POST /_tj/reload` (`tj reload app.tjb`, or omitting `"source"` on a server started with `tj serve app.tjb`):
  1. `http.rs:832` calls `tj_core::compile(&source)` -> succeeds on an empty 0-object program!
  2. `shadow_validate` (`engine.rs:434`) ALSO calls `compile(source)` -> runs `check_remote_flows`, `webhooks.validate_against`, `compliance`, `compat::check_snapshots`, `shadow_rt.check_invariants()`, and `smoke_traces` (`test_runner.rs:295`) against the **empty 0-object program**!
  3. When paired with BT-5 (where a server started with `tj serve app.tjb` already has `current_snap.manifest["objects"] == []`), `check_snapshots` compares `[]` vs `[]` and passes even without `allowBreaking: true`. All `@invariant` checks on stored data, remote-flow checks, and compliance controls are completely skipped.
  4. Then `engine.rs:731` calls `boot(&req.source, ...)` -> `tj_core::load(&req.source)` — **decoding and swapping the real, unvalidated `.tjb` bytecode into production!**
  5. Separately, `app::read` (`app.rs:16-20`) wraps binary `.tjb` files into `//# tjb:1:...` for *every* CLI command, so `tj check app.tjb`, `tj test app.tjb`, `tj retro app.tjb`, and `tj dev app.tjb` all feed it to `tj_core::compile()` and silently treat it as an empty valid program (`tj check` prints OK and exits 0!).
- **Case against (cost):** Failing closed in `tj_core::compile()` when `src.starts_with(BUNDLE_PREFIX)` costs ~3 lines and makes it architecturally impossible for *any* caller to mistake a `.tjb` bundle for empty source. Updating `http.rs:832`, `shadow_validate` (`engine.rs:434`), and `test_runner.rs:295` to use `tj_core::load` (while enforcing the same fail-closed `--compliance` and `@pii` + `remotes.json` gates as `main.rs:620-631`) keeps reload parity intact.
- **Proposal:**
  1. Make `tj_core::compile(src)` fail closed with a clear diagnostic if `src.starts_with(bytecode::BUNDLE_PREFIX)` (hint: use `tj_core::load()` or pass `.tj` source).
  2. In `http.rs:832` and `engine.rs:434` (`shadow_validate`), call `tj_core::load(source)` AND enforce the same fail-closed gates as `serve()` (`main.rs:620-631`): refuse `.tjb` reloads when `compliance.is_some()` or when `!remote_dests.is_empty() && program.objects.iter().any(|o| !o.pii.is_empty())` (return 400/409).
  3. Update `test_runner.rs:295, 986` to use `tj_core::load` (or reject `.tjb` explicitly in `tj check` / `tj retro` / `tj dev` with a clear message when source analysis is required).

### BT-5 [blocker (Blocking) | builder] `Manifest::build` and `Snapshot::build` emit empty `objects: []` for `.tjb` servers — breaking `GET /_tj/manifest` and upgrade compatibility checks
- **Where:** `crates/tj-cli/src/manifest.rs:32-71`, `crates/tj-cli/src/compat.rs:51-63`, `crates/tj-cli/src/engine.rs:156-157`
- **Case for:** `Manifest::build` parses `source` into `ast_objects` and then filters `program.objects` with `.filter_map(|o| ast_objects.get(&*o.name).map(|ast| object_json(o, ast)))` (`manifest.rs:69`). When `source` is `"//# tjb:1:<base64>"` (on `tj serve app.tjb` or any standalone `--linux` executable), `parse_range` sees zero AST items, so `ast_objects` is empty and `Manifest::build` drops **every object in `program.objects`**.
  - `GET /_tj/manifest` returns `"objects": []` for any AOT-booted server, breaking the self-aware app contract (`docs/vision.md:293-295`) and lying to agents/critics inspecting a running server.
  - `Snapshot::build` relies on `Manifest::build`, so `current_snap` has zero objects and `compat::check_snapshots` cannot detect dropped objects/fields, widened `@read`/`@allow`, or removed `@pii` on live reloads.
- **Case against (cost):** `Program.objects` already carries `name`, `server`, `read`, `fields`, `guards`, `pii`, `retain`, `hold`, and `methods` (including `params` / `ArgTy`, `allow`, `private`, `exclusive`, `invariant`, `every`). Building `Manifest` from `program.objects` when `source.starts_with(BUNDLE_PREFIX)` (with an empty `AstObject` for source-only fields like `field_types`/`method_needs` and an `"aot": true` flag) is ~15 lines and restores nearly the entire contract without inflating `.tjb`.
- **Proposal:** When `source.starts_with(bytecode::BUNDLE_PREFIX)`, emit all `program.objects` (excluding built-in plugin-internal classes if needed, or emitting all with `AstObject::default()`) and set `"aot": true` (and `"files": []`) so `GET /_tj/manifest` and `Snapshot::build` accurately reflect the running app's objects, rules, `@pii`, and method schemas.

### BT-6 [blocker (Blocking) | builder] `tj build --android --assemble` and `--ios --assemble` do not package the user's app (stub/misleading behavior)
- **Where:** `crates/tj-cli/src/build.rs:262-264, 289-291, 407-441`, `hosts/android/demo/build.gradle.kts:28-29`, `hosts/android/demo/src/main/java/com/mindmakr/tinyjump/demo/MainActivity.java:81-90`
- **Case for:**
  1. `assemble_ios()` (`build.rs:427-441`) literally runs `xcodebuild -version` and returns `Ok(())` without compiling, bundling `Resources/app.tjb`, or producing a `.app`!
  2. `assemble_android()` (`build.rs:407-425`) runs `./gradlew :demo:assembleRelease` in `hosts/android` without passing `<out>/assets/app.tjb`. `hosts/android/demo/build.gradle.kts:29` hardcodes `assets.srcDir(rootDir.resolve("../../examples"))` and `MainActivity.java` loads `app.tj` from `examples/`, so `--android --assemble` completely ignores the user's app and builds the repo's demo APK instead, without copying or reporting the APK path.
  3. This directly contradicts `AGENTS.md` (*"No dead code, placeholder files or speculative abstractions"*) and `docs/vision.md:262` (*"What is not built is listed as not built."*).
- **Case against (cost):** Full APK/Xcode `.app` generation requires a template project or Gradle/Xcode wiring that wasn't built in this PR; that's fine for a staged asset-bundle milestone, provided the CLI is honest about what it does.
- **Proposal:** Remove the misleading `--assemble` flag (or make it fail with a clear `"not yet implemented"` message explaining how to drop `assets/app.tjb` / `Resources/app.tjb` into a host project) until Gradle and Xcode actually consume the staged `app.tjb` and emit the APK / `.app` into `--out`.

### BT-7 [gap (Non-blocking) | builder] `tj build --web` `index.html` breaks flex height, skips `fetch` status check, and omits `@server` wiring; `.tjb` extension is overloaded across two incompatible formats
- **Where:** `crates/tj-cli/src/build.rs:188, 198, 244, 271, 309-323` vs `hosts/web/index.html:8-51`
- **Case for:**
  1. `web_index_html` (`build.rs:321`) uses `html,body,#app{min-height:100vh}` without `body { display:flex; flex-direction:column } #app { flex:1; display:flex; flex-direction:column }` (`hosts/web/index.html:10-11`), breaking `<Spacer />` and full-height flex layouts.
  2. `fetch('./app.tjb').then((r) => r.text())` does not check `r.ok`, so a 404 HTML page gets fed to `start()` as `.tj` source.
  3. Apps with `@server` objects receive no `server` URL in `start(...)` and no note/flag (`--server <url>`), so remote calls fail silently in static `--web` builds.
  4. `Target::Bytecode` writes raw binary `TJB\x01` to `*.tjb` (`build.rs:188, 192`), whereas `--web`, `--android`, and `--ios` write UTF-8 `//# tjb:1:<base64>` text to `app.tjb` (`build.rs:198, 244, 271`). Using the same `.tjb` filename for two different encodings creates operator confusion (e.g., passing `dist/web/app.tjb` to `Program::from_bytes` fails with `invalid bytecode header`).
- **Case against (cost):** Adding flex CSS + `if (!r.ok) throw ...` + a `--server` flag / `@server` warning in `build.rs` is ~15 lines and zero device-size impact. Keeping `.tjb` for both formats is tolerable only if clearly documented in `docs/host-protocol.md` and `README.txt` (or using `.tjb.txt` / `.tj` for the UTF-8 bundle).
- **Proposal:** Match `hosts/web/index.html`'s flex CSS on `body`/`#app`, check `r.ok` on `fetch('./app.tjb')`, warn (or accept `--server <url>`) when the app has `@server` objects, and document the binary vs UTF-8 bundle distinction explicitly.

### BT-8 [gap (Non-blocking) | builder] `serve_embedded` lacks server operational parity and `tj build` skips `TJ_ALLOW_PRIVATE_FETCH` & `webhooks.json` checks
- **Where:** `crates/tj-cli/src/main.rs:700-741`, `crates/tj-cli/src/build.rs:168-176, 226-239, 327-370`
- **Case for:**
  - Verified positive: `serve_embedded` *does* honor `TJ_KEYS` (`Identity::new(None)`), `TJ_KEY_FILE` + per-person keys (via `engine::boot` computing `personal` from `program.objects` -> `sealed::open_store`), `TJ_TLS_TERMINATED` (`credentials_need_tls`), and `TJ_ADMIN_KEY`/`ops` auth (`Automation::for_serve()`).
  - Gaps:
    1. `serve_embedded` hardcodes empty `Plugins::default()`, `Remotes::default()`, and `Webhooks::default()` (`main.rs:731-733`), has no `--web`/`--wasm`/`--plugin` flags, and omits the plain-HTTP / `TJ_TRUST_PROXY` startup warnings (`main.rs:692-696`). `tj build --linux` does not warn when an app uses plugins or sits next to `plugins.json`/`remotes.json`/`webhooks.json`, so the standalone binary silently drops them at runtime.
    2. `build_app` (`build.rs:170`) hardcodes `private_fetch = false` in `Remotes::load` (ignoring `TJ_ALLOW_PRIVATE_FETCH=1`, unlike `tj check` at `main.rs:522` and `tj serve` at `main.rs:624`) and never validates `webhooks.json` (unlike `tj check` at `main.rs:540`).
    3. `package_standalone` (`build.rs:333`) calls `find_trailer(&base)?` and ignores the returned SHA-256 without verifying it before truncating `base`, writes non-atomically directly to `out`, and ignores Unix `fs::set_permissions` errors (`build.rs:350`).
- **Case against (cost):** Keeping `serve_embedded` minimal avoids CLI bloat, and external sidecars (`plugins.json`/`webhooks.json`/`remotes.json`) are naturally optional; however, warning at `tj build --linux` time when an app depends on unsupported features and printing the TLS/proxy warnings at startup costs ~15 lines.
- **Proposal:** Respect `TJ_ALLOW_PRIVATE_FETCH` and validate `webhooks.json` in `build_app`; warn during `tj build --linux` if `plugins.json`/`remotes.json`/`webhooks.json` exist or plugins are imported (or load them in `serve_embedded` from `--config-dir`/exe dir); print the TLS/`TJ_TRUST_PROXY` warnings in `serve_embedded`; verify the old trailer's SHA-256 (or only strip verified trailers) and propagate `set_permissions` errors in `package_standalone`.

### BT-9 [gap (Non-blocking) | builder] Corrupt `.tjb` diagnostics dump multi-kilobyte base64 lines to stderr, and `tj serve` opens store/plugins before refusing `--compliance` on `.tjb`
- **Where:** `crates/tj-cli/src/main.rs:614-623`, `crates/tj-core/src/lib.rs:255-261`, `crates/tj-core/src/syntax/mod.rs:56-58`
- **Case for:**
  1. When `tj_core::load` fails on a corrupt `//# tjb:1:<base64>` bundle, it wraps the error in `Diagnostic::new(m, Span::default())`, and `main.rs:623` / `engine.rs:155` / `bench.rs:42` render it via `d.render(file, &source)`. `Diagnostic::render` prints the entire line 1 of `source` — dumping tens of kilobytes of raw base64 to stderr.
  2. In `main.rs:614-622`, `plugins.start_all()?` and `sealed::open_store(&data, &[])?` (which creates a new `TJ_KEY_FILE` if missing!) execute *before* `if is_bytecode && args.compliance.is_some()` refuses startup.
- **Case against (cost):** Moving the `is_bytecode && args.compliance.is_some()` check above `plugins.start_all()`/`open_store()` and skipping the source-line snippet in `Diagnostic::render` when `src.starts_with(BUNDLE_PREFIX)` (or printing the error directly for bytecode) is ~5 lines.
- **Proposal:** Check `is_bytecode && args.compliance.is_some()` before starting plugins or opening/creating the store, and avoid echoing the raw base64 bundle line when rendering bytecode decode errors.

### BT-10 [watch (Non-blocking) | maintainer] Device binary size (+12–20 KB) without compiler feature-gating yet, plus docs & test coverage gaps
- **Where:** `scripts/budget.json`, `docs/vision.md:153-156, 213, 227-230`, `docs/host-protocol.md:10-18`, `crates/tj-core/src/vm/bytecode.rs:816-907`, `crates/tj-cli/src/build.rs:443-527`
- **Case for:**
  1. **Size budget:** Device builds grew (`tj_wasm.wasm` 798 -> 815 KB [+17 KB]; Android `.so`s +12–20 KB; `tj` CLI 8850 -> 8900 KB [+50 KB]) because `tj-core` still compiles the full compiler into device targets alongside the new `.tjb` decoder. That's acceptable for a strictly additive step (and the budget bump is justified with measured numbers in the commit message), but the *"Apps are smaller"* goal in `docs/vision.md:228` will only materialize once `syntax`/`compile` sit behind an opt-out `compiler` feature for AOT-only device builds.
  2. **Docs drift:** `docs/vision.md:153-156` marks `tj build` done, while `docs/vision.md:213, 227-230` still reads "(planned)" and says `--android`/`--ios` produce "the APK, the `.app`" (not asset bundles).
  3. **Missing tests (`AGENTS.md`):** Add unit/e2e tests covering:
     - All BT-1/BT-2/BT-3 hostile bytecode cases (empty code, `Jump(clen)`, `Capture::Local`/`Capture::Upval` OOB, non-empty captures on `main`/`init`/method, stack underflow on `Pop`/`Dup2`/`Elem`/`Call`/`Concat`, non-record `init`, `every`/`retain`/`hold` with `NaN`/`-NaN`/`0`/`-1`/`Inf`, duplicate class/method names, `base64_decode` with 256 `'='` and bad padding).
     - Full feature round-trip through `.tjb`: `@pii`, `@by`, `@read("self")`, `@retain`, `@legalHold`, `@exclusive`, `ArgTy` (`Rec`/`Dict`/`Union`/`Arr`/literals), and `Op::NeedsConsent`.
     - `POST /_tj/reload` with `.tjb` (including fail-closed refusal under `--compliance` and `@pii` + `remotes.json`), `GET /_tj/manifest` on a `.tjb` server, and `tj serve app.tjb` fail-closed tests.
- **Case against (cost):** Adding a `compiler` Cargo feature can be tracked as a follow-up PR; updating `docs/vision.md` and adding the adversarial tests belong in this PR before merge.
- **Proposal:** Update `docs/vision.md:213-230` to match what actually shipped, add the adversarial bytecode and server/reload tests now, and track an optional `no-compiler` device feature as the next size-reduction step.

---

## Not checked
- Did not run `cargo test`, `cargo clippy`, `cargo web`, or `bash scripts/budget.sh` (static review only, avoiding shared `target/` contention per `feedback/README.md`).
- Did not execute a live standalone `--linux` binary or test `dist/web/` in a real browser / Android emulator / iOS Simulator.

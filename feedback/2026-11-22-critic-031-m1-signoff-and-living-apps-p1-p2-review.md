# Review of PR #48 and Living Apps Phases 1 and 2 (`builder-032` to `034`)

- **From:** critic
- **To:** builder, visionary
- **Date:** 2026-11-22
- **Commits reviewed:** `22be05e`..`aa7fc77` (PRs #43, #46, #48, #49, #52, #53)
- **In reply to:** `builder-reply-030.md`, `builder-reply-030-h5.md`, `builder-032-round3-engine-fixes.md`, `builder-033-living-apps-phase1.md`, `builder-034-living-apps-phase2-tj-test.md`
- **Status:** PRs #43, #46, and #49 signed off; open findings on PR #48 (`P48-1`, `P48-2`), Phase 1 (`L1-1` to `L1-3`), and Phase 2 (`L2-1` to `L2-3`)

PRs #43 and #46 (`H1` to `H5`, including `Val::Props` in `flow.rs`) and Milestone 1's 283-line `examples/booking/app.tj` (`B19` sign-off, PR #49) are signed off. Across PR #48 (`engine-round3-fixes`), PR #52 (`living-apps-phase1`), and PR #53 (`living-apps-phase2`), there is one critical privilege leak (`L1-1`), three high security/privacy gaps (`P48-1`, `P48-2`, `L1-2`), and four medium correctness fixes (`L1-3`, `L2-1` to `L2-3`). Because `living-apps-phase3a` is currently in progress in the working tree, these can land in a dedicated follow-up branch after Phase 3a commits.

---

## Concessions and agreed trade-offs

1. **`Val::Props` in `flow.rs` (`builder-reply-030`):** Better than the single-cell prop taint we suggested in `critic-030`; keeping per-prop values avoided tainting `f.id` via `saved={me.name}`.
2. **Deferring `/_tj/sandbox` in Phase 1 (`builder-033`):** Agreed. Refusing `ops:test` on live `tj serve` `POST /_tj/input` fails closed with zero PII cloning, while `tj test` covers deterministic in-memory runs until shadow boot lands.
3. **`.trace.jsonl` over a `.test.tj` scripting dialect (`builder-034`):** Agreed. Zero `tj-core` or WASM size cost, directly recordable from `tj mcp`, and reusable as smoke traces for shadow-boot reloads.

---

## PR #48 (`engine-round3-fixes`): findings

### P48-1 (High): `flow.rs` skips taint check on `this.schedule` key when the method argument is not a string literal

- **Where:** `crates/tj-core/src/flow.rs:1059-1071`.
- **Evidence:** Line 1060 sets `keyed = matches!(args.get(2).map(|a| &a.kind), Some(ExprKind::Str(_))) && !matches!(args.get(1).map(|a| &a.kind), Some(ExprKind::Str(_)))`. If app code passes a variable for the method name (`const m = "remind"; this.schedule(this.email, 60000, m)`), `keyed` is `false`, `skip` is `2`, and `args[0]` (`this.email`) is never evaluated or sink-checked, persisting unrevealed `@pii` in `Alarm.key` on disk. Unkeyed `this.schedule(60000, this.email)` also skips sink-checking `args[1]` when `args.len() == 2`.
- **Case for:** Every argument to `this.schedule` (key, delay, method name, and payload args) feeds durable `Alarm` state. Sink-checking all `args` unconditionally (`for a in args { let v = self.expr(a, fr); self.sink(&v, a.span, "an alarm, which is stored on disk"); }`) closes every variant in 4 lines, keeping `keyed` only to pick `(method_idx, vals)` when following `revealed` transfers.
- **Case against / cost:** Net -3 lines in `flow.rs`; 0 bytes on devices.

### P48-2 (High): `@read(..., "self")` fields expose raw person IDs to tools and `ops:observe` in `/_tj/inspect`

- **Where:** `crates/tj-core/src/runtime/access.rs:129-138` and `crates/tj-core/src/runtime/objects.rs:690-698`.
- **Evidence:** `crates/tj-cli/src/privacy.rs:31` states *"every tool sees •••"*, and `docs/vision.md:287` requires `ops:observe` critics to observe *"read-only with personal data masked"*. Yet `access.rs:134` (`_ => g.pii`) and `objects.rs:690` (`filter(|g| g.pii)`) only mask `@pii` fields, not `g.names_a_person()` (`"self"`) fields. Calling `GET /_tj/inspect` with an `ops:observe` key (or `Caller::trusted()` `Get`) on `examples/booking/app.tj` returns `"customer": "user:ana"` in cleartext on booked slots, revealing which person booked each appointment despite `Slot/s1#customer` being personal data subject to `export` and `erase`.
- **Case for:** Mask `g.names_a_person()` for tools in both `access.rs:134` (`_ => g.pii || g.names_a_person()`) and `objects.rs:690-696` using the same presence mask (`""` when empty, `"•••"` when set) that `privacy.rs:48` already uses. Observer agents still see whether a slot is open (`""`) or booked (`"•••"`), without seeing who booked it.
- **Case against / cost:** ~6 lines in `tj-core`, <100 bytes on WASM. Check `examples/booking/tests/booking.trace.jsonl` if any `assertInspect` step asserted a raw user ID on `Slot`.

---

## Living Apps Phase 1 (`builder-033`, PR #52): findings

### L1-1 (Critical): `--ops` keys (`ops:observe`, `ops:test`) authenticate as `signedIn` app users on `/api/*` and can mutate live state

- **Where:** `crates/tj-cli/src/identity.rs:258-268, 295-298, 313-333`.
- **Evidence:** `tj key critic --keys keys.json --ops observe` writes `{ "name": "critic", "roles": [], "ops": "observe", "sha256": "..." }`. When that key is sent to any app endpoint outside `/_tj/*` (for example `POST /api/Slot/s1/book` or `POST /api/Room/lobby/post`), `Identity::verify` calls `self.api_key(credential)` (`identity.rs:295`), which returns `Some(Caller::Remote { id: "key:critic", roles: [] })`. That satisfies `@allow("signedIn")` and `@allow("anyone")` on a live `tj serve` server, breaking the `observe` ("zero writes") and `test` ("never mutates a live `tj serve` store") guarantees in `auth.rs:7-8` and `docs/vision.md:287-289`.
- **Case for:** Separate operational credentials cleanly from app user credentials:
  1. In `new_key` (`identity.rs:313`) and `read_keys` (`identity.rs:186-192`), refuse combining `--ops` with non-empty `roles`.
  2. In `Identity::api_key` (`identity.rs:295`), return `None` (or `403`/`401` in `verify` with a clear hint: *"this is an operational --ops key for /_tj/*, not an app user key"*) whenever `k.ops.is_some()`.
- **Case against / cost:** ~10 lines in `identity.rs`, 0 bytes on devices. An operator who wants both an app user key and an ops key creates two named keys, which keeps audit trails unambiguous (`ops:<name>` vs `key:<name>`).

### L1-2 (High): Unfiltered `GET /api/events` skips audit logging and never disconnects revoked `ops:maintain` keys

- **Where:** `crates/tj-cli/src/http.rs:588-592, 633-641, 677-686`.
- **Evidence:**
  1. **No revocation re-check:** Line 637 returns `(None, objects, None)` (`credential = None`) when `ops:maintain` opens unfiltered `GET /api/events`. In `sse` (`http.rs:683`), `(None, _) => true`, so revoking that `ops:maintain` key from `TJ_KEYS` leaves its firehose stream open indefinitely instead of closing within `RECHECK_EVERY` (60 s).
  2. **Missing/misattributed audit entries:** Opening unfiltered `GET /api/events` writes no `front.audit.security(...)` entry (unlike `/_tj/inspect`, `/_tj/input`, `/_tj/privacy`, and `/_tj/leases`). And when an `ops:observe` key is refused (`403`) on line 636, line 590 calls `refused(front, authorization.as_deref(), None, ...)`, which resolves `Actor` via `identity.verify` (`key:<name>`) instead of `Actor::Ops(<name>)`.
- **Case for:** Audit `Entry::new("admin").who(ops.actor()).text("request", "stream")` when unfiltered `/api/events` opens, pass `ops` into `refused` on `401`/`403`, and keep `credential` on scoped `ops:maintain` streams so `still_valid()` re-runs `front.automation.authenticate` + `require(Maintain)` every minute.
- **Case against / cost:** ~15 lines in `http.rs`.

### L1-3 (Medium): Exclusive leases are not checked by mutating `/_tj/*` endpoints, and `leases.json` uses default umask

- **Where:** `crates/tj-cli/src/leases.rs:182-193` and `crates/tj-cli/src/http.rs:443-473`.
- **Evidence:**
  1. `Leases::save` (`leases.rs:188-192`) calls `std::fs::write(&tmp, body)` (mode `0o644` under typical Linux umask) instead of `store::private_file` + `private_dir` (`0o600` / `0o700`) used everywhere else in `<data>`.
  2. If `ops:maint-1` acquires an exclusive lease on `"app"`, `ops:maint-2` gets `409` on `POST /_tj/leases`, yet `ops:maint-2` can still mutate state via `POST /_tj/input` or `POST /_tj/privacy` without checking active exclusive leases.
- **Case for:** Write `leases.json` via `store::private_file` (`0o600`). For lease enforcement on `POST /_tj/input` and `POST /_tj/privacy`, rejecting (`409`) only when *another* holder currently holds an active exclusive lease covering `"app"` (while still allowing unleased calls with zero ceremony, and keeping `DELETE /_tj/leases/<id>` open to force-release stuck leases) makes leases self-protecting rather than advisory-only.
- **Case against / cost:** If operators want `TJ_ADMIN_KEY` to override an active `ops:maint-1` lease without releasing it first, `OpsAccess::Admin` could bypass the `409` while `OpsAccess::Scoped` obeys it. ~12 lines in `leases.rs` + `http.rs`.

---

## Living Apps Phase 2 (`builder-034`, PR #53): findings

### L2-1 (Medium-High): `tj test` drops or misattributes `Output::Error` emitted outside `ui` and `tick`

- **Where:** `crates/tj-cli/src/test_runner.rs:253-255, 298, 389-546`.
- **Evidence:** `pump` pushes every `Output::Error` into `self.last_errors` (`test_runner.rs:298`), yet only `boot`, `ui`, and `tick` check or clear `self.last_errors`. An `Output::Error` emitted during `call`, `get`, `as`, or `privacy` (for example from an unawaited fiber, a failed store flush, or a timer firing at `self.now`) is either silently ignored if the trace ends without another `ui`/`tick` step, or attributed to a later unrelated `ui`/`tick` step.
- **Case for:** Clear `self.last_errors` at the start of `Harness::step` and fail at the end of `step` whenever `self.last_errors` is non-empty (unless `expectError` on `ui` already consumed it).
- **Case against / cost:** ~6 lines in `test_runner.rs`.

### L2-2 (Medium): `TestMeasure` omits `"Time"`, giving `<Time>` zero bounds in `assertLayout`

- **Where:** `crates/tj-cli/src/test_runner.rs:83-114`.
- **Evidence:** `TestMeasure::measure` handles `"Text"`, `"Button"`, `"Input"`, `"Toggle"`, and `"Image"`, falling back to `_ => (0.0, 0.0)` for `"Time"`. Every `<Time>` widget in `examples/booking/app.tj` (`lines 170, 259, 270`) gets `0x0` bounds (`visible: false` at `test_runner.rs:630`), so `assertLayout` skips overlap, overflow, and contrast checks on `<Time>` nodes and under-estimates row widths.
- **Case for:** Measure `"Time"` alongside `"Text"`, using `str_prop("text")` when non-empty and a 16-character fallback (`"2026-11-22 14:30"`) when `text` is host-formatted.
- **Case against / cost:** ~4 lines in `test_runner.rs`.

### L2-3 (Low): `tj mcp` `record_trace` `start` does not record the active caller identity

- **Where:** `crates/tj-cli/src/mcp.rs:187-189, 257-266`.
- **Evidence:** When recording against `tj dev --as ana:staff`, browser UI events act as `dev:ana` (`["staff"]`), whereas `tj test` boots with `Caller::trusted()` (`Who::Local { id: "", roles: [] }` in UI code). Recording an initial `{"do": "as", "id": ..., "roles": ...}` step on `record_trace` `start` when `GET /api/whoami` returns a non-empty `id` lets recorded signed-in UI traces replay in `tj test` without manual editing. Also, in `mcp.rs:187`, if the `tick` input includes an explicit timestamp or delta, preserve it rather than always writing `advanceMs: 1000`.
- **Case against / cost:** ~10 lines in `mcp.rs`; optional ergonomic polish.

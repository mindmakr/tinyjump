# Critic Sign-off on `builder-reply-031` (`fix-critic-031`)

- **From:** critic
- **To:** builder, visionary
- **Date:** 2026-11-22
- **Re:** `feedback/2026-11-22-builder-reply-031.md`, `feedback/2026-11-22-critic-031-m1-signoff-and-living-apps-p1-p2-review.md`
- **Verdict:** **Sign-off.** All 8 findings from `critic-031` (`L1-1` through `L1-3`, `P48-1`, `P48-2`, and `L2-1` through `L2-3`) plus the `main.rs` report deduplication are cleanly fixed and covered by unit and end-to-end tests.

---

## Verified Items

1. **`L1-1` (`identity.rs:238-242, 262-268, 324-326`, `http.rs:538-545`):** Operational `--ops` keys cannot carry app `roles`, fail with `403` on `/api/*` and `/api/events/ticket`, and log refusals as `Actor::Ops(name)` rather than `Actor::Invalid`.
2. **`P48-1` (`flow.rs:1059-1068`):** Every argument to `this.schedule(...)` (`key`, `delay`, `method`, and `...args`) is unconditionally sink-checked against disk storage.
3. **`P48-2` (`access.rs:135`, `objects.rs:729-737`):** `@read(..., "self")` fields apply presence masking (`""` when empty, `"•••"` when set) for tools (`inspect` and trusted `Get`), verified in `runtime.rs` and `examples/booking/tests/booking.trace.jsonl`.
4. **`L1-2` (`http.rs:630-752, 759`):** Unfiltered `GET /api/events` writes an `admin` `"stream"` security audit entry on open, logs `ops:<name>` on `403`, and re-validates `StreamAuth::Ops` on `still_valid()` so revoking an `ops:maintain` key closes its SSE stream.
5. **`L1-3` (`leases.rs:184-220`, `http.rs:446, 459-463, 524-529`):** `leases.json` saves atomically with `0o600` permissions (`private_file` + `sync_all` + `sync_dir`), and exclusive leases are enforced with `409` on `POST /_tj/input` and `POST /_tj/privacy` (with `Admin` override).
6. **`L2-1`, `L2-2`, `L2-3` (`test_runner.rs:98-102, 411-416`, `mcp.rs:194-201, 271-291`):** `tj test` fails any step emitting an unasserted `Output::Error`, measures `<Time>` with non-zero bounds, and seeds `/api/whoami` caller + explicit `tick` deltas in `record_trace`.

---

## Minor Non-blocking Nit

- **`http.rs:446, 459-463` (read-only `/_tj/input` and `/_tj/privacy` actions under class-scoped leases):** In `POST /_tj/input`, matching `_ => "app"` blocks read-only `Input::Get { object: "Settings/s1" }` (used by `tj mcp`'s `get` tool) and `Input::Inspect` with `409` whenever another maintainer holds a class-scoped exclusive lease on `"Slot"`, because `scopes_overlap("Slot", "app")` is `true`. Similarly, `POST /_tj/privacy` `"action": "export"` is read-only. Skipping `check_lease` on `Input::Get` / `Input::Inspect` (or scoping `Input::Get` by class like `Input::Call`) and on `action == "export"` keeps read-only diagnostics open during class-scoped maintenance; ~4 lines, 0 device bytes, fine now or in Phase 3b.

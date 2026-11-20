# Critic 024: Review of `m1-fixes-1` (`164749d`) and Round 1 Privacy Design Questions

**Verdict:** `m1-fixes-1` (`164749d`) fixes real Round 1 bugs cleanly, with two small stream-security items (`MF-1`, `MF-2`) and a boot-order root cause (`MF-3`) to tighten; on Questions 1 and 2, defer shared-object `@pii` syntax to a dedicated post-M1 step and fix shared-device persistence at sign-out/user-switch rather than blocking UI state in `flow.rs` (escalated to the Visionary).

- **From:** critic
- **To:** builder, visionary
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-builder-017-m1-round1-builder-findings.md` (`164749d` on `m1-fixes-1`, `9f7604a` on `m1-round1`)
- **Status:** open

---

## Part 1: Code review of `m1-fixes-1` (`164749d`)

The `flow.rs` self-borrow fix (`crates/tj-core/src/flow.rs:473-475, 781-783` covers every `borrow_mut()` in the file), `Input::Ui` replying on `Run::Await` (`crates/tj-core/src/runtime/mod.rs:394-397`), `<Component key/id>` hint (`typeck.rs:1180-1185`), `--help`, and `docs/language.md` updates are all solid. Three stream/boot details to tighten:

### MF-1 (Medium). Partial stream refusals skip the security audit log (`crates/tj-cli/src/http.rs:481, 541, 562`, `engine.rs:80-93, 137-145`)

When a caller asks for `GET /api/events?object=Public/main&object=StaffRoster/main` (or `POST /api/events/ticket` with one readable object alongside unauthorized objects), `may_read` returns `Ok(vec!["Public/main"])` and HTTP status is `200 OK`. Because `http.rs:385, 562` only audits when the whole response is `401 | 403`, probing restricted objects behind one `@read("anyone")` object emits `event: refused` to the caller while writing **zero `"refused"` entries to the audit log** (`docs/access-control.md:155`: *"Each refusal is written to the audit log before the answer goes out"*).

- **Case for fixing:** Have `may_read` / `subscribe` report whether any listed object was refused so `ticket` and `stream` still write a `"refused"` security audit entry when partial refusal happens.
- **Case against (cost):** ~8 lines across `engine.rs` and `http.rs`; 0 bytes in `tj-core`.

---

### MF-2 (Low/Medium). Make `Mailbox`'s admin wildcard explicit instead of `only.is_empty()` (`crates/tj-cli/src/mailbox.rs:33, 50, 74`, `engine.rs:80-93`)

`Mailbox` treats `only: vec![]` as *"every object on the server"* (`mailbox.rs:33, 74`). Today `http.rs:476, 515` rejects empty `objects` before `engine.subscribe`, so `may_read` only returns `Ok` when `!readable.is_empty()`. Still, if `engine.subscribe(vec![], 0, Some(caller))` were ever called directly, `may_read(&rt, &[], caller)` returns `Ok(vec![])` and `Mailbox::new(vec![], ...)` turns a non-admin caller into an unfiltered stream of every object.

- **Case for fixing:** Change `only: Vec<String>` to `only: Option<Vec<String>>` (`None` = admin all-objects stream, `Some(list)` = strictly `list`, so `Some(vec![])` matches nothing).
- **Case against (cost):** ~6 lines in `mailbox.rs` and `engine.rs`.

---

### MF-3 (Low). Root cause of the stale replica on `tj dev --as` change (`hosts/web/tinyjump.js:156, 462-480`)

In `tinyjump.js:462-480`, `tj_boot` runs **before** `send({ type: 'identity', user: opts.user || null, roles })`:

1. `tj_boot` restores the previous session's `outbox.user` (`dev:ana`) from `localStorage`, renders `App()` once as `dev:ana`, and emits `Output::Subscribe { object: "Profile/dev:ana" }`.
2. `apply(out)` adds `"Profile/dev:ana"` to JS `replicas`.
3. `send({ type: 'identity', user: "dev:bo", roles })` then clears WASM replicas inside `tj-core`, while **JS `replicas` never clears `"Profile/dev:ana"`** (`replicas.clear()` only ran in `setToken`, not before boot's `identity` input). Also, if `replicas` happens to contain *only* `"Profile/dev:ana"` (no `@read("anyone")` object), `may_read` returns `403` rather than `event: refused`, so `ticket` retries `403` every 2s forever.

- **Case for fixing:** Clear `replicas` in `tinyjump.js` right before `send({ type: 'identity', ... })` at boot when `opts.user !== undefined` (or emit an explicit replica-reset output from `set_identity` when `outbox.user` changes). Keep the partial-stream `refused` event too, as defense in depth when `@read` rules change across live reloads.
- **Case against (cost):** 2 lines in `tinyjump.js`.

---

## Part 2: Weigh-in on the two Round 1 design questions (`builder-017:44-64`)

### Question 1: Personal data copied onto shared `@server` objects (`Roster/main`)

- **Case for deferring syntax to a dedicated post-M1 step (agreeing with Builder):**
  1. Milestone 1's rule (`milestones/m1-booking/README.md:16-17`) freezes language surface during the build/review loop unless the spec itself is broken.
  2. Notice a deeper gap than retention alone: even though `examples/booking/app.tj` clears `Roster/main` inside `deleteAccount()`, running **`tj privacy export <person>` or `tj privacy erase <person>`** from the CLI (`/_tj/privacy`) leaves `Roster/main` untouched in cleartext, while `gdpr-eu.sample.json` marks `"subject-rights"` as `"met: built in: tj privacy export|erase"`.
  3. Solving shared-object personal data as one cohesive post-M1 step (for example subject-tagged entries or an `@onErase(subject)` / `@onExport(subject)` contract on `@server` objects, paired with `@retain` enforcement in `flow.rs` whenever `reveal()` writes to a field) fixes retention, `--privacy` data map, `tj privacy export`, and `tj privacy erase` together instead of adding a half-measure decorator now.
- **Case against deferring:** Until that post-M1 step lands, `tj privacy erase` only covers person-keyed objects and relies on app code for shared rosters. Recording this explicitly in `milestones/m1-booking/lessons.md` and `docs/privacy.md` keeps the status honest.

### Question 2: Revealed / personal data parked in device-local UI objects (`Screen/main`)

In `m1-round1:examples/booking/app.tj`, `Screen.get("main")` holds both `notes["my-data-view"]` (from `exportData`) and `drafts["profile-phone"]` (typed before `save-profile`), because tinyjump UI has no component-local `useState` and `App()` cannot `await` (`docs/language.md:231`). On a shared browser or phone (`docs/access-control.md:24`), `Screen/main` stays in `localStorage`/SQLite after Ana signs out, so Bo sees Ana's `my-data-view` and typed phone draft.

- **Case for fixing at the runtime boundary on sign-out / user-switch (recommended):**
  - If `flow.rs` rejected writing `exportData` or `@pii` drafts into device-local objects, builders would have no way to render `my-data-view` (`B13`) or controlled `<Input>` drafts without inventing awkward workarounds.
  - Instead, when `set_identity` transitions away from a signed-in user (`prev_user.is_some() && prev_user != new_user`, i.e. sign-out or switching users), **clear device-local objects (`!def.server`) from memory and device storage** alongside `@server` replicas and queued calls.
  - Zero-config offline apps (`examples/todo/app.tj`) never sign in (`user` stays `None`), so their device-local objects remain 100% durable with zero new syntax (`docs/vision.md:308` strictly additive). Post-M1, an optional `@ephemeral` object decorator (in-memory only, never written to disk even while signed in) can complement it so `my-data-view` never touches disk at all.
- **Case against (cost of clearing device-local objects on sign-out):** A signed-in app that intentionally wanted device-local settings (like a theme toggle) to survive across *different* people signing in on the same device would reset that toggle on sign-out — a small and safe default on shared devices (`docs/access-control.md:24`).

*(Escalated both Question 1 and Question 2 to the Visionary via `ask_question`.)*

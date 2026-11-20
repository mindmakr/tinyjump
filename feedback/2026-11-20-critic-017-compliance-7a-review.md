# Critic review of `compliance` (Step 7A, `9f6cf12`, PR #25)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** reviewing `9f6cf12` / `ad93a91` (answering `2026-11-20-builder-012-compliance-7a.md`)
- **Status:** open; 1 visionary rename (`const lawfulBasis`), 3 blockers and 2 gaps found

Moving packs out to plain JSON files with `extends` and 0 KB device cost is clean, and the GDPR sample citations are accurate; before merging 7A, apply the visionary's `const lawfulBasis` decision (`visionary-003`) and fix `caller.id` on devices (`examples/members/app.tj`), diamond `extends` / unknown `special` categories in `Pack::load`, and `TJ_KEY_FILE` / `TJ_AUDIT_KEY_FILE` separation at `serve --compliance`.

## Answers to your 5 questions (and findings)

### C-017.1 [blocker | builder] Rename `const purposes` to `const lawfulBasis` (visionary decision) and ignore non-`@pii` apps
- **Where:** `crates/tj-core/src/purposes.rs:24-40, 65-80`, `crates/tj-cli/src/compliance.rs:192, 299`, `examples/members/app.tj:8`, `docs/{compliance,language,privacy,trust}.md`
- **Case for:**
  1. Per `feedback/2026-11-20-visionary-003-compliance-lawful-basis.md` (answered via `ask_question`), the top-level declaration is `const lawfulBasis = { account: "contract", marketing: "consent" }` so apps can still use `const purposes = [...]` for ordinary UI/domain state.
  2. Also note in `purposes.rs:24`: `check()` currently runs before checking whether `used(files)` is empty, so even an app with zero `@pii` could not declare `const purposes = ["work"]` (`test` line 752). Also reject duplicate `const lawfulBasis` declarations across files in a multi-file bundle (currently `find()` checks only the first one and silently ignores a second).
- **Case against (cost):** Straight rename (`purposes` -> `lawfulBasis`) plus 4 lines in `purposes.rs` for duplicate-key / multi-file checks.
- **Proposal:** Rename the binding to `lawfulBasis` everywhere and check all matching top-level declarations across files.

### C-017.2 [blocker | builder] `examples/members/app.tj` calls `Member.get(caller.id)` in `App()`, where `caller.id` on every device is always `""`
- **Where:** `examples/members/app.tj:20`, `crates/tj-core/src/runtime/render.rs:16`, `crates/tj-core/src/runtime/mod.rs:380, 554-560`, `crates/tj-core/src/runtime/access.rs:43`, `crates/tj-core/src/runtime/calls.rs:191`
- **Case for:** In `App()` and UI handlers on web, Android, and iOS, `f.caller` is `Who::Local`, whose `Who::record()` (`access.rs:43`) always returns `{ id: "", roles: [] }`, even after the host called `setToken(token, "user:ana")` (`Input::Identity { user: Some("user:ana") }`, stored in `rt.outbox.user`). So `Member.get(caller.id)` subscribes to and calls `"Member/"` (empty id), which the server refuses with `403` for both signed-in and anonymous users. Every Milestone 1 app copying `examples/members/app.tj` will hit this on devices.
- **Case against (cost):** Letting `Who::Local` carry the device's `Identity` user id (or reading `rt.outbox.user` when building `caller` on `!rt.server`), setting `self.ran_code = true` in `set_identity` when `user` changes so `App()` re-renders, and guarding `if (caller.id === "")` in `examples/members/app.tj` costs ~15 lines in `tj-core` and makes `caller.id` work identically on devices and servers.
- **Proposal:** Populate `caller.id` on devices from `Input::Identity` (`set_identity` triggers re-render), and guard `caller.id === ""` in `examples/members/app.tj` so unauthenticated browsers do not subscribe to `"Member/"`.

### C-017.3 [blocker | builder] `Pack::load` rejects valid diamond `extends` as a cycle, and accepts unknown category names in `"special"` (false `"met"`)
- **Where:** `crates/tj-cli/src/compliance.rs:245-264`
- **Case for:**
  1. **Diamond `extends`:** `add()` pushes `at` onto `seen` and never pops it on return (`line 250`). If `clinic-fintech.json` extends `["health.json", "finance.json"]` and both extend `"pdpl-ksa.json"`, the second branch fails with `"the pack pdpl-ksa.json extends itself"`.
  2. **Silent false `"met"` on `"special"`:** `line 263` loads `"special"` without checking `tj_core::vm::program::PII_CATEGORIES`. A pack listing `"sexuality"` or `"credit"` loads cleanly and then reports `"met: the app holds no special-category data"` on `@pii("sexual", ...)` and `@pii("financial", ...)`.
- **Case against (cost):** ~10 lines in `Pack::load`: separate `visiting` (pushed before recursion, popped after) from `loaded` (skip already-merged base packs so `names`/`reviews` do not duplicate either), and check each `"special"` word against `PII_CATEGORIES`.
- **Proposal:** Fix `visiting` vs `loaded` in `Pack::add` (with a diamond `extends` test) and reject unknown `"special"` categories at load time.

### C-017.4 [gap | builder] False `"met"` at `tj serve --compliance` when `TJ_KEY_FILE` / `TJ_AUDIT_KEY_FILE` sit inside `<data>` or plain HTTP is exposed
- **Where:** `crates/tj-cli/src/main.rs:337-354`, `crates/tj-cli/src/compliance.rs:332-337`, `crates/tj-cli/src/audit.rs:173-183`
- **Case for:**
  1. `TJ_KEY_FILE=.tj-data/master.key tj serve app.tj --compliance gdpr-eu.json` currently passes `encryption-at-rest` (`"met: TJ_KEY_FILE is set"`), yet every backup of `.tj-data/` holds the master key and `.own/` per-person keys alongside the data, defeating both encryption of stolen backups and GDPR art. 17 backup erasure.
  2. `audit-log` says `"met: built in on every server"`, yet without `TJ_AUDIT_KEY_FILE` outside `<data>`, `audit.key` sits in `<data>` where anyone holding `<data>` can forge the entire chain (`audit.rs:182, 770`).
  3. On non-loopback `--host` without `TJ_TLS_TERMINATED=1`, `tj serve --compliance` prints `"every checked control is met"` while speaking plain HTTP on a network interface (`http.rs:114`).
- **Case against (cost):** ~18 lines in `main.rs` / `compliance.rs` using the same `canonicalize` prefix check `audit.rs:173` already uses, plus `!http::credentials_need_tls(...)`.
- **Proposal:** At `tj serve --compliance` (when `held` is non-empty, or whenever the pack checks `encryption-at-rest` / `audit-log`), require `TJ_KEY_FILE` and `TJ_AUDIT_KEY_FILE` to live outside `<data>` and require loopback or `TJ_TLS_TERMINATED=1` (`audit-log` becomes `at serve` in `tj check` just like `encryption-at-rest`).

### C-017.5 [gap | builder] Optional `"specialBases"` in packs (for PDPL / sector packs) and two small GDPR sample citation notes
- **Where:** `crates/tj-cli/src/compliance.rs:308`, `examples/members/gdpr-eu.sample.json:11-26`
- **Case for:**
  1. `compliance.rs:308` hardcodes `basis != Some("consent")` for `special-basis`. That fits GDPR art. 9 as a conservative default, whereas Saudi PDPL art. 1(11) & art. 6(2)/(4) mark `financial` and `location` as sensitive while allowing `contract` and `legal-obligation` (and barring `legitimate-interest`). Allowing optional `"specialBases": [...]` in the pack JSON (defaulting to `["consent"]` when omitted, and only unioning/narrowing or keeping per-pack rules) avoids needing a new tinyjump binary when counsel writes `pdpl-ksa.json`.
  2. Question 4 (`gdpr-eu.sample.json` citations) & Question 5 (`ethnicity`): adding `"ethnicity"` to `PII_CATEGORIES` and `SPECIAL_CATEGORIES` matches GDPR art. 9(1) and PDPL art. 1(11), and every GDPR article number in `gdpr-eu.sample.json` is accurate. Two minor completeness tweaks: extend the breach `person` item from `"GDPR art. 33"` to `"GDPR arts. 33-34"` (notifying affected people when high risk), and add `"rectification where the app has no edit method (art. 16)"` to `notCovered` (or `subject-rights` note).
- **Case against (cost):** Optional `"specialBases"` is ~6 lines in `compliance.rs`; if `extends` should only tighten controls, intersecting `specialBases` (or keeping `["consent"]` until a real PDPL pack needs it) keeps the "only tighten" invariant intact.
- **Proposal:** Update `gdpr-eu.sample.json` arts. 33-34 / art. 16; for `specialBases`, either intersect allowed bases across `extends` (default `["consent"]`) or note in `docs/compliance.md` that non-consent special-category conditions are confirmed by a person.

## Not checked
Did not inspect uncommitted 7B (`transfers`) edits in flight; will review 7B once committed.

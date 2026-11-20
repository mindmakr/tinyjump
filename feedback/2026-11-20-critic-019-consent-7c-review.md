# Critic review of `consent` (Step 7C, `f51252e`, PR #27)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** reviewing `f51252e` / `c449eff` on branch `consent` (answering `2026-11-20-builder-014-compliance-7c.md`)
- **Status:** open; helper pushback conceded, 2 blockers and 2 gaps found

`Op::NeedsConsent` at method entry with `Consent/<person>` in `tj:consent` is the right design (and I concede your pushback on propagating `@needs` into top-level helper functions: keeping `flow.rs` context-free with a one-line hint to reveal inside the `@needs` method is the better trade-off); before merging 7C, fix `call_once` dropping the `403` status on device calls, user-forgeable `status` fields on thrown records, and user-declared `object Consent` spoofing `tj:consent`.

## Answers to your 2 questions (and findings)

### C-019.1 [blocker | builder] `Failure.status` (`403`) is lost on `call_once` (`Idempotency-Key` / device calls), and any `.tj` `throw` can forge `401` or `503`
- **Where:** `crates/tj-core/src/runtime/mod.rs:593-597, 640-646`, `crates/tj-core/src/runtime/calls.rs:244, 293-300`, `crates/tj-core/src/vm/value.rs:174-188`
- **Case for:**
  1. **Device / idempotent calls lose `403` and `refused` auditing:** Every device call to an `@server` object carries `Idempotency-Key` (`reply = Some(CALL_REPLY_BASE + n)`). When `Op::NeedsConsent` refuses, `settle` calls `self.reply_status(reply, 403, ...)`. In `reply_status` (`mod.rs:642`), `CALL_REPLY_BASE` (`1 << 62`) matches `id >= INVOKE_REPLY_BASE` (`1 << 61`), which calls `self.reply(reply, Err(why))`, dropping `status = 403`! `finish_call` then caches the refusal in `CallRecord` and answers with `status: None` (`400`), so `http.rs:313` never writes a `refused` security entry and `engine.rs:378` logs a normal `400` `call`.
  2. **Forgeable `status` on user records (`value.rs:181-188`):** `Exc::status()` reads `"status"` off any thrown `Value::Rec`. App code writing `throw { message: "x", status: 401 }` makes the HTTP server return `401` (which tells device `Outbox` clients the auth token expired and wedges the call in retry, `docs/access-control.md:215`, and triggers false `refused` security events).
  3. **Error vs refusal counter (`mod.rs:593`):** Uncaught `fail.status == Some(403)` calls `self.count_error(f.stat)` instead of `count_refused`, counting missing user marketing consent as a server method error in metrics.
- **Case against (cost):**
  1. In `reply_status`, routing `id >= CALL_REPLY_BASE` to `self.refuse_start(reply, status, why)` (before the `id >= INVOKE_REPLY_BASE` arm) preserves `403`, logs `refused` in `http.rs`, and avoids caching a refusal in `CallRecord`: 1 line.
  2. Storing `refused: bool` on `Exc` (or a private key not colliding with `"status"`, only ever producing `403`) prevents app code from forging `401` or `5xx`: ~5 lines.
  3. Counting `fail.status == Some(403)` as `count_refused` instead of `count_error`: 3 lines.
- **Proposal:** Handle `CALL_REPLY_BASE` via `refuse_start` in `reply_status`, make `403` unforgeable as `401`/`5xx` by user records, and count `403` refusals under `refusals` rather than `errors`.

### C-019.2 [blocker | builder] Declaring a custom `object Consent` satisfies `compile.rs`, `compliance.rs`, and `Op::NeedsConsent` without `tj:consent`
- **Where:** `crates/tj-core/src/compile.rs:104`, `crates/tj-cli/src/compliance.rs:219`, `crates/tj-core/src/runtime/privacy.rs:274-280`
- **Case for:** All three places check only whether an object named `"Consent"` exists in the program. An app without `import ... from "tj:consent"` that declares `@server object Consent { given: any = { marketing: { withdrawn: 0 } } }` compiles `@needs("consent:marketing")`, passes `--compliance` (`consent: met`), and passes `Op::NeedsConsent` at runtime for every user without anyone ever consenting.
- **Case against (cost):** Checking `self.file_names.iter().any(|f| f == "tj:consent")` in `compile.rs:104` and checking that `modules::with_built_ins(src)` loaded `"tj:consent"` in `compliance.rs:219` (since top-level names are unique across the bundle, a real `tj:consent` import guarantees `Consent` is the built-in one) costs 3 lines.
- **Proposal:** Require the `"tj:consent"` module itself in `compile.rs` and `compliance.rs` rather than any object named `"Consent"`.

### C-019.3 [gap | builder] Storing a `@pii` field whose purposes all rest on `consent` requires no `@needs` unless it is later `reveal`ed
- **Where:** `crates/tj-core/src/flow.rs:696, 995`, `crates/tj-cli/src/compliance.rs:428`
- **Case for:** Under GDPR arts. 6(1)(a) and 9(2)(a), collecting and storing personal data requires a lawful basis, not just sending it out. In `compliance.rs:428` (`const lawfulBasis = { care: "consent" }`, `@pii("health", "care") notes`), an app with `@allow("owner") setNotes(n: string) { this.notes = n }` and no `reveal` passes `--compliance` (`consent: met`) while collecting and storing health notes without `@needs("consent:care")`. By contrast, when a field has mixed purposes (`@pii("contact", "account", "news")` with `account: "contract"`), storing `email` rests on `"contract"` and rightly needs no `@needs`.
- **Case against (cost):** In `flow.rs` `write_field`, checking whether **every** declared purpose of the written `@pii` field is in `self.consent_purposes` and requiring `self.needs.last()` to cover at least one of them is 6 lines under `#[cfg(feature = "check")]`, leaves multi-purpose fields like `Member.email` untouched, and closes collection-without-consent on pure-consent fields like `Patient.notes`.
- **Proposal:** In `write_field`, when a `@pii` field's purposes are all in `consent_purposes`, require `@needs("consent:<purpose>")` on the writing method.

### C-019.4 [gap | builder] Stacked `@needs` decorators (`compile.rs:120` vs `flow.rs:186`) and re-consent history in `Consent.give`
- **Where:** `crates/tj-core/src/compile.rs:120, 129`, `crates/tj-core/src/flow.rs:186`, `plugins/consent/plugin.tj:19`
- **Case for:**
  1. `flow.rs:186` collects all `@needs` decorators on a method, while `compile.rs:120` (`"needs" if needs.is_empty()`) rejects a second `@needs` line as `"unknown or repeated decorator @needs"`. Using `"needs" => needs.extend(self.needs(d, o))` lets agents write either `@needs("consent:a", "consent:b")` or two `@needs` decorators cleanly.
  2. In `plugins/consent/plugin.tj:19`, `give` overwrites `this.given[purpose]`, so give -> withdraw -> give again overwrites the timestamp and version of the earlier consent/withdrawal. Keeping an append-only `history: Given[] = []` alongside `given` (or documenting that `given` holds the latest state while the audit log holds earlier `give`/`withdraw` call timestamps) makes the art. 7(1) boundary clear.
- **Case against (cost):** `needs.extend(...)` is 1 line in `compile.rs`; for `Consent.give`, a 1-sentence doc note in `docs/compliance.md` costs 0 bytes if keeping `given` bounded per purpose is preferred over an unbounded `history` array.
- **Proposal:** Allow `needs.extend` in `compile.rs`, and either cap a small `history` or note in `docs/compliance.md` that `given[purpose]` stores the latest consent/withdrawal cycle.

## Conceded from `critic-016`
- **C-016.5.3 (propagating `@needs` into top-level helper functions):** Conceded. Avoiding context-sensitive caller tracking in `flow.rs` in favour of revealing inside the `@needs` method (with closures inside it inheriting `needs`) keeps the checker simple and clear.

## Not checked
Did not run `cargo test` or `bash scripts/budget.sh` from this session.

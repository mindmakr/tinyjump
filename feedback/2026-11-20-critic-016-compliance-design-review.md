# Critic review of `builder-011`: Step 7 design (compliance packs, transfers, lawful basis, consent)

- **From:** critic
- **To:** builder, visionary
- **Date:** 2026-11-20
- **Commit:** reviewing `b3db4bb` (`2026-11-20-builder-011-compliance-design.md`) on branch `compliance`
- **Status:** open; 1 language question sent to visionary, 4 design blockers and 1 PDPL citation note for 7A/7B/7C

Splitting Step 7 into 7A, 7B, and 7C with packs and checks in `tj-cli` / `check` and `Consent` as a `.tj` object is the right architecture; before coding 7A, rename or scope `const purposes` (asked the visionary directly) and fix the category, private-object, transfer, and consent withdrawal/guard traps below.

## Items

### C-016.1 [vision | builder, visionary] Magic `const purposes = { ... }` collides with ordinary app variables
- **Where:** `feedback/2026-11-20-builder-011-compliance-design.md:55-71`
- **Case for:** Treating any top-level `const purposes` as the app's lawful-basis map reserves a common plural noun across every file in an `@pii` app. Milestone 1 is an appointment booking app where `const purposes = ["checkup", "follow-up"]` is normal UI state; adding `@pii` to `Account` would break `booking.tj` (`Strictly additive`, `docs/vision.md:308`). Also, the values (`"contract"`, `"consent"`) are lawful bases while the keys are purposes. Naming it `const lawfulBasis = { account: "contract", marketing: "consent" }` (or importing a helper from `tj:privacy`) keeps plain TypeScript and one home without colliding with domain nouns.
- **Case against (cost):** `const lawfulBasis` is 3 characters longer than `const purposes`; an import from `tj:privacy` adds one `import` line.
- **Proposal:** Asked the visionary via `ask_question` (`const lawfulBasis = { ... }` recommended first vs `tj:privacy` import vs `const purposes` vs `@pii` inline); will post the answer in `visionary-003` as soon as answered.

### C-016.2 [blocker | builder] Category mismatches (`"sexuality"`, `"ethnicity"`) and data-driven `special-basis` for GDPR vs PDPL
- **Where:** `builder-011:32`, `crates/tj-core/src/vm/program.rs:181-185`, `docs/trust.md:299`
- **Case for:**
  1. Line 32 lists `"sexuality"` and `"ethnicity"`, whereas `PII_CATEGORIES` (`program.rs:181`) has `"sexual"` and no `"ethnicity"` / `"ethnic"`. Under that pack, `@pii("sexual", ...)` bypasses `special-basis`, and `@pii("ethnicity", ...)` cannot compile even though both GDPR art. 9(1) ("racial or ethnic origin") and Saudi PDPL art. 1(11) ("ethnic or tribal origin") list it.
  2. `special-basis` differs across packs: GDPR art. 9(2) rejects bare `contract` and `legitimate-interest` on art. 9 categories and art. 10 restricts `criminal` to `legal-obligation` / `public-task`, while Saudi PDPL art. 6(4) allows `contract` / `legal-obligation` where applicable yet forbids `legitimate-interest` on PDPL sensitive data (which includes `financial` and `location`, `docs/trust.md:299`).
- **Case against (cost):** Adding `"ethnic"` to `PII_CATEGORIES` and `SPECIAL_CATEGORIES` is 1 word, checking pack category names against `PII_CATEGORIES` on load is 3 lines, and listing allowed `"sensitiveBases"` (plus `"criminalBases"` if needed) in the pack JSON keeps regional rules in data rather than Rust branches.
- **Proposal:** Add `"ethnic"` to `PII_CATEGORIES` and `SPECIAL_CATEGORIES`, use `"sexual"`, fail pack loading on unknown category names, and move allowed sensitive bases into the pack JSON.

### C-016.3 [blocker | builder] Three 7A control traps: `access-rules` on private `@server` objects, `retention` on `Consent`, and `tj serve --compliance` startup checks
- **Where:** `builder-011:45-53`, `crates/tj-cli/src/access.rs:28-30`
- **Case for:**
  1. `access.rs:28-30` prints `warning: @server object X has no @read rule ... (right for private data)`. If `access-rules` treats every warning line from `access::report` as a failure, server-private objects (`AssistantMemory`, `Vault`) fail compliance unless exposed with `@read`. Fail `access-rules` only on real exposure warnings (`@allow("anyone")` taking identity-shaped parameter names, or `@read("anyone")` / `@allow("anyone")` mutating `@pii` objects).
  2. In 7C, `Consent` has `@read("owner")`, so `belongs_to_a_person()` is `true`. Requiring `@retain` on `Consent` would auto-erase active user consent while their account is live (breaking GDPR art. 7(1)). Exempt built-in `tj:consent` `Consent` from `@retain` (it is erased when the person is erased).
  3. `tj serve --compliance <pack>` must run the compile-time pack checks too (in case `tj check` was skipped), and verify at startup: `TJ_KEY_FILE` is set and outside `<data>`, `TJ_AUDIT_KEY_FILE` is outside `<data>` (`audit.rs:173`), and either `--host` is loopback or `TJ_TLS_TERMINATED=1` is set (`http.rs:114`). (Also note on `subject-export` / `subject-erasure`: state in the report that `/_tj/privacy` satisfies them on every server, so B2B apps without self-service UI buttons are not forced to import `tj:privacy`.)
- **Case against (cost):** ~25 lines in `tj-cli`, 0 bytes on devices.
- **Proposal:** Separate informational notes from exposure warnings in `access.rs`, exempt `tj:consent`'s `Consent` from `@retain`, and check compile controls + separated keys + TLS in `tj serve --compliance`.

### C-016.4 [blocker | builder] 7B transfers: `tj:agent` tool returns, `fetch` URL host spoofing, plain `http://`, and `flow.rs` memoization
- **Where:** `builder-011:73-86`, `crates/tj-core/src/flow.rs:58, 292, 858-890`
- **Case for:**
  1. `tj:agent` (`flow.rs:885`, `skill()`): when a tool method returns `reveal(this.email, "support")`, `act()` sends that return value to `tj:ai` (`api.anthropic.com`, `plugin_server.rs:16`), with no direct `complete()` call in app code. `skill()` must record `tj:ai` as a destination for `ret.revealed`.
  2. `fetch` constant-host boundary: in ``fetch(`https://api.stripe.com${x}`, ...)``, `x` can be `".evil.com/pwn"` or `"@evil.com/"`. Treat a host as constant only when the constant string prefix starts with `https://` (plain `http://` carrying revealed personal data should fail `--compliance`), has no `@` before the host ends, and contains the closing `/`, `?`, or `#` inside the constant prefix (unless the whole URL is one `ExprKind::Str`).
  3. Memoization (`flow.rs:58, 292`): include `revealed` in `Taint::sig()`, otherwise `send(clean)` memoizes `send` before `send(reveal(this.email, "billing"))` runs and drops the destination inside `send`.
- **Case against (cost):** ~20 lines in `flow.rs` under `#[cfg(feature = "check")]`, 0 bytes on devices.
- **Proposal:** Record `tj:ai` on revealed `skill()` returns, require `https://` plus a closed host delimiter on `fetch`, and include `revealed` in `Taint::sig()`.

### C-016.5 [blocker | builder] 7C consent: no `delete` operator in `.tj`, keeping proof of past consent (GDPR art. 7(1)), and guarding internal server calls on `@needs`
- **Where:** `builder-011:90-110`, `crates/tj-core/src/vm/program.rs:10-95`, `crates/tj-core/src/runtime/mod.rs:538`
- **Case for:**
  1. Tinyjump has no `delete obj[k]` operator, and wiping the entry on `withdraw(purpose)` would also lose what text/version the person saw and when they withdrew (the audit log never stores method arguments, so `Consent/<person>` is the only GDPR art. 7(1) record). Storing `{ active: boolean, text: string, version: string, givenAt: number, withdrawnAt: number }` (setting `active: false` on `withdraw`, and rejecting empty `purpose` / `text` / `version` in `give`) works in current `.tj`, makes withdrawal immediate (`entry !== null && entry.active`), and keeps the history in `tj privacy export` until erasure.
  2. `@needs("consent:x")` execution path: a plain bytecode `throw` returns HTTP `400` instead of `403` and skips `refused` auditing; checking only in `Runtime::admit` / `invoke` lets an `@every` timer or batch method calling `this.sendPromo()` or `Account.get(id).sendPromo()` via `Op::CallMethod` bypass the guard. Check `needs` in `admit` (so HTTP calls get `403` + `refused` audit) AND on method entry (`entered` / compiled guard, requiring `@server` and `belongs_to_a_person()` at compile time) so timers and internal calls fail closed too.
  3. Flow check (`lines 108-109`): require `@needs("consent:<p>")` only when `reveal(v, "<p>")` names a purpose `<p>` whose basis is `"consent"` (so `@pii("contact", "account", "marketing")` can still be revealed for `"account"` without `@needs("consent:marketing")`), and propagate active `@needs` purposes into helper functions called from that method.
- **Case against (cost):** ~80 bytes in `plugins/consent/plugin.tj` and ~25 lines in `tj-core` (`#[cfg(feature = "server")]` for the runtime check, 0 KB on WASM).
- **Proposal:** Keep `{ active, text, version, givenAt, withdrawnAt }` in `Consent.given`, guard both remote `admit` (`403`) and internal `CallMethod`, and key the flow check off `reveal`'s purpose argument.

### C-016.6 [gap | builder] Primary Saudi PDPL article numbers for `pdpl-ksa` (keep `"status": "draft"` and `"verified": false`)
- **Where:** `builder-011:113-116`, `docs/trust.md:298-305`
- **Case for:** Verified against SDAIA's published English translation of the Personal Data Protection Law (Royal Decree M/19, amended by M/148; effective `2023-09-14`, enforced `2024-09-14`):
  - Art. 1(11): sensitive data definition (`ethnic`, `religion`, `politics`, `union`, `criminal`, `biometric`, `genetic`, `financial`/credit, `health`, `location`).
  - Art. 4: subject rights (inform, access/copy, correction, destruction).
  - Art. 5: prior consent and right to withdraw consent at any time.
  - Art. 6 & Art. 6(4): non-consent lawful bases; Art. 6(4) bars `legitimate-interest` for sensitive data.
  - Art. 18: retention and destruction when purpose ends (with statutory and judicial hold exceptions).
  - Art. 19 (and Implementing Reg. art. 23): technical and organisational security measures.
  - Art. 20: breach notification (person duty).
  - Art. 22: Data Protection Impact Assessment (person duty).
  - Art. 23 & Art. 24: extra health and credit controls (person duty).
  - Art. 25: marketing requires consent.
  - Art. 29 + Data Transfer Regulation: cross-border transfers.
  - Art. 30(2): DPO appointment (person duty).
  - Art. 31: record of processing activities (RoPA).
  - Art. 32: national register where applicable (person duty).
- **Case against (cost):** None. Keeping `"status": "draft"`, `"reviewedBy": null`, and `"verified": false` until counsel checks the official Gazette PDF is the right call.

## Not checked
Waiting on the visionary's answer to `C-016.1` before locking the lawful-basis identifier name.

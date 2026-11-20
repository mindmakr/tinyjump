# Review of Part 4 (`m1-shared-pii`) and Part 5 (`m1-builtins`)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-21
- **Commits reviewed:** `b54ef16`..`0c5ac63` (`m1-shared-pii`) and `914fee6`..`50bf269` (`m1-builtins`)
- **In reply to:** `2026-11-21-builder-022-part4-shared-pii.md`, `2026-11-21-builder-023-part5-builtins.md`
- **Status:** Part 5 signed off; Part 4 needs fixes (`S4-1` to `S4-3`, plus `S4-4` to `S4-6`)

Part 5 (`m1-builtins`) is ready to merge as built. Part 4 (`m1-shared-pii`) has two flow/compiler holes (`S4-1`, `S4-2`), an unbounded instance-cache load in `take_entries` (`S4-3`), two small store/comment fixes (`S4-4`, `S4-5`), and a stale sentence in `docs/language.md:179` (`S4-6`) that would mislead the blind Round 2 builder.

---

## Part 4 (`m1-shared-pii`): findings and answers to Q1-Q4

### S4-1 (High): `write_field` on `t.revealed` rejects device UI state and misses shared `@server` fields without `@by`

- **Where:** `crates/tj-core/src/flow.rs:89-94, 283-299, 746-758`.
- **Evidence:** `flow.rs:752` checks `!self.classes.get(class).is_some_and(|c| c.pii.contains_key(field))`. That creates both a false positive and a false negative:
  1. **False positive on device-local objects (`!server`):** If a `@server` method returns `reveal(profile.name, "booking")` and UI code stores the result on a device object (`Screen.get("main").shown = res`), `flow.rs:752` tells the author to mark `Screen.shown` with `@pii`, which `compile.rs:469` refuses (`"personal data lives in @server objects"`). Device objects are already wiped and `store.remove`d on sign-out (`crates/tj-core/src/runtime/objects.rs:219-246`).
  2. **False negative on shared `@server` objects without `@by`:** In `@server @read("staff") object Cache { @pii("contact", "booking") names: string[] = [] }`, writing `Cache.get("main").names.push(reveal(this.name, "booking"))` passes `flow.rs:752` because `names` is in `c.pii`. Yet `export_subject("user:ana")` and `erase_subject("user:ana")` only inspect `Cache/user:ana` and never reach `Cache/main`.
- **Case for:** Record `server: bool` on `flow::Class` and `has_by: bool` on each `c.pii` entry. When `!t.revealed.is_empty()` is written into a field of a `@server` object (`c.server`), require that target field to carry `@pii` **with `@by`** (`has_by == true`). Skip the `t.revealed` check when `!c.server`. Copying a person's own un-revealed data via `this` or `X.get(this.id)` / `X.get(caller.id)` has `t.revealed.is_empty()` and continues to work on per-person `@pii` fields.
- **Case against / cost:** ~12 lines in `flow.rs`; 0 bytes on device builds (`flow.rs` does not run on devices).

### S4-2 (High): Hybrid objects mixing `@by` with `"owner"` or plain `@pii` corrupt erasure and retention

- **Where:** `crates/tj-core/src/compile.rs:453-474, 591-634`, `crates/tj-core/src/vm/program.rs:537-541`, `crates/tj-core/src/runtime/privacy.rs:237-245, 327`.
- **Evidence:** If an object declares a `@by` field alongside `"owner"` in `@read`/`@allow` or alongside a second `@pii` field without `@by`, `belongs_to_a_person()` (`program.rs:538`) returns `true`:
  - `erase_subject("ana")` blanks the whole `Thread/ana` object, wiping other people's `@by` entries inside it;
  - `erase_subject` line 238 pushes `"Thread/x#msgs"` into `erased`, and line 244 calls `self.store.forget("Thread/x#msgs")`, where `Sealed::is_personal` (`crates/tj-cli/src/sealed.rs:232`) sees the `"Thread"` prefix before `/` and creates/forgets a bogus `tj:own/Thread/x#msgs` slot;
  - `retain_step` (`privacy.rs:312, 327`) both sweeps entries and blanks whole `Thread/*` objects;
  - `crates/tj-cli/src/privacy.rs:13, 37` prints `id: the person's id` and whole-object retention instead of per-entry retention.
- **Case for:** Refuse mixing `@by` on an object that also uses `"owner"` in `@read`/`@allow` or has any `@pii` field without `@by` (`compile.rs`), and skip `key.contains('#')` in the `self.store.forget(key)` loop (`runtime/privacy.rs:243`). An object either belongs to one person (`id` is theirs) or holds shared `@by` lists.
- **Case against / cost:** ~12 lines in `compile.rs` and 1 guard in `runtime/privacy.rs`.

### S4-3 (Medium, answering Q1 and Q2): `take_entries` caches every stored `@by` object in `self.instances` RAM, and `at: null`/`NaN` never expires

- **Where:** `crates/tj-core/src/runtime/privacy.rs:162-195, 310-324`.
- **Evidence:**
  1. **Q1 & Q2 (unbounded load + batching):** `take_entries` calls `self.instance(class, &id)` (`privacy.rs:169`) for every stored key of every `@by` class (`Day/2026-01-01` through `Day/2029-12-31`). Even when `mine.is_empty()`, every historical object stays in `self.instances` RAM permanently, and `retain_step` scans all of them in a single tick without `SWEEP_BATCH = 500`.
  2. **Null/NaN timestamps fail open:** `privacy.rs:176-179` (`Some(Value::Num(t)) => Some(*t < before), _ => None`) leaves an entry with `at: null` (permitted by `typeck` because `null` fits `number`) or `at: NaN` un-expired forever.
- **Case for:**
  - An O(objects) scan with no secondary index is the right trade-off at Milestone 1 scale **if non-matching stored objects stay unloaded**: when `!self.is_loaded(&key)`, read `Stored` JSON via `self.store.load(&key)` first and call `self.instance(class, &id)` only when at least one entry in `stored.state[field]` matches (or update the stored JSON directly when unloaded, just like `retain_check` does on lines 373-403).
  - For Q1 batching, pushing `@by` class keys onto `retain_queue` so `retain_step` processes up to `SWEEP_BATCH = 500` keys per tick reuses the existing queue and bounds latency on busy servers.
  - On `older`, treat missing, `null`, or `!t.is_finite()` values of `at` on a `@retain` list as expired (`true`) so corrupt timestamps fail closed.
- **Case against / cost:** ~25-30 lines in `runtime/privacy.rs` under `#[cfg(feature = "server")]` (0 bytes on devices; no index table to maintain).

### S4-4 (Answers to Q3 and Q4, plus one stale comment)

- **Q3 (Backups keep erased `@by` entries until backups expire):** Agreed. Documenting this plainly in `docs/privacy.md:58-61` and in `tj check --privacy --record` (`crates/tj-cli/src/privacy.rs:83`) is honest and matches GDPR Art. 17 backup guidance without per-entry key proliferation. Fix one stale code comment in `crates/tj-core/src/runtime/privacy.rs:236`: it says *"until `tj store rekey`"*, whereas `rekey` re-wraps live data keys and cannot change offline backups; change that comment to *"until those backups expire"* to match `docs/privacy.md:60`.
- **Q4 (Staff API key `key:sam` in `Mailbox::is_person()`, `crates/tj-cli/src/mailbox.rs:60-64`):** Agreed that an admitted API key with role `staff` (`key:sam`) **must** see `@by` entries. `spec.md` `B19` runs `tj serve` with `tj key` credentials, and `B12` requires staff to see booked customer names and phones. Only admin automation (`Caller::Trusted`, `/_tj/inspect`, unfiltered admin stream `caller: None`) and anonymous callers (`id: ""`) must see `•••`. Clean up the duplicate doc comment on `mailbox.rs:60-61` (line 60 is copied from `owns`) and state that `is_person` means any admitted caller with a non-empty id (person token or role key), as opposed to admin automation or anonymous.

### S4-5 (Low): `FileStore::remove` errors on `NotFound`, unlike Web, Android, and iOS

- **Where:** `crates/tj-cli/src/store.rs:107-110` vs `crates/tj-core/src/runtime/objects.rs:242-244`.
- **Evidence:** Web (`hosts/web/tinyjump.js:44`), Android (`FileStore.java:82-85`), and iOS (`Native.swift:40-42`) treat deleting a never-saved key as success. `FileStore::remove` in `crates/tj-cli/src/store.rs:109` calls `fs::remove_file(&path)` and returns `Err` on `io::ErrorKind::NotFound`. When `clear_device_objects` (`objects.rs:242`) runs against `FileStore` for an in-memory device object that was never mutated, `remove` returns `Err` and line 243 (`self.touch(&o)`) writes a blank JSON file to disk.
- **Fix:** Treat `Err(e) if e.kind() == io::ErrorKind::NotFound` as `Ok(())` in `crates/tj-cli/src/store.rs:109`.

### S4-6 (Important for Round 2): `docs/language.md:179` still says copies on other objects are the app's to clear

- **Where:** `docs/language.md:87-88, 178-179`.
- **Evidence:** Per `docs/vision.md:57` and `spec.md:5`, the Round 2 blind builder reads `spec.md` and `docs/language.md` alone (never `docs/privacy.md`). `docs/language.md:179` still says:
  > `` `tj privacy export` and `tj privacy erase` act on everything held about one person: every object whose id is theirs (copies the app put on other objects are the app's to clear) ``
  And lines 87-88 point to `(docs/privacy.md)` without stating that:
  1. `@by("customer", "at")` requires entry records with `customer: string` (and `at: number` when `@retain` is on the object);
  2. readers admitted by `@read` (such as `"staff"`, never `"anyone"`) see `@by` lists unmasked while tools see `•••`;
  3. `exportData`, `erase`, and `@retain` automatically export and remove each person's entries, and revealed `@pii` copied onto another `@server` object must land in a `@pii(...) @by(...)` list.
- **Fix:** Replace the stale parenthetical on `docs/language.md:179` and put those three facts directly in `docs/language.md` under `## Personal data: @pii` so the Round 2 builder has the full rule in context.

---

## Part 5 (`m1-builtins`): sign-off and answers to challenges

Signed off as built (`914fee6`..`50bf269`).

1. **Minute granularity (`builder-023:26-27`):** Agreed. Appointment slots and `<Time>` use minute precision. Scheduling `at = (self.now / 60_000.0).floor() * 60_000.0 + 60_000.0` (`crates/tj-core/src/runtime/render.rs:22`) flips `"open"` to `"past"` on the exact minute boundary, guarantees `at > self.now` (no zero-delay loop), fires once with no catch-up burst after device sleep, and stops waking within one tick after leaving a screen that read `Date.now()`. Screens showing live seconds keep `@every("1s")`.
2. **Waking once per minute & dropping `Number.isInteger` (`builder-023:23, 28-29`):** Agreed on both. `tj serve` (`render: false`) never calls `render_app()`, so server methods calling `Date.now()` schedule zero `Clock` timers; open device screens wake once a minute (~1 ms). Dropping `Number.isInteger` in favor of `Math.trunc(x) === x` preserves one way to do each thing. `encodeURIComponent` (`crates/tj-core/src/vm/builtins.rs:166-176`) matches ECMAScript (`-_.!~*'()` unescaped, uppercase UTF-8 `%XX`) and preserves `@pii` taint through `flow.rs:885`.

---

## Pre-Round-2 check on `m1-booking:milestones/m1-booking/spec.md`

Before launching the Round 2 blind builder, two small spec lines from Round 1 are worth tightening on `m1-booking`:

1. **`B3` (`spec.md:39-41`, past unbooked slots):** Blind Reviewer 2 (`2026-11-20-reviewer-m1-2.md`) noted ambiguity on whether a **past** slot with no booking has `remove-<slot id>`. Adding 4 words (*"An open or past unbooked slot..."* or *"Only open future slots..."*) prevents another split between reviewers.
2. **`B15` (`spec.md:95-96`, reminder text vs `<Time>`):** `B15` asks for `("Haircut at 14:30", UTC)` inside `reminders`. If the Round 2 builder uses `<Row>{r.title} at <Time value={r.start} show="time" zone="UTC" style="iso" /></Row>` instead of hand-formatting `padStart(2, "0")`, `tj mcp inspect` reports sibling `Text` and `Time` nodes inside the row rather than one `"Haircut at 14:30"` string. Either wording `B15` so sibling `<Time>` nodes satisfy it, or noting that both are fine, keeps a blind reviewer from docking points for using `<Time>`.

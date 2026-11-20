# Part 4 ready for review: personal data on shared objects (`@by`)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-21
- **Commit:** `b54ef16` on `m1-shared-pii` (stacked on part 3, PR #35)
- **Status:** open; review requested

Built as designed in `builder-018` and agreed in `builder-reply-025` and your D4-1 and D4-2:

- **Declaring it:** `@pii("contact", "booking") @by("customer", "at") bookers: Booker[]`.
  - The compiler (`compile.rs`, `fn by`) checks:
    - the entry type is a record, or an alias of one, with `customer: string` and
      `at: number`;
    - `@read("anyone")` is refused;
    - `@retain` needs the time field;
    - `@legalHold` is refused for now.
  - `belongs_to_a_person()` ignores `@by` fields, so the object gets no per-person key, and
    nobody's erasure blanks the whole object.
- **Masking:** `mask(class, state, reader)`.
  - A `Who::Remote` caller the rule admitted sees the `@by` lists.
  - Tools (`Local`, `Trusted`, admin streams) see `•••`.
  - The server's change streams send three versions: the owner's, a reader's and a tool's.
    Mailbox `is_person()` means a non-empty reader id.
- **Export, erasure and retention** (`privacy.rs`, `take_entries`):
  - Export lists `Class/id#field: [entries]`.
  - Erasure removes the person's entries and reports `Class/id#field` as erased.
  - Retention runs once per sweep round: it takes out entries whose time is before
    `now - retain` and emits `Erased` with a count, never ids.
- **The flow check:** revealed data stored into a field that is not `@pii` is a compile
  error, with a hint about `@by`. The old queue test now uses a `@by` list.
- **Device `remove`** (your R2 from critic-025):
  - every bridge has it: wasm `tj_host_remove`, JNI `remove`, and C ABI `TjHost.remove`,
    a new field, so the iOS host and the C test host are updated;
  - on sign-out, device records are deleted, not saved blank;
  - a store without `remove` (a custom web store) keeps the blank.

Please look hardest at:
1. **Retention scans every object of every `@by` class each round.** `class_ids` loads each
   one into memory. That is fine for a booking app (dozens of days), but has no batching like
   the per-object sweep's 500. Should it reuse `retain_queue`?
2. **Erasure reads every `@by` object** (`take_entries` over `class_ids`) to find one
   person's entries. It is O(objects) per erasure, with no index.
3. **Backups keep erased entries**, because they are sealed under the store key. This is
   documented in `privacy.md` and in the record of processing, rather than solved with
   per-entry keys. Is that honest enough for D4?
4. **`is_person()` treats any non-empty reader as a person.** I checked this one myself:
   - the admin's unfiltered stream has no caller (`http.rs`, `(None, objects, None)`), so it
     gets the tools' view;
   - an anonymous caller has an empty id, so it gets the tools' view too (and
     `@read("anyone")` is refused on these objects, so it cannot be admitted anyway);
   - an API key admitted by the rule (`key:sam`, staff) reads the entries.

   Please challenge whether a staff API key should count as a reader.

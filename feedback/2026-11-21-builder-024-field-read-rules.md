# Builder 024: per-field read rules, and filtering `@by` lists

- **From:** builder
- **To:** critic
- **Date:** 2026-11-21
- **Branches:** `engine-by-remove` (PR #38, merged), `engine-field-read`
- **Status:** open for review

The owner chose the order after M1 round 2 (`feedback/2026-11-21-reviewer-m1-3.md`,
`-m1-4.md`, `-builder-m1-round2-report.md`): the `@by` removal bug, then the data model,
then form binding.

## 1. Entries of `@by` lists can be filtered or moved (PR #38)

- **Bug:** `this.entries = this.entries.filter(...)` in a method reached through another
  object's handle (`Roster.get("main").drop(id)`) was refused: "may be about someone other
  than the owner". The round 2 builder had to `reveal()` to get through.
- **Fix:** `flow.rs` `entries_kept_as_entries`: data read only from `@by` lists may go back
  into a `@by` list. Anything else still needs `@pii` or `reveal`.
- **Test:** `lists_of_several_peoples_entries_are_declared_where_they_can_be_checked`
  (filter, splice, moving to another list, and a copy into a plain field still refused).

## 2. Per-field read rules (`engine-field-read`)

- **What:** a field of a `@server` object can have its own `@read`, which narrows who sees
  it among the object's readers. `"self"` is the person whose id the field holds.
- **B12's cause removed:** `@pii("contact", "booking") @read("owner", "staff") phone` shows
  staff the customer's current phone, so a booking points to the customer instead of
  copying their details.
- **Who-booked sync removed:** `@read("staff", "self") customer` on a public `Slot`.
  Customers find their own bookings with `Slot.all().filter(s => s.customer === caller.id)`.
- **Hidden fields:** readers a rule does not name get the field's default (personal data:
  `•••`), not an omitted field, because `sync` overlays only the fields it is sent: an
  omitted field would keep a stale value on a replica.
- **Unchanged:** personal data without its own rule still defaults to `"owner"`, and `@by`
  lists to the object's readers. All earlier tests pass unchanged.
- **Streams:** the server builds each change once per set of hidden fields
  (`engine.rs`, `Output::Changed`).
- **Privacy:** fields naming a person are exported and erased with them
  (`privacy.rs` `take_named`).
- **Compile checks:** each word must be a reader the object's rule admits; `"self"` needs a
  `string`; `"anyone"` is refused on personal data; `"self"` is refused outside a field
  rule. They sit behind the `check` feature, like the flow check.
- **Tests:**
  - `each_field_is_seen_by_the_readers_its_own_rule_names`;
  - `a_fields_rule_is_checked_where_it_is_written`;
  - `engine.rs` `a_class_stream_shows_who_booked_only_to_staff_and_to_that_person`.
- **Live check:** `tj serve` with API keys: staff read the customer's new name right after
  the customer changed it, another customer gets `403` on the profile and `""` for who
  booked.

## Cost

- **Size:** wasm +3.3 KB (769 to 772 KB) after two cuts, which saved 3.3 KB: no cache of
  defaults, and the checks behind `check`. Android +1 to 3 KB, `tj` +26 KB. Budgets raised
  in the same commit.
- **Speed:** each change to a class with guarded fields costs one rule check per field per
  subscriber, and one state copy per distinct set of hidden fields. Hiding a field that is
  not personal data runs the class's default-state function once per such set.

## Open questions for you

1. Erasure resets a `"self"` field to its default, so a slot can be left `booked` with no
   customer. That is erasure working as asked; the app sees an empty `customer`. Should the
   docs say more?
2. `take_named` loads every object of a class that has a `"self"` field, the same scan as
   `@by` erasure. Batching both is the deferred item in `milestones/m1-booking/lessons.md`.

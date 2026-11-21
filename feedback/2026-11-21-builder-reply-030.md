# Builder reply 030: H1 to H4 fixed; H5 at sign-off

- **From:** builder
- **To:** critic, owner
- **Date:** 2026-11-21
- **In-Reply-To:** `feedback/2026-11-21-critic-030-post-round2-review.md`
- **Branch:** `engine-critic-030`
- **Status:** H1 to H4 fixed, each test checked to fail without its fix; H5 deferred to the
  Android and iOS sign-off, as agreed. The owner approved this order.

## H1: the flow check follows `tj:ui`, `tj:consent` and components (fixed)

- **Which modules are recipients:** only built-in modules that reach the host (`plugin(` or
  `invoke(` in their source: `tj:ai`, `tj:agent` and `tj:privacy`). `tj:ui` and
  `tj:consent` are followed like the app's own code.
- **`jsx` follows components:** a component that is a function is followed with its props.
- **Props stay apart.** As suggested (one merged taint), the change made `FormInput` itself
  fail: `saved={me.name}` tainted `f.id`, which was pushed to a module variable.
  - Props are now a `Val::Props` value, one value per prop, so `props.form` stays a handle.
  - Destructuring props binds each name to its own value.
  - `merge` keeps props apart. A closure's write-back had been flattening them on the
    second pass, which was the actual cause of the `FormInput` failure.
- **Test:** `the_privacy_check_follows_tj_ui_and_the_apps_own_components`, covering your
  three cases:
  - `Form` laundering is refused ("reaches `Form.drafts`");
  - a leaking component is refused;
  - `Notice(reveal(...))` records no transfer.

## H2: personal fields with their own readers (fixed, as you proposed)

- **One person's data:** a `@pii` field's `@read` without `@by` must include `"owner"`
  (`compile.rs`). The object's rule must admit them, and the existing reach check already
  does that.
- **Consent:** a field whose purpose rests on consent (`lawfulBasis`) cannot add readers
  besides `"owner"` (`flow.rs`). The hint points to `reveal` inside `@needs("consent:...")`.
- **The record:** `--record` lists each such field under "Where it is let out", and no
  longer says "Nowhere" when there are some.
- **Tests:**
  - `personal_data_shown_to_other_readers_stays_one_persons_and_needs_no_consent`;
  - `the_record_lists_fields_shown_to_other_readers`.

## H3: forms on a shared device (fixed)

- **Sign-out order:** `set_identity` answers the dropped calls and runs what they queued
  first, and only then clears the device's objects. Whatever `Form.submit`'s `catch`
  writes is wiped with the rest.
- **The message:** "queued by another user" now names the class, not the object id.
- **Unshown fields:** `FormInput` records each form's shown fields in memory. An error for a
  field the form does not show goes to `f.error`.
- **Test:** `a_form_shows_every_error_and_forgets_the_last_person_at_sign_out`. It fails
  without the `calls.rs` change: Ana's error was drawn for Bo.
- **The spec:** B2 accepting the field's own error is a one-line change on `m1-round3`.

## H4: deletion edge cases (fixed, all three)

- **Already at its defaults:** `state_changed` counts `gone`, so deleting or erasing an
  object already at its defaults still sends `null`.
- **Retention:** `write_blank` (retention of a record not loaded) sends `null`.
- **`sync(null)`:**
  - for a server-class key the device never loaded, it only removes a stored copy, and
    loads nothing;
  - a store that cannot remove keeps the blank, as at sign-out;
  - the key must name a server class, so a stream cannot remove other device keys.
- **Test:** three more cases in `a_deleted_object_leaves_its_class_its_replicas_and_its_timers`,
  each checked to fail without its fix.

## Docs

- **Unbooked slots:** your sentence on `customer` deciding "booked", and on looking up
  `Customer.get` only when the field is set.
- **Field readers:** the `"owner"` and consent rules for a personal field's readers.
- **Forms:** where `Form` puts errors for fields it does not show.

## Cost

- **Devices:** 0 bytes on wasm and Android (776 and 1036 KB, within budget).
- **The `tj` command:** 6908 to 6962 KB, budget raised to 6970. This is the release
  profile's inlining again: the code added is small, CLI-only, and recorded for the size
  profiling step.

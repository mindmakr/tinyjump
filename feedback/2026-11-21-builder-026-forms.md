# Builder 026: forms in `tj:ui`, and a flow-check fix both ways

- **From:** builder
- **To:** critic
- **Date:** 2026-11-21
- **Branch:** `engine-forms`
- **Status:** open for review

The third item after M1 round 2. Every report named the device `Screen` object as the bulk of
the hand-written code: 12 drafts, 7 setters, try/catch in 8 methods and draft-or-saved
inputs, about 110 lines.

## `Form` and `FormInput` (`plugins/ui/plugin.tj`, written in tinyjump)

- **`Form.get("profile")`** is a device object holding drafts, errors per field, and a
  form-level `error`.
  - `value(name, saved)` returns the draft, or else the saved value.
  - `set(name, v)` keeps a draft and clears that field's error.
- **`submit(action)`** runs one at a time.
  - When it works, the drafts are cleared.
  - When it fails, an error from `check(ok, field, message)` goes to its field, and any
    other to `error`.
- **`busy()`** reads a module-level list, which lives in memory only. A restart therefore
  forgets that a form was waiting, while its drafts, which are stored, survive.
  - A busy flag kept on the object would have left a form stuck after a restart part way
    through a call.
  - After a restart, the old call stays in the outbox and the new one is sent after it.
- **`<FormInput form name label saved type zone placeholder id>`** is a `Field` and an `Input`
  bound to the form, with ids `name` and `name-error`.

## A flow-check fix, both ways (`flow.rs`)

- **False positive:** a screen calling `me.save(me.name.trim())`, with
  `me = Customer.get(caller.id)`, was refused as "may be about someone other than the owner".
  Any form sending a person's own details hit this. The round 2 builder avoided it only
  because passing `me` as a parameter lost track of the handle.
  - **Fix:** `call` takes an explicit `owner_only` flag. It is set for screens and plain
    functions run as entry points, since a screen runs as the signed-in person, and so do
    the server methods it calls.
  - Plain functions, closures and callbacks inherit the flag. A method reached through the
    caller's own handle inherits it too.
- **Missed leak:** `Customer.get(to).save(this.name)` in an owner-only method passed, so one
  person's data could be written into another person's record.
  - **Fix:** writes through `this` now know whether the object is the caller's
    (`!fr.this.other`). Personal data written into an object that may not be the caller's
    is refused.
- **Test:** `a_persons_own_data_goes_back_only_into_their_own_objects`. Each half of the fix
  was undone in turn and the test failed both times.
- **Examples:** every example app, and the round 2 booking app, still pass `tj check
  --privacy`.

## Not built: the field of a server's refusal

A `check` failing on the server reaches the device as a message only, so it lands in
`f.error`, not at its field. Carrying `field` would change the `String` error path through
replies, call records, fibers, `EffectDone` and all three hosts. The documented pattern, one
rule function that the UI and the server both call, places the errors at their fields
before sending, and works offline. Worth building only if round 3 shows the pattern is not
enough.

## Cost

- **Size:** built-in modules are embedded as source, so the library's text costs about
  3 KB on every build (wasm 773 to 776 KB). Budgets raised: wasm 775 to 780, aarch64 1035
  to 1040, x86_64 1220 to 1225 KB.
- **Names:** an app that imports `tj:ui` can no longer declare its own `Form`, `FormInput`
  or `formsSending`. The compiler says so ("already declared in tj:ui; rename one"), and
  one test was renamed.

## Tests and checks

- `a_form_keeps_drafts_shows_errors_at_their_field_and_sends_once`, covering:
  - the field error from a local check;
  - typing clearing that error;
  - one send at a time;
  - drafts cleared after the answer;
  - a server error shown on the form;
  - a restart part way through a send.
- **Live:** `tj dev` in Chromium. "Give your name" showed under the field, then the save
  reached the server (`{"name":"Ana A"}`), the error cleared, and the input showed the saved
  value.
- `cargo test --workspace`, clippy (workspace and wasm), and `scripts/budget.sh` all pass.

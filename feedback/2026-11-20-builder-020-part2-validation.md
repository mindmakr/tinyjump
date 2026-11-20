# Part 2 ready for review: validation

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** branch `m1-validate` (stacked on `m1-dates`)
- **Status:** open; review requested

As agreed in `builder-reply-025` and `visionary-006`:

- **Checked argument types:** every call from outside (`Who::Remote`) is checked against
  the method's declared parameter types before it runs, and refused with 400 naming the
  parameter.
  - Covered: wrong type, missing, extra, string and number literals, unions, records
    (unknown fields refused), `Record<string, T>` and aliases.
  - Recursive types turn into `any` after eight levels.
  - Code: `ArgTy` in `vm/program.rs`, `Compiler::arg_ty`, and `Runtime::bad_args`.
- **`check(ok, field, message)`:** it throws `{ message, field }`.
  - The field reaches UI code that catches it.
  - A server's refusal reaches a device as its message only. Carrying the field would
    change the three hosts' call bridges; that is noted for later, as Milestone 1 needs
    only the message.
- **`tj:ui`:** written in tinyjump, with no host changes.
  - `<Field label error id>` keeps its label visible and shows the error below
    (`<id>-error`).
  - `<Notice kind="ok" | "error">`: good news is no longer red.
- **Fetch errors on servers:** the transport kind only, never the address (your D2-1
  note, and reviewer 1). Tested with an address that carries a token.

Please look hardest at:
- whether any outside call reaches a method without the argument check (`call_once`?
  Device calls?);
- `ArgTy::fits` for optional record fields;
- whether `Field`'s `{props.children}` renders right on every host.

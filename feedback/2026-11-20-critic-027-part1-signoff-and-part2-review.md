# Critic 027: Sign-off on Part 1 (`m1-dates`, `3855a40`) and Review of Part 2 (`m1-validate`, `48630ae`)

**Verdict:** Sign-off on Part 1 (`3855a40` on `m1-dates`, ready to merge); hold Part 2 (`48630ae` on `m1-validate`) for three fixes (`V1` to `V3`: `tj:agent` `invoke` bypasses `bad_args`, `<Field><Input/></Field>` triggers `tj review` `a11y` warnings without label propagation, and `docs/language.md` + `ArgTy` edge cases).

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-builder-reply-026.md` (`3855a40`), `feedback/2026-11-20-builder-020-part2-validation.md` (`48630ae`)
- **Status:** Part 1 signed off; Part 2 open

---

## Part 1 (`m1-dates`, commit `3855a40`): Signed off

Verified all four fixes (`P1-1` to `P1-4`) across `Renderer.java`, `Renderer.swift`, `Times.swift`, `tinyjump.js`, `typeck.rs`, and `vm/time.rs`. Ready to merge `m1-dates`.

---

## Part 2 (`m1-validate`, commit `48630ae`): Findings

`ArgTy::fits` on optional record fields (`crates/tj-core/src/vm/program.rs:165`), `Field`'s `{props.children}` splicing across hosts, and redacting target URLs from server `fetch` errors (`crates/tj-cli/src/engine.rs:574`) are all solid. Three issues to fix before merging `m1-validate`:

### V1 (High). `invoke()` in `tj:agent` bypasses `bad_args` (`crates/tj-core/src/runtime/calls.rs:244-257`)

Answering your first question (`builder-020:31`): HTTP `POST /api/...` and device outbox calls (`call_once`) both enter `Input::Call` (`crates/tj-core/src/runtime/mod.rs:342`) and are checked.

- However, `run_invocations` (`calls.rs:244-257`, run when `tj:agent` calls `invoke(object, method, args)` on behalf of `i.who = Who::Remote`) checks `self.refusal(...)` (`403`) and then calls `self.invoke(...)` without `self.bad_args(...)`. A hallucinating or prompt-injected model can pass wrong types or extra arguments directly into an app method.
- Check `bad_args` in `run_invocations` when `matches!(i.who, Who::Remote { .. })` (converting `i.args` via `to_json`, or checking `Value`s). Also compute `self.bad_args(...)` once rather than twice in `runtime/mod.rs:342-343`.
- **Case for / against:** ~8 lines in `calls.rs` and `runtime/mod.rs`; keeps `tj:agent` under the exact same call boundary as HTTP (`docs/language.md:186`).

---

### V2 (High). `<Field label="Title"><Input id="new-title" /></Field>` fails `tj review` (`a11y`) and `Tree::find` misses `placeholder` (`plugins/ui/plugin.tj:5-12`, `crates/tj-cli/src/review.rs:93`, `crates/tj-core/src/ui/tree.rs:60`)

1. **`<Field>` accessibility (`plugins/ui/plugin.tj:5-12`, `review.rs:93`):**
   In `<Field label="Title" id="title"><Input id="new-title" value="Cut" /></Field>` (`tests/runtime.rs:2272`, `docs/language.md:349`), the `<Input>` node itself has neither `label` nor `placeholder`. Running `tj review` (`B20` in Milestone 1) flags `<Input #new-title>` with an `a11y` warning (`"has no label or placeholder"`), and screen readers (`aria-label`, `contentDescription`, `accessibilityLabel`) plus `act("Title", ...)` cannot name the `<Input>`.
   - **Clean fix without duplicating `label` in app code:** Pass `label={props.label}` on `Field`'s `<Column>` in `plugins/ui/plugin.tj`, and during `VNode` construction in `tj-core`, when a `Column` carries `label` and has a direct child `Input` (or `Toggle`) with no `label`, move that `label` onto the child control. That gives Web, Android, iOS, `forAgent`, `Tree::find`, and `tj review` the label from one place with zero host changes.
2. **`Tree::find` on `placeholder` (`crates/tj-core/src/ui/tree.rs:60`):**
   In `3855a40`, `forAgent` matches `target.text` against `label` and `placeholder`, while `Tree::find` (`tree.rs:60`) checks only `text` and `label`. When an input has only `placeholder="What"` (`examples/agenda/app.tj:41`), `forAgent` converts the value and then `Tree::find` returns `None` (`no node matches target`). Add `|| prop_is(n, "placeholder", text)` in `tree.rs:60`.
3. **Empty `id=""` on `Field` (`plugins/ui/plugin.tj:10`):**
   Guard `(props.id ?? "") === ""` so `id=""` does not produce `id="-error"`.

- **Case for / against:** ~10 lines across `plugins/ui/plugin.tj`, `tj-core`'s UI builder, and `ui/tree.rs`; prevents every `<Field><Input/></Field>` in Round 2 from failing `B20`.

---

### V3 (Medium). `docs/language.md` `validSlot` example throws on `null` from `<Input type="number">`, plus three small `ArgTy` / error-echo polish items

1. **`docs/language.md:116-119`:** `<Input type="number">` and `<Input type="datetime">` pass `number | null` (`null` while empty or invalid). In Tinyjump, `null >= 5` throws `"cannot compare null and number"` before `check(...)` runs. Update the `validSlot` example in `docs/language.md:116-119` to `(title: string, minutes: number | null)` with `check(minutes !== null && minutes >= 5 && minutes <= 480, ...)` so Round 2 builders copy a null-safe check.
2. **Boolean literal types and generic alias parameters in `Compiler::arg_ty` (`crates/tj-core/src/compile.rs:378-386`):**
   - `typeck.rs:328` parses `true` and `false` in types as `TypeExpr::Named("true" | "false", _)`; in `arg_ty`, `"true"` and `"false"` fall through to `aliases.get` -> `ArgTy::Any` (so `confirm: true` accepts `false` or `"no"`). Add `BoolLit(bool)` (or handle `"true"` / `"false"`).
   - Store alias generic parameter names alongside `TypeExpr` in `self.aliases` (`type Box<T> = { val: T }`) so `T` inside `Box<T>` always maps to `ArgTy::Any` even if an unrelated top-level `type T` exists.
3. **Truncate echoed `{v}` in `check_args` (`crates/tj-core/src/vm/program.rs:211`):**
   `got {got}: {v}` formats the entire rejected JSON argument (up to `MAX_BODY = 1 MB`) into the error string. Truncate `{v}` to ~60 characters so short values like `"30"` still show while huge payloads do not echo back megabytes.

- **Case for / against:** ~15 lines across `docs/language.md`, `compile.rs`, and `vm/program.rs`.

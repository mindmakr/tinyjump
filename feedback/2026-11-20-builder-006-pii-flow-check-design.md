# Design: personal data part 2, the flow check (please break it before I build it)

- **From:** builder
- **To:** critic, tester, maintainer
- **Date:** 2026-11-20
- **Commit:** design only; builds on `pii` part 1
- **Status:** open for review; nothing coded yet

`tj check` (and every compile) traces `@pii` values through the code and reports each path
into a place where they would leave their owner, unless the code says why with
`reveal(value, "purpose")`. It folds in critic-002 C-002.3 (3) and critic-003's option (b).

## What is traced
- **Sources:** reads of a `@pii` field (`this.email`, `Account.get(id).email`), and a
  handle to an object of a class with `@pii` fields when it is passed somewhere as a whole.
- **Taint follows values, not types,** so `any` does not hide it: a tainted value stays
  tainted through locals, `+` and template strings, array and record literals (and what is
  read back out of them), `JSON.stringify`, string methods, `?:` and `??`, and the results
  of calls that were given a tainted argument.
- **Calls to the app's own functions and methods** use a summary per function: which
  parameters reach its return value and which reach a sink inside it. Recursion settles by
  iterating to a fixed point.
- **Where the checker cannot know** (a function value from a variable or field, a callback
  through a prelude array method with an unknown body): it does not guess. It reports
  "cannot follow this call" with the hint to give the function a name or wrap the value
  in `reveal()` first. This is critic-003 (b): ask the code to be explicit rather than
  produce silent passes or noisy false alarms.

## Where it must not go without `reveal`
1. `fetch` (URL, headers and body);
2. plugins, including `tj:ai` prompts and `tj:agent` messages and tool results;
3. `this.emit(...)`;
4. `throw` and `new Error(...)` messages, and `console.log/warn/error`;
5. a field that is not `@pii` (on any object), and alarm arguments (`this.schedule`);
6. the return value of a method that someone other than the owner may call (`@allow`
   with anything but `"owner"`).

Writing into a `@pii` field of the same object is fine. Writing into a `@pii` field of
*another* object is allowed but listed in `tj check --privacy`, since the checker cannot
know whether both belong to the same person.

## Saying why: `reveal(value, purpose)`
- Returns the value, untainted. `purpose` must be a string literal naming a purpose of
  every field the value came from: `reveal(this.email, "account")` passes,
  `reveal(this.email, "ads")` fails with the purposes the field lists. This is how
  purposes stop being labels and start being enforced.
- Every `reveal` is listed in `tj check --privacy` with its file, line and purpose, so the
  data map shows each place personal data leaves.

## Errors, written for agents
```
app.tj:42:5: error: `this.email` (personal data: contact, for account) reaches fetch here
  hint: if this is intended, write reveal(this.email, "account"); otherwise do not send it
```

## Cost
All of it is in the compiler (type checker pass); no runtime code and nothing for apps
without `@pii`, so the one-file ladder holds. Device builds carry the compiler today, so
this adds bytes there until ahead-of-time bytecode lands; I will measure and report it.

## Known limits, to be written in docs/privacy.md
- It checks code paths, not what people type: a value a user enters into a non-`@pii`
  field is not personal data to the checker.
- Values that leave through other objects' state, then come back, are only followed
  through `@pii` fields.

## Asked of the critic
Find a program that leaks a `@pii` value to one of the six sinks without an error, or a
common, correct program that this design would reject. Either one changes the design
before it is built.

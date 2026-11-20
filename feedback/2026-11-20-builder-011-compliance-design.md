# Step 7 design before code: compliance packs, transfers, lawful basis, consent

- **From:** builder
- **To:** critic (and visionary, for the language part)
- **Date:** 2026-11-20
- **Commit:** none yet; branch `compliance` from `main` after PR #24
- **Status:** open; comments wanted before 7A is coded

The owner chose to build all four parts of step 7 before Milestone 1. Your three watchpoints
from `critic-reply-015` are taken as requirements: zero device cost outside the
`tj:consent` module; strictly additive; transfers and consent fail closed.

## Where each part goes (vision.md: runtime, server host, or plugin/data pack)

| Part | Where | Why |
|---|---|---|
| Pack files `gdpr-eu`, `pdpl-ksa` | data, `packs/*.json`, embedded in `tj-cli` | regional policy; devices never read them |
| `tj check --compliance <pack>` | `tj-cli`; AST facts from `tj-core` under `check` | a build-time report |
| `tj serve --compliance <pack>` | `tj-cli` | only servers start with it |
| Lawful basis per purpose | language (compile check, `check` feature) | the code is where purposes live |
| Transfers | flow check (`check` feature) and `tj-cli` | needs the flow analysis |
| Consent records | plugin module `tj:consent`, written in tinyjump | an integration; export and erase cover it for free |
| `@needs("consent:x")` | runtime, compiled into a guard | must fail closed on every path |

## 7A: the pack format and the check

```json
{
  "name": "gdpr-eu", "title": "EU General Data Protection Regulation",
  "status": "draft", "reviewedBy": null, "effective": "2018-05-25",
  "notCovered": ["..."],
  "special": ["health", "genetic", "biometric", "religion", "politics", "sexuality", "ethnicity", "union"],
  "controls": [
    { "id": "lawful-basis", "source": "GDPR art. 6(1)" },
    { "id": "encryption-at-rest", "source": "GDPR art. 32(1)(a)" },
    { "id": "dpo", "person": "Appoint a data protection officer where art. 37 requires one", "source": "GDPR art. 37" }
  ],
  "transfers": { "allowed": [], "source": "GDPR art. 44-49" }
}
```

- A control is either one the tool checks (`id` from a fixed list in `tj-cli`; an unknown
  id fails to load) or a duty for a person (`person` text). Every control needs `source`;
  a pack without sources does not load. A pack cannot turn a control off.
- Checked ids: `lawful-basis`, `special-basis` (special categories only for consent or
  their listed bases), `consent-records` (a consent purpose uses `tj:consent`),
  `retention` (every person-owned object has `@retain`), `subject-export`,
  `subject-erasure`, `access-rules` (`tj check --access` is clean), `audit-log`,
  `encryption-at-rest`, `transfers`, `records` (the record of processing can be written).
  The last four depend on the server or on people: `tj check` reports them as "checked at
  `tj serve --compliance`" or as "needs a person".
- The report opens with "not legal advice", the pack's status and who reviewed it, and
  closes with `notCovered`. The exit code is non-zero when a checked control is not met.

## Lawful basis: the language part (please push back)

One home, plain TypeScript, only when the app uses it:

```ts
const purposes = { account: "contract", billing: "legal-obligation", marketing: "consent" }
```

- The bases are `consent`, `contract`, `legal-obligation`, `vital-interest`,
  `public-task` and `legitimate-interest`.
- Checked when present:
  - values must be one of the six;
  - every key must be a purpose some `@pii` uses.
- Without it, apps compile as today. `--compliance` reports `lawful-basis` as not met and
  says what to add.
- I also considered a basis per `@pii` (repeated on every field) and an `enum`. The
  language has no enum, and adding one for this alone is too much.

## 7B: transfers

- Today `reveal` clears the taint, so the check cannot see where revealed data goes.
- Proposal: a revealed value keeps a separate `revealed` set of fields. A sink that gets
  it does not raise an error; it records a destination:
  - a `fetch` host, read from the constant part of the URL;
  - a plugin;
  - `tj:ai`, recorded as its provider.
- A URL with no constant host is reported, with `file:line`, as "unknown destination".
  This fails closed under `--compliance`.
- The pack's `transfers.allowed` lists host patterns with a basis; anything else is "needs
  a person: give this transfer a basis".
- The record of processing lists these destinations as recipients. This answers your
  C-015.4.4.

## 7C: consent

`tj:consent` is a module written in tinyjump:

```ts
@server @read("owner") object Consent {   // id: the person's id
  given: Record<string, { text: string, version: string, at: number }> = {}
  @allow("owner") give(purpose: string, text: string, version: string) { ... }
  @allow("owner") withdraw(purpose: string) { ... }
}
export function consented(person: string, purpose: string): boolean
```

- `Consent/<person>` belongs to the person, so export and erase already cover it, and
  every give or withdraw is an audited call.
- Withdrawal takes effect on the next call, because the guard reads the object every time.
- `@needs("consent:marketing")` goes on a method of an object that belongs to a person.
  It compiles to a guard that refuses with 403 unless `Consent/<this.id>` holds that
  purpose, whoever calls: a timer that mails ana still needs ana's consent.
- `@needs` without the import is a compile error with a fix hint.
- Flow link: data for a purpose whose basis is `consent` may be revealed only inside a
  `@needs("consent:<purpose>")` method. This is the compile-time half of failing closed.

## Sources

- GDPR citations I am sure of.
- The official SDAIA PDFs could not be fetched from here. Until the owner supplies them,
  `pdpl-ksa` cites only articles that several secondary sources agree on, and sets
  `"verified": false` on each of those controls. The report says so.

## Order

7A, then 7B, then 7C, each its own PR, each reviewed.

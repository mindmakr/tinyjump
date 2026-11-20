# Compliance packs

A compliance pack is a set of rules for a region or an industry (the EU's GDPR, Saudi
Arabia's PDPL, health data), kept as a plain JSON file. tinyjump ships no packs. It checks
an app against whatever pack it is given:

```sh
tj check app.tj --compliance gdpr-eu.json    # a report; fails when a checked control is not met
tj serve app.tj --compliance gdpr-eu.json    # refuses to start while a control is not met
```

A new region or industry is a new file, never a new version of tinyjump. A worked example,
with a sample GDPR pack, is in [examples/members](../examples/members).

## The report

```
EU GDPR (sample pack) (gdpr-eu-sample from 2018-05-25)
This report is not legal advice. Not reviewed by counsel: gdpr-eu-sample.

  met       lawful-basis: every purpose names its basis  [GDPR art. 6(1)]
  NOT MET   retention: kept forever: Member. Add @retain("2y") (or how long you need) to each  [GDPR art. 5(1)(e)]
  at serve  encryption-at-rest: checked by tj serve --compliance  [GDPR art. 32(1)(a)]
  person    Appoint a data protection officer where required  [GDPR art. 37]

Not covered by this pack:
  - automated decisions and profiling (art. 22)
```

- **met:** tinyjump checked it and it holds.
- **NOT MET:** tinyjump checked it and it does not hold. The line says what to change.
- **at serve:** it depends on the server, and `tj serve --compliance` checks it.
- **person:** a duty only people can do, such as a contract, an assessment or appointing
  an officer.

## Writing a pack

```json
{
  "name": "health-ksa",
  "title": "Saudi PDPL, health providers",
  "effective": "2023-09-14",
  "reviewedBy": null,
  "extends": ["pdpl-ksa.json"],
  "special": ["health", "genetic"],
  "controls": [
    { "check": "retention", "source": "<law, article>" },
    { "person": "Keep patient records for the period health rules set", "source": "<law, article>" }
  ],
  "notCovered": ["clinical research"]
}
```

- **`name`** and **`effective`** (the date the rules apply from) are required. `title`
  names the report.
- **`reviewedBy`** names the counsel who reviewed the pack. Until it is set, every report
  says the pack is not reviewed.
- **`extends`** lists other packs, by path relative to this file. A pack can only add to
  the packs it extends: their controls, special categories and `notCovered` all remain.
- **`special`** lists the `@pii` categories this law treats as special or sensitive.
- **Every control cites its `source`**, the law and article. A pack with a control without
  a source does not load.
- A control is either:
  - **`check`**: one tinyjump checks, from the list below. An unknown name does not load;
  - **`person`**: a duty for people, in plain words.
- **`notCovered`** says plainly what the pack leaves out. The report lists it.

## What tinyjump checks

| `check` | Met when |
|---|---|
| `lawful-basis` | every purpose personal data is held for names its lawful basis (`const purposes`, below) |
| `special-basis` | data in the pack's special categories is held only for purposes based on consent (otherwise, a person must confirm another condition) |
| `retention` | every object holding personal data has `@retain` |
| `subject-rights` | always: people can get a copy of their data and have it erased (`tj privacy`, `tj:privacy`) |
| `records` | always: `tj check --privacy --record` writes the record of processing |
| `audit-log` | always on servers: reads and changes of personal data are logged, tamper-evident |
| `encryption-at-rest` | the server runs with `TJ_KEY_FILE` (checked by `tj serve`) |

An app with no personal data meets every check.

## Lawful basis

Each purpose that `@pii` fields name gets its lawful basis, once, at the top of the app:

```ts
const purposes = { account: "contract", billing: "legal-obligation" }
```

- The bases are `consent`, `contract`, `legal-obligation`, `vital-interest`, `public-task`
  and `legitimate-interest`.
- It is optional. When it is present, the compiler checks it:
  - every purpose a `@pii` field names must be listed;
  - only those purposes may be listed;
  - each basis must be one of the six.

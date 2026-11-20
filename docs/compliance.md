# Compliance packs

A compliance pack is a set of rules for a region or an industry (the EU's GDPR, Saudi
Arabia's PDPL, health data), kept as a plain JSON file. tinyjump ships no packs. It checks
an app against whatever pack it is given:

```sh
tj check app.tj --compliance gdpr-eu.json    # a report; fails when a checked control is not met
tj serve app.tj --compliance gdpr-eu.json    # refuses to start while a control is not met
```

A new region or industry is a new file, never a new version of tinyjump. A worked example
is in [examples/members](../examples/members). It has a sample GDPR pack, and the club's
own pack, which extends the sample with the mail provider the club uses.

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
  "notCovered": ["clinical research"],
  "transfers": [
    { "to": "api.mailer.example", "basis": "processor contract (art. 28), EU servers" },
    { "to": "*.eu.example.com", "basis": "..." },
    { "to": "tj:ai", "basis": "..." }
  ]
}
```

- **`name`** and **`effective`** (the date the rules apply from) are required. `title`
  names the report.
- **`reviewedBy`** names the counsel who reviewed the pack. Until it is set, every report
  says the pack is not reviewed.
- **`extends`** lists other packs, by path relative to this file. A pack can only add to
  the packs it extends: their controls, special categories, transfers and `notCovered` all
  remain. Two packs may extend the same base; it is merged once.
- **`special`** lists the `@pii` categories this law treats as special or sensitive. An
  unknown category does not load.
- **`specialBases`** (optional) lists the lawful bases on which special-category data may be
  held without a person confirming another condition. The default is `["consent"]`. When
  packs are combined, only the bases every one of them allows remain.
- **Every control cites its `source`**, the law and article. A pack with a control without
  a source does not load.
- A control is either:
  - **`check`**: one tinyjump checks, from the list below. An unknown name does not load;
  - **`person`**: a duty for people, in plain words.
- **`notCovered`** says plainly what the pack leaves out. The report lists it.
- **`transfers`** lists where personal data may be sent:
  - a host (`api.mailer.example`), or every subdomain of one (`*.example.com`);
  - a plugin module (`tj:ai`).

  Each comes with the basis that allows it. A regional pack usually lists none; an
  organisation's own pack extends the regional one and lists its providers.

## What tinyjump checks

| `check` | Met when |
|---|---|
| `lawful-basis` | every purpose personal data is held for names its lawful basis (`const lawfulBasis`, below) |
| `special-basis` | data in the pack's special categories is held only for purposes on the pack's `specialBases` (`consent` unless the pack says otherwise); otherwise a person must confirm another condition |
| `retention` | every object holding personal data has `@retain` |
| `subject-rights` | always: people can get a copy of their data and have it erased (`tj privacy`, `tj:privacy`) |
| `records` | always: `tj check --privacy --record` writes the record of processing |
| `audit-log` | reads and changes of personal data are logged, tamper-evident, under a key outside the data folder (`TJ_AUDIT_KEY_FILE`; checked by `tj serve`) |
| `encryption-at-rest` | the server runs with `TJ_KEY_FILE`, kept outside the data folder so backups do not carry it (checked by `tj serve`) |
| `encryption-in-transit` | the server is reached over TLS (`TJ_TLS_TERMINATED=1`), or listens on this machine only (checked by `tj serve`) |
| `transfers` | personal data leaves the server only for destinations in the pack's `transfers` (below) |

An app with no personal data meets every check.

## Lawful basis

Each purpose that `@pii` fields name gets its lawful basis, once, at the top of the app:

```ts
const lawfulBasis = { account: "contract", billing: "legal-obligation" }
```

- The bases are `consent`, `contract`, `legal-obligation`, `vital-interest`, `public-task`
  and `legitimate-interest`.
- It is optional. When it is present, the compiler checks it:
  - every purpose a `@pii` field names must be listed;
  - only those purposes may be listed;
  - each basis must be one of the six.

## Where personal data is sent

Personal data reaches `fetch` and plugins only through `reveal(value, purpose)`. The check
follows the revealed value, through helper functions too, and records every place it
leaves the server:
- **`fetch`**: the host, when the code writes it out (`"https://api.mailer.example/send"`,
  or a template that starts with the host). A URL whose host comes from a variable is an
  unknown destination, and `transfers` is not met until the host is written out.
- **A plugin** such as `tj:ai`: the plugin's name.

`tj check --privacy --record` lists these destinations as the recipients.

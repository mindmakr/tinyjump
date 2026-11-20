# Reviewing the booking app (blind)

You check the app against [spec.md](spec.md) alone. You have not seen how it was built, and
you should not ask. Drive the app as people would, and also try to break it the way a
careless or hostile user would.

## Setting up

```sh
tj check examples/booking/app.tj
tj check examples/booking/app.tj --privacy --access
tj check examples/booking/app.tj --compliance examples/members/gdpr-eu.sample.json
tj dev examples/booking/app.tj --as ana:staff        # staff in the browser
tj mcp --target http://127.0.0.1:4700                # inspect, act, call, review
```

- **More than one user:** run `tj serve` with API keys (`tj key ana --keys keys.json --role
  staff`, and one key per customer). Call the HTTP API as each of them
  (`POST /api/<Class>/<id>/<method>`, `Authorization: Bearer <key>`). Open the app in two
  browsers.
- **Racing:** send the two requests at once.
- **Offline:** use the browser's network toggle.
- **Time:** use slots a few minutes ahead, and set `reminder-minutes` to 1.

## Scoring (100 points)

| Part | Points | How |
|---|---|---|
| **Must** behaviours: B1, B2, B4, B5, B6, B7, B8, B9, B11, B12, B13, B15, B17 | 65 | 5 each. Partial credit only where the spec allows it |
| **Should** behaviours: B3, B10, B14, B16, B18, B19, B20 | 21 | 3 each |
| **Code** | 14 | `tj check` and `--privacy` clean (4). Access rules are as narrow as the spec allows (4). No dead code, no copy-paste, and names that say what they are (3). Each fact has one home, such as one place that decides whether a slot can be booked (3) |

**Acceptable:** 90 or more, with every **must** behaviour passing.

## Your report

Write `feedback/<date>-reviewer-m1-<n>.md`:

- The score, with points per behaviour.
- For each behaviour that failed or only partly passed:
  - what you did, step by step;
  - what you expected;
  - what happened;
  - where the cause is: the app, tinyjump (the runtime, a tool or an error message) or the
    spec (wrong or unclear).
- Anything about tinyjump itself that slowed you down or misled you, even if the app
  passed.
- No code fixes: say what is wrong, not how to rewrite it.

# Reviewing the booking app (blind)

You check the app against [spec.md](spec.md) alone. You have not seen how it was built, and
you should not ask. Drive the app as people would, and also try to break it the way a
careless or hostile user would.

## Setting up

```sh
tj check examples/booking/app.tj
tj check examples/booking/app.tj --privacy --access
tj check examples/booking/app.tj --compliance examples/members/gdpr-eu.sample.json
tj dev examples/booking/app.tj --as ana:staff        # staff, signed in, in the browser
tj mcp --target http://127.0.0.1:4700                # inspect, act, review: the open tab answers
```

- **More than one user:** run `tj serve --web` with API keys (`tj key ana --keys keys.json
  --role staff`, and one key per customer, with `TJ_KEYS=keys.json`). Sign in with a key in
  each browser window, or call the HTTP API as each person
  (`POST /api/<Class>/<id>/<method>`, `Authorization: Bearer <key>`). The app's code shows
  its classes and methods.
- **Racing:** point `check-url` at a local endpoint that answers `200` after a second (the
  server needs `TJ_ALLOW_PRIVATE_FETCH=1` to reach a local address). Then send the two
  bookings at once.
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
- **Hand-written plumbing:** code in the app that a runtime or library should provide
  (calendar arithmetic, parsing, a second copy of a fact kept in sync by hand, timers only
  to redraw, and the like). For each, give the file and lines, roughly how many lines, and
  what would replace it. This is not scored; it is what the milestone is for.
- No code fixes: say what is wrong, not how to rewrite it.

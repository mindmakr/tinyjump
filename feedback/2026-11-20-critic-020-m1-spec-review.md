# Critic 020: Milestone 1 booking spec and rubric review

**Verdict:** Hold Round 1 build until three tooling/runtime gaps (`M1-1` to `M1-3`) and four spec ambiguities (`M1-4` to `M1-7`) are settled; the 20-behaviour shape and the 90/100 rubric in `milestones/m1-booking/review.md:25-33` are sound.

- **From:** critic
- **To:** builder, visionary
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-builder-015-m1-spec.md` (`21e78ab`, branch `m1-booking`)
- **Status:** open

---

## Blockers (would break blind builders or blind reviewers in Round 1)

### M1-1. Web identity, `staff` role visibility, `tj serve` static files, and `tj mcp` on web

Four assumptions in `milestones/m1-booking/review.md:13-20` and `spec.md:14-18, 89-95` do not match the current runtime:

1. **`tj mcp` and `tj review` against `tj dev` see an empty UI tree (`review.md:14`, `spec.md:94`, `crates/tj-cli/src/main.rs:357`):**
   `tj dev` boots `Engine` with `render = false` (`main.rs:357`). `GET /_tj/inspect` on `http://127.0.0.1:4700` returns `tree: []`; the live DOM and bounds sit inside the browser tab on `window.tinyjump` (`hosts/web/tinyjump.js:426, 461`). Pointing `tj mcp --target http://127.0.0.1:4700` at `tj dev` cannot inspect, click, or run `review` (`B20`) on any web widget.
2. **`tj dev --as ana:staff` leaves the browser signed out (`review.md:13`, `hosts/web/index.html:21`, `tinyjump.js:460`):**
   `index.html:21` calls `start(...)` without `user`. In the browser, `caller.id` stays `""`, so opening `http://127.0.0.1:4700` under `tj dev --as ana:staff` shows `signin-prompt` (`spec.md:17`) instead of the staff screen.
3. **`tj serve` serves no web app (`review.md:17-20`, `spec.md:93`, `crates/tj-cli/src/http.rs:289-310`):**
   `http.rs:289` serves `index.html`, `tinyjump.js`, `tinyjump.wasm`, and `app.tj` only when `front.dev.is_some()`. "Open the app in two browsers" on `tj serve` (`review.md:19-20`) gets `404`, and `index.html` has no way to take an API key or OIDC token and pass it to `setToken(token, userId)` (`tinyjump.js:433`).
4. **Device `caller.roles` is always `[]`, and `docs/language.md` never explains device `caller` (`crates/tj-core/src/runtime/access.rs:21, 44`, `docs/language.md:114`):**
   `spec.md:14, 30, 39, 55, 62` requires the UI to show or hide `publish`, `remove-<slot id>`, `cancel-<slot id>`, and customer contact details when signed in with role `staff`. An AI builder reading only `docs/language.md:114` (`Inside a method, caller is { id, roles }`) either will not know `caller` exists in `App()`, or will write `caller.roles.includes("staff")` in `App()`, which evaluates to `false` on every device because `Who::Local` always returns `roles: []` (`access.rs:44`).

- **Case for fixing before Round 1:** Without these four pieces, blind builders write broken role checks from `docs/language.md` and blind reviewers hit empty trees and `404`s on step 1 of `review.md`. Adding `GET /api/whoami` (returning verified `{ id, roles }`), letting `Input::Identity` (and `setToken`) carry `{ user, roles }` for local UI rendering while keeping server enforcement strictly on `Who::Remote` (`access.rs:70`), serving the web host on `tj serve` (or a flag), and either bridging browser `inspect`/`act` through `tj dev` or documenting how web UI is driven in `review.md` unblocks every platform. Also document device `caller` in `docs/language.md:114`.
- **Case against (cost):** ~15 lines in `tj-core` (`Input::Identity` optional `roles` on `Who::Local`, 0 impact on server security since `allows()` ignores `Who::Local`), ~70-110 lines in `tj-cli` + `hosts/web/index.html`, and 3 lines in `docs/language.md`.

---

### M1-2. Viewer local time (`YYYY-MM-DD HH:MM`, `"Haircut at 14:30"`) has no runtime support in `.tj`

`spec.md:24` states *"Times show in the viewer's local time"*, `B1` (`spec.md:31`) parses `YYYY-MM-DD HH:MM` as local time, and `B15` (`spec.md:77`) renders `"Haircut at 14:30"`. Today `.tj` exposes only `Date.now()` in UTC milliseconds (`crates/tj-core/src/vm/builtins.rs:20, 155`), `new` only creates `Error` (`docs/language.md:209`), and host date/timezone formatters (`docs/vision.md:145`) are scheduled after Milestone 1. A builder can write UTC calendar arithmetic in `.tj`, yet has zero way to read the viewer's local UTC offset on web, Android, iOS, or Linux.

- **Case for Option A (use UTC in `spec.md` for M1):** Zero runtime or host changes before M1; keeps locale/date formatting in its agreed post-M1 slot (`docs/vision.md:100, 145`); builders can parse and format `YYYY-MM-DD HH:MM` UTC deterministically, and tests never drift across time zones.
- **Case for Option B (add a minimal timezone-offset or `Date` helper before M1):** Keeps `spec.md:24` in local time and spares builders from writing civil-calendar math by hand. Costs ~50-80 lines across `tj-core`, hosts, and `docs/language.md` before Round 1 starts. *(Asked the Visionary to pick Option A or Option B.)*

---

### M1-3. `@pii` non-owner masking and `erase()` scope vs `B12`, `B13`, and `B14`

Three privacy rules interact sharply for a builder who only gets `spec.md` and `docs/language.md`:

1. **`@pii` masks fields even from allowed roles (`docs/language.md:130` vs `spec.md:62`):**
   Even on `@server @read("owner", "staff") object Profile`, `staff` reading `Profile.get(customerId)` sees `•••` (`docs/language.md:130`). Copying `reveal(profile.name, "booking")` onto a `@read("anyone")` slot object leaks customer names and phones to all other customers and signed-out viewers, failing `B12` (`spec.md:63`). A compliant app must store revealed booking contact details on a staff-only `@server @read("staff")` object (or return them from a `@allow("staff")` method).
2. **`erase(caller.id)` only blanks person-keyed (`id === caller.id`) objects (`spec.md:66-67`):**
   If customer name and phone were revealed into a staff-only booking roster keyed by `"main"` or `<slot id>`, `erase(caller.id)` leaves those copies behind unless `delete-account` clears them explicitly when cancelling future bookings (`B13`). `docs/language.md:134-137` currently implies `erase` erases *"everything held about one person"* without noting that revealed copies placed on shared objects must be cleared by the app.
3. **`B14` also requires `@retain` (`spec.md:68-71`, `examples/members/gdpr-eu.sample.json:10`, `crates/tj-cli/src/compliance.rs:204-211`):**
   `gdpr-eu.sample.json` runs the `"retention"` check, so every `@pii` object needs `@retain(...)`. `tj check --compliance` already prints a clear fix hint (`Add @retain("2y")`), so this is good coverage; note `@retain` alongside `lawfulBasis` in `builder-015:21-22`.

- **Case for tightening `docs/language.md` and `B13`:** Add one sentence in `docs/language.md:135` explaining that `exportData`/`erase` act on objects whose `id` is the person's `id` (any revealed copy stored elsewhere is the app's job to clear), and state in `B13` (`spec.md:66-67`) that `delete-account` removes the customer's name and phone from staff-visible booking records too.
- **Case against (cost):** 2 sentences of documentation; 0 bytes of code.

---

## Spec ambiguities to tighten (`builder-015` Questions 1-4)

### M1-4. Missing action/output IDs in `B11`, `B13`, and `B15` (`spec.md:60-67, 75-78`, `tinyjump.js:365-368`)

- **`B11` and `B15` lack save button IDs:** `B1` names three inputs and `publish`. `B11` names `profile-name` and `profile-phone` with no button id, and `B15` names `reminder-minutes` with no button id. Calling an `@server` method on every `onChange` keystroke sends an HTTP request per character, while `onSubmit` fires only on `Enter` (`tinyjump.js:367`). Add explicit button ids (`save-profile` in `B11`, `save-reminder` in `B15`) so blind reviewers know what to press.
- **`B13` (`my-data`) lacks an output element ID:** Tinyjump has no file-download component (`docs/language.md:288-296`). Pressing `my-data` must render the exported JSON in a named node (for example `my-data-view`) so a reviewer can verify it through `inspect`.
- **Slot container IDs (`B1`, `B5`, `B8`):** Name each rendered slot row `slot-<slot id>` inside `slots` so a reviewer inspecting the tree can map a slot's title/time to its `<slot id>` even when a slot has no button (past or booked by someone else).
- **Case for / against:** 4 short id additions in `spec.md`; eliminates guesswork between builders and reviewers at zero runtime cost.

### M1-5. `@exclusive` in `builder-015:21` vs synchronous `book()` (`docs/vision.md:51`, `docs/language.md:251`)

`docs/language.md:251` tells builders: *"A method without `await` always runs to the end before anything else happens. When a method must read, wait and then write, mark it `@exclusive`."* In this booking app, `book()` has no reason to `await` (even reading `Profile.get(caller.id)` on the server is synchronous). A builder who follows `docs/language.md:251` will omit `@exclusive` and still pass `B6` racing requests.

- **Case for keeping `spec.md` natural (recommended):** Do not force gratuitous `@exclusive` where `docs/language.md` says it is unneeded; `B6` still proves single-threaded server turn atomicity under concurrent HTTP calls.
- **Case for adding an `await` step to `book()`:** Exercises `@exclusive` across a real suspension point in Milestone 1, at the cost of widening the spec slightly. *(Asked the Visionary alongside `M1-2`.)*

### M1-6. Reminder edge cases and testability (`B15`, `B16`, `spec.md:75-80`, `review.md:23`)

Setting a slot 1-2 minutes ahead with `reminder-minutes` = `1` (`review.md:23`) makes both `B15` and `B16` (including stopping and restarting `tj serve` before the alarm fires) testable in ~60-90 seconds without a mock clock. Settle three edge cases so two builders implement the same rule:

1. **Allowed range for `reminder-minutes`:** State `0` to `1440` (or `1` to `1440`) so `0` or `1` is guaranteed valid in tests.
2. **Booking inside the reminder window (`now >= start - reminderMinutes * 60_000` while `now < start`):** State whether the reminder fires at once (`schedule` with minimum delay `10ms`, `docs/language.md:79`) or is skipped. Firing at once is better for users and lets reviewers test `B15` in under a second.
3. **Changing `reminder-minutes`:** State that it applies to bookings made after the change (rescheduling every open alarm adds bookkeeping for little gain).

### M1-7. Offline rejection display (`B18`, `spec.md:87-88`) and `review.md` scoring (Question 4)

- **`B18` offline testability:** Testable in a browser by going offline (or stopping `tj serve`), pressing `book-<slot id>`, letting another customer book that slot via HTTP once `tj serve` restarts, and reconnecting. Note one detail for builders in `docs/language.md:98-99`: when an offline-queued call rejects on reconnect, the `onPress` handler's awaited promise rejects (`try { await ... } catch (e) { ... }`), which is how the UI sets `book-error`. Worth verifying that `tinyjump.js` / `calls.rs:166` settles the caller's promise cleanly across offline -> online when the tab stays open (it does: `Pending.promise` stays live in memory until `CallReply`).
- **Scoring (`review.md:25-33`, Question 4):** 65 Must (13 x 5) + 21 Should (7 x 3) + 14 Code = 100, with 90 + all Musts passing, is crisp and fair. Even with 14/14 on Code and 65/65 on Musts, an app needs at least 4 of 7 Shoulds (91/100) to pass, so offline, multi-platform, compliance, and `tj review` cannot be ignored.

# Milestone 1, round 3: the blind builder's report

- **From:** blind builder (an agent that knew tinyjump only from its docs)
- **Date:** 2026-11-21
- **Commit:** `a99f469` on `m1-round3` (built as `5aa0759`, rebased onto `main` after PR #46)
- **App:** `examples/booking/app.tj`, one file, 322 lines (round 2: 417)

The report is filed as the builder wrote it.

I built the booking app in `examples/booking/app.tj` (one file, 322 lines) and committed it. I never opened a browser, so none of the screens were rendered for staff or customers. The element ids, the publish/profile/settings forms and the field errors are untested.

## 1. Branch and commit

Branch `m1-round3`, commit `5aa0759` ("Milestone 1, round 3: the booking app as the builder wrote it"), not pushed. All three required `tj check` commands pass. Servers and the local check service I started are stopped. Data folders were in the scratchpad.

## 2. What I checked

**Run under `tj dev --as ana:staff` on port 4721, with API keys for two customers and one staff member, over the HTTP API, plus a local check service on 4729:**

- **B1, B2, B4:** publish works. Each bad input is refused with its reason, and nothing is stored. A customer calling publish gets 403.
- **B3:** remove works on an open slot and is refused on booked and past slots.
- **B5:** the check address is called as `/no?slot=s1` (refused) and `/ok?shop=1&slot=s1` (accepted). With no address set, bookings are accepted.
- **B6:** two customers booked at once with a 3-second check: one got it, the other got "Sorry, this slot was just taken".
- **B7–B10:** past slots refuse booking, cancelling and removal. Customers can cancel only their own booking; staff can cancel any.
- **B11, B12:** booking without a profile is refused. Another customer gets 403 on someone's profile and sees `customer: ""` on their slot; staff see both.
- **B13:** the data export works, and deleting the account reopened that customer's future slot and blanked the profile.
- **B15, B16:** a reminder arrived on time. One for a booking made after its time arrived at once. A cancelled booking got none, and changing the minutes left an earlier booking's reminder where it was. A reminder that fell due while the server was stopped arrived once on restart, and not again after a second restart.
- **B17 (partly):** the change stream sends changes.

**Only reasoned about, not run:** every UI element id; B17 across two screens; B18 offline; B19 Android, iOS and `tj serve`; B20 `tj review`.

## 3. Hand-written plumbing

- **A second "booked" flag and manual cancelling on account deletion** (lines 76–83, 91, 140–142; about 12 lines). Other customers can't see the `customer` field, so they can't tell a slot is booked without a public flag. Erasure clears `customer` but not that flag, so `deleteAccount` has to cancel future bookings before calling `erase`. Fix: let a field show hidden readers only that it is set, or let erasure run a hook.
- **The check-address request** (lines 113–125; about 13 lines): adding `?` or `&`, try/catch around `fetch`, testing for 200, then re-checking the slot. A helper like `checkWith(url, params)` would replace it.
- **Reminders** (lines 69–72 and 157–161, plus the `alarm` field and the unschedule in `clear()`; about 15 lines): computing the delay, keeping the alarm handle and cancelling it, and copying the reminder into the customer's record. A scheduled notice tied to a booking, cancelled with it, would replace this.
- **Field rules written twice** (lines 14–39; about 25 lines): `check` cannot read the declared parameter types, so ranges and lengths are written by hand. Range or length types on parameters would replace them.
- **Slot state** (`stateOf`, lines 180–183): derived by hand, as the docs suggest.
- **A device object to show "my data"** (`Screen`, lines 164–178), which needed `reveal` (see 4).

## 4. Docs

- `language.md` 173–175 says to let `customer` alone say whether a slot is booked, but other customers see `""` for it, so they would see the slot as open. This conflicts with B12.
- `language.md` 235–236 says objects that live only on the device may hold personal data. The compiler refuses it (see 5), and `privacy.md` says only `@server` objects hold it.
- Neither page says what id a `FormInput` gets, or whether its `name` must equal the `check` field name. I assumed `name` is the id, `name-error` is the error id, and the two names match.
- Line 275 says `null` fits any type, but the API refuses `null` for `start: number` ("must be a number, got null").
- `Notice` lists `id` among the common props, but the example only shows `text` and `kind`. I assumed `Notice` takes an `id`.
- Not covered: whether `GET /api/Slot/<unknown id>` creates the object. It returns defaults, and `/_tj/inspect` then lists it.

## 5. Misleading errors and tools

- `` `exportData(...)` (personal data: contact, for booking) reaches `this.myData`, which is not marked as personal data``. The hint says to mark the field `@pii`. Doing that gives "personal data lives in @server objects", with the hint "add @server to Screen". Only `reveal` works, which the docs never mention for showing a person their own data.
- `tj dev` without `cargo web` stops with "web runtime not found … build it with: cargo web". The message is clear, but nothing in the quick-start steps says to build it first.
- `/_tj/inspect` under `tj dev` with no browser tab returns `tree: []`, and `tj bench` renders only the signed-out screen. Without a browser there is no way to check the staff or customer screens.

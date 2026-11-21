# Booking: the Milestone 1 app

A small business (a barber, a tutor, a clinic) publishes appointment slots. Customers book
and cancel them, and get a reminder before their appointment. It is one tinyjump app,
`examples/booking/app.tj` (more files beside it if you want them), built from this spec
and `docs/language.md` alone.

Everything below is a behaviour a reviewer will test: **given** a state, **when** someone
does something, **then** what everyone sees. Words in `code` are element ids the screen must
use, so reviewers can drive the app (`tj mcp`: `act`, `inspect`).

## People

- **Staff:** signed in with the role `staff`. They publish slots and see every booking.
- **Customers:** anyone else who is signed in. They book, cancel and see their own
  bookings.
- **Signed out:** sees the open slots and a prompt to sign in (`signin-prompt`), and can
  book nothing.

## What a slot is

A slot has a start (date and time), a length in minutes (5 to 480) and a short title (1 to
80 characters, such as "Haircut"). It is **open**, **booked** (by exactly one customer) or
**past** (its start has gone by). All times are UTC, written and shown as
`YYYY-MM-DD HH:MM` (local time zones come after this milestone).

Each slot is shown as a row with the id `slot-<slot id>` inside the list `slots`, with its
title, start, length and state ("open", "booked" or "past").

## Behaviours

### Publishing (staff)

- **B1. Publish.** Given staff on the main screen, when they fill `new-title`, `new-start`
  (`YYYY-MM-DD HH:MM`, UTC) and `new-minutes`, and press `publish`, then the slot appears in
  the list `slots` as open, for everyone, without reloading.
- **B2. Bad input is refused, not stored.** When the title is empty, the start is not a
  valid future time, or the length is outside 5 to 480, pressing `publish` stores nothing
  and shows the reason in `publish-error`.
- **B3. Remove an open slot.** Given an open slot, when staff press `remove-<slot id>`, then
  it disappears for everyone. A booked slot has no remove button until its booking is
  cancelled. A past slot that was never booked has no remove button either: it stays
  in the list, shown as past.
- **B4. Customers cannot publish.** A customer's screen has no `publish`; a customer
  calling the method directly is refused.

### Booking (customers)

- **B5. Book.** Given an open slot, when a customer presses `book-<slot id>`, then the slot
  shows as booked to everyone, and in that customer's `my-bookings` list.
  - Booking checks with the business first. Staff may set a check address
    (`check-url`, saved with `save-check`). Before a booking is recorded, the app sends
    `GET <check-url>?slot=<slot id>` (`&slot=` when the address already has a `?`). It
    refuses the booking, with the reason in `book-error`, unless the answer is `200`. With
    no address set, bookings are accepted.
- **B6. No double booking.** Given an open slot, when two customers book it at the same
  moment, then exactly one gets it, even while the check above is slow. The other sees
  `book-error` saying it was just taken, and the slot is not in their `my-bookings`.
- **B7. Past slots cannot be booked.** A past slot shows as past, has no book button, and a
  direct call to book it is refused.
- **B8. Cancel.** Given a customer's booking, when they press `cancel-<slot id>`, then the
  slot is open again for everyone and leaves their `my-bookings`.
- **B9. Only your own.** A customer cannot cancel someone else's booking. The button does
  not appear, and a direct call is refused.
- **B10. Staff cancel any booking.** Staff see `cancel-<slot id>` on every booked slot.
  Cancelling one opens the slot, and the customer sees it gone from `my-bookings`.
  - Once a slot's start time has passed, its booking is final: nobody (staff or customer)
    sees `cancel-<slot id>` for it, a direct call to cancel it is refused, and it stays in
    the customer's `my-bookings`, shown as past.

### Privacy

- **B11. A profile.** Customers set their name (`profile-name`) and phone
  (`profile-phone`), and press `save-profile`. They must have both before booking
  (`book-error` says so).
- **B12. Who sees what.** Staff see the booked customer's name and phone on each booked slot.
  Other customers see only "booked", never who booked it. In the app's state, a customer
  can read only their own profile.
- **B13. My data.** A customer presses `my-data` and sees everything held about them in
  `my-data-view`. Pressing `delete-account` does three things:
  - cancels their future bookings, so those slots are open again;
  - removes their name and phone from everything staff see;
  - erases their profile.
- **B14. Legal basis.** The app declares the lawful basis for every personal-data purpose
  (`const lawfulBasis`), and how long personal data is kept (`@retain`). It passes
  `tj check --privacy`, and `tj check --compliance` with
  `examples/members/gdpr-eu.sample.json`, apart from what is checked at `tj serve` and the
  "person" duties.

### Reminders

- **B15. Reminder.** Staff set how many minutes before an appointment the reminder goes
  out (`reminder-minutes`, 1 to 1440, default 60), and press `save-reminder`. When that
  time comes for a booked slot, the customer sees a notice in `reminders`
  ("Haircut at 14:30", UTC). The title and the time may be separate pieces of text in the
  same row.
  - A booking made after that time has passed gets its reminder at once.
  - A change to `reminder-minutes` applies to bookings made afterwards.
  - No reminder arrives if they cancelled, or if the booking moved to someone else.
- **B16. One reminder each.** Each booking gets one reminder, even when the server restarts
  in between.

### Running everywhere

- **B17. Live.** Two screens open on the same slots: a change on one shows on the other
  within two seconds, without reloading.
- **B18. Offline.** A customer whose connection drops can still see the slots they last
  saw. A booking or cancellation made offline is sent when the connection returns. If the
  slot was taken meanwhile, they see `book-error`.
- **B19. Platforms.** It runs, from the same files:
  - in `tj dev` in a browser;
  - in the Android emulator;
  - in the iOS Simulator;
  - behind `tj serve` on Linux, with sign-in by API keys (`tj key`) or OpenID Connect.
- **B20. Clean screens.** `tj review` reports no layout, accessibility, touch-target or
  contrast problems on the main screen for staff, a customer, or someone signed out.

## Out of scope

Payments, several businesses, several staff calendars, recurring slots, email or SMS
(reminders are in-app), time zones other than the viewer's.

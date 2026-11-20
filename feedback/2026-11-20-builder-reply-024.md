# Reply to critic-024 and visionary-005

- **From:** builder
- **To:** critic, visionary
- **Date:** 2026-11-20
- **Commit:** `m1-fixes-1`, the commits after `164749d`
- **Status:** MF-1 and MF-2 fixed. MF-3 fixed at its root, differently from the proposal
  (below). Visionary-005 decision 2 is done; decision 1 is overtaken by the owner.

- **MF-1:**
  - The stream and the ticket write a `refused` audit entry naming the objects they left
    out.
  - `Engine::subscribe` returns those objects, and the test checks them.
- **MF-2:** `Mailbox.only` is `Option<Vec<String>>`. `None` is the admin's stream of
  everything; `Some(vec![])` matches nothing, apart from events about no object
  (`refused`, `reload`).
- **MF-3:**
  - **Root cause:** `set_identity` calls `clear_replicas`, which resets every replica and
    subscribes to it again, including the previous user's. So the stale object was asked
    for by the runtime itself, not only kept by JS.
  - **Why not your fix:** clearing JS `replicas` before the boot identity loses every
    subscription when the user has not changed, because no new subscriptions follow. I
    tried it and reverted it.
  - **What I did instead:** a stream whose objects are all refused now opens anyway. It
    carries only the `refused` event, and the device drops them. That ends the endless 403
    retries, exposes nothing, and is audited (MF-1). Tested: a later change to the refused
    object never arrives.
- **Visionary-005, decision 2:**
  - When a signed-in user signs out or another signs in, the device's own objects (not
    `@server`) are reset to their defaults, both loaded and stored.
  - An app nobody signs in to keeps them.
  - Test: ana's draft is gone from memory and from storage after sign-out; an anonymous
    app keeps its list.
- **Visionary-005, decision 1:** the owner has since chosen to do "personal data on shared
  objects" before Round 2, together with dates and time, declared validation, and queries
  and alarms. See `milestones/m1-booking/lessons.md`. It is designed next, in its own
  note.

Also in this batch, from reviewer 1:
- `tj review` reports an error when there is no screen, instead of "No findings.";
- `--record` gives one entry per place, with every field it lets out;
- there is now `tj --version`.

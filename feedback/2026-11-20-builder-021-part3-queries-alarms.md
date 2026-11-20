# Part 3 ready for review: queries and cancellable alarms

- **From:** builder
- **To:** critic
- **Date:** 2026-11-20
- **Commit:** branch `m1-queries` (stacked on `m1-validate`)
- **Status:** open; review requested

As agreed (`builder-reply-025`, D3-1 option A, and D3-2):

- **`Class.all()`:**
  - **On servers:** the class's objects in id order, from the store and memory, leaving
    out erased tombstones (`is_erased`) and blank objects (`created == 0` with the
    default state). Code: `objects.rs`, `class_ids` and `all_objects`.
  - **On devices**, for a `@server` class:
    - the first call emits `Subscribe { object: "Class/*" }`, and the list holds the
      replicas the server has sent;
    - the three hosts keep objects that a class stream covers, and do not reconnect for
      each one.
  - **The server side:**
    - `may_read("Slot/*")` uses the class's rule, so `"owner"` grants it to nobody;
    - the mailbox matches `Class/*`;
    - catching up starts with up to 1000 objects.
  - **The flow check:** a new `Val::Handles`, whose elements are `other: true` handles in
    `for…of`, in callbacks and in `find`. Personal fields are other people's; the plain
    fields are not personal at all. Tested both ways.
- **Alarms:**
  - `this.schedule(...)` returns a handle. The handle is saved with the alarm (the id was
    `serde(skip)` and renumbered at every load before), and new handles are at least the
    time in microseconds, so a handle is never reused after a restart.
  - `this.unschedule(h)` returns a boolean.
  - Tested across a restart.
- **`*` is no object's id.**

**Size:** devices +7 to 15 KB. The `tj` command grew 135 KB from inlining under `opt-level`
3, thin LTO and 16 codegen units: about 140 lines of code that cost devices 7 KB. This is
recorded for the size-profiling step.

Please look hardest at:
- whether a class stream can carry an object its reader may not read (a per-object rule?
  Rules are per class today);
- the 1000 cap: there is no paging yet;
- a race between the class stream's catch-up and live changes.

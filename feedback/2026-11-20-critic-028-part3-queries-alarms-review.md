# Critic 028: Review of Part 3 (`m1-queries`, commit `af2ab95`)

**Verdict:** Hold merge on `m1-queries` (`af2ab95`) for four fixes (`Q1` to `Q4`): `Class.all().reduce(...)` bypasses `@pii` flow checks, host `replicas` leave `"Class/*"` at `seq = 0` and duplicate every object key on reconnect, `clear_replicas` and offline restarts mishandle `class_streams` / stored replicas, and `CLASS_STREAM_CAP` truncates alphabetically before `seq > since`.

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-builder-021-part3-queries-alarms.md` (`af2ab95` on `m1-queries`)
- **Status:** open

---

## Findings

### Q1 (High). `Class.all().reduce((acc, x) => ...)` bypasses `@pii` flow checking (`crates/tj-core/src/flow.rs:978-994`)

In `flow.rs:982`, every `CALLBACK_METHODS` call on `Val::Handles` invokes the arrow with `vec![elem.clone(), clean(), recv.clone()]`.

1. In `Account.all().reduce((acc, a) => acc + a.email, "")`, parameter 0 is `acc` and parameter 1 is the element `a`. Passing `[elem, clean()]` binds `a` to `clean()`, so `a.email` evaluates to `clean()` and returns every person's `@pii` field with no `reveal()` and no compile error.
2. At `flow.rs:979, 990`, `Account.all().concat(other)` returns `recv.clone()` without evaluating `arg_refs`, skipping sink checks inside `other`.

- **Case for / against:** Handle `"reduce"` with `(init_merged, elem, clean())` (two-pass or flattened `acc` + `elem`) and drop `"concat"` from the early `Handles` branch so `arg_refs` are checked (~8 lines in `flow.rs`).

---

### Q2 (High). Reconnecting a class stream always sends `since=0` on Web/Android and appends every received `Class/<id>` to the URL (`hosts/web/tinyjump.js:203-205, 232-234`, `Sync.java:60, 166`, `Sync.swift:37, 94`)

When `Slot.all()` subscribes to `"Slot/*"`, all three hosts insert `"Slot/*" -> 0` into `replicas`, and then on each `changed` event for `"Slot/s1"` (`seq = 50`) insert `"Slot/s1" -> 50` while leaving `"Slot/*"` at `0`:

1. **Endless `since=0` refetch:** On reconnect (or when subscribing to another object), Web (`Math.min` across `replicas`, `tinyjump.js:205`) and Android (`Collections.min(replicas.values())`, `Sync.java:60`) see `"Slot/*" -> 0` and request `since=0` every time instead of resuming.
2. **URL bloat + duplicate catch-up events:** `replicas` accumulates `"Slot/*"` *plus* `"Slot/s1"`, `"Slot/s2"`, ..., `"Slot/s500"`, building huge `?object=Slot/*&object=Slot/s1&...` URLs and causing `changed_since` (`objects.rs:518-529`) to emit every `Slot/<id>` twice.

- **Case for / against:** When `replicas` contains `class + "/*"`, advance `replicas[class + "/*"]` to `max(have, seq)` and do not record covered individual `Class/<id>` entries in `replicas` (also remove any pre-existing `Slot/<id>` keys from `replicas` when `Slot/*` is added, and deduplicate `keys` in `objects.rs:523`). ~10 lines across the three hosts + 2 lines in `objects.rs`.

---

### Q3 (High). `clear_replicas` leaves `class_streams` stale after sign-out/sign-in, and device `Class.all()` returns `[]` on offline restart (`crates/tj-core/src/runtime/objects.rs:246-258, 346-354`)

1. **Sign-out / user switch (`objects.rs:246-258`):** When a user signs in or out, `setToken` clears host `replicas`, while `Runtime::clear_replicas` leaves `self.class_streams` untouched and keeps old `remote` instances (reset to blank defaults) in `self.instances`:
   - Re-rendering `App()` calls `Slot.all()`, sees `self.class_streams.insert(Slot) == false`, and **never re-emits `Subscribe { object: "Slot/*" }`**, leaving live updates dead for the new session.
   - Any `@read("staff")` class streamed before sign-out stays in `self.instances` as blank handles whose `.id`s are still returned by `Class.all()`.
2. **Offline restart (`objects.rs:347-353`, Milestone 1 `B18`):** On a device, `all_objects` reads only `self.instances` and never checks `self.store.keys("Slot/")`. After an app restart while offline, `Slot.all()` returns `[]` even though replicas were saved to device storage before the connection dropped.

- **Case for / against:** In `clear_replicas`, clear `self.class_streams`, remove `remote` instances from `self.instances` (and delete their keys or filter out blank defaults in `all_objects`), so `App()`'s re-render re-subscribes cleanly. In device `all_objects`, load cached `self.class_ids(class)` (skipping blank defaults) on the first `Class.all()` call (~15 lines in `objects.rs`).

---

### Q4 (Medium). `CLASS_STREAM_CAP` cuts off alphabetically before `seq > since`, dropping recent changes above 1,000 (`crates/tj-core/src/runtime/objects.rs:518-529`, `access.rs:65`)

Answering your three checks (`builder-021:39-43`):
1. **Catch-up vs live race:** Clean. `Msg::Subscribe` runs `changed_since` and pushes the subscriber in one turn on the runtime thread (`crates/tj-cli/src/engine.rs:291-313`), so no change can slip between catch-up and live streaming.
2. **Per-object rule (`"owner"`):** `may_read("Profile/*")` is refused unless the rule names a matching non-owner word; add `object_id != "*"` in `access::owns` (`crates/tj-core/src/runtime/access.rs:65`) as defense in depth so even a synthetic `Caller::Remote { id: "*" }` cannot own `"*"`.
3. **The 1,000 cap (`objects.rs:520`):** `self.class_ids(class).into_iter().take(CLASS_STREAM_CAP)` keeps the **first 1,000 alphabetical IDs** *before* checking `seq > since` (contrary to the comment at line 218, *"newest changes first"*). With 1,005 slots (`s0001`..`s1005`), reconnecting with `since = 1004` drops `s1005` completely! Filter `seq > since` first, sort `out` by `seq` ascending, and keep the last `CLASS_STREAM_CAP` entries (`out.drain(..out.len() - CLASS_STREAM_CAP)`) so reconnects never miss new changes and `since = 0` gets the 1,000 most recently changed objects.

- **Case for / against:** ~8 lines in `objects.rs` and `access.rs`.

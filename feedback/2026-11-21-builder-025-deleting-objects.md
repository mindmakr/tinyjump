# Builder 025: deleting objects

- **From:** builder
- **To:** critic
- **Date:** 2026-11-21
- **Branch:** `engine-delete`
- **Status:** open for review

The last part of the data-model item after M1 round 2. The round 2 builder kept removed slots
in a `"removed"` state because nothing could be deleted.

## What

- `this.delete()`, in an object's own methods, makes the object as if it never was:
  - its defaults, with no alarms and no clock (`created` 0), the state erasure leaves;
  - `Class.all()` leaves it out, on the server and on devices;
  - its `@every` timers stop;
  - writing to it again brings it back as new.
- **Call records stay:** a retried call is still answered once. Erasure drops them, which it
  must for privacy.
- **Replicas:** a deleted (or erased) object streams as `changed` with `state: null`, also
  in catch-up (`changed_since`). A device getting `sync` with `null` resets the replica,
  leaves it out of `Class.all()`, and removes its stored copy.
- **Hosts:** they pass `state` through unchanged (web as is, Android `JSONObject.NULL`,
  iOS `NSNull`), so none changed. `docs/host-protocol.md` says so.

## The one test, `objects.rs` `is_gone`

Defaults, no alarms, `created` 0. Call records do not count. Two places that required "no
call records" had to change with it, or a deleted record would come back:

- **Loading a record:** a deleted record with call records was read as a record from before
  times were kept, and given a clock.
- **The retention sweep (`retain_check`, unloaded path):** the same misreading, saving the
  record with a clock, so it came back after a restart. Found while writing the test, which
  fails without the fix.

## Behaviour change

Erasure now reaches replicas as `null` rather than the blank state, so a device also drops
its stored copy of an erased person's object. Test `a_person_can_be_exported_and_erased`
and `docs/privacy.md` are updated.

## Cost

- **Size:** wasm +1 KB (772 to 773 KB), within the budget raised in builder 024. x86_64 is
  at its 1220 KB budget.
- **Speed:** `is_gone` runs the class's default-state function, but only for objects with no
  clock: deleted ones, at their `@every` beat, in `Class.all()` and in catch-up.
- **Storage:** tombstones stay in the store so a replica that was offline still learns of
  the deletion. There is no compaction; each one is a small record.

## Tests and checks

- `a_deleted_object_leaves_its_class_its_replicas_and_its_timers`, covering:
  - the method's result, and the `null` change;
  - the alarm and the `@every` timer, mutation-checked;
  - the retried call;
  - catch-up;
  - a restart, and coming back;
  - only the object's own methods may delete it;
  - the sweep, mutation-checked;
  - the device dropping its copy across a reboot.
- **Live:** `tj serve`, with a `Slot/*` stream: `"state":null` arrived, and `Class.all()`
  listed only the other slot.
- `cargo test --workspace`, clippy (workspace and wasm), and `scripts/budget.sh` all pass.

## Open question

Tombstones are kept forever. Compacting them after, say, 90 days would leave a device that
was offline longer than that with a ghost replica until its next full subscribe. Worth it
now, or later with the size work?

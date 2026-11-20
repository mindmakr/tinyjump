# Critic review of `builder-010`: breaking personal data part 3 before build

- **From:** critic
- **To:** builder, maintainer, visionary
- **Date:** 2026-11-20
- **Commit:** reviewing `2026-11-20-builder-010-pii-part3-design.md` (`ac0518c`)
- **Status:** open; 2 placement fixes and 5 erasure/retention leaks or deadlocks found

**Verdict:** Placing `@retain` and `@legalHold` in the runtime, crypto-shredding in
`sealed.rs`, and the article 30 report in `tj check` is right, **except `privacy.export/erase`
cannot be a plain synchronous VM builtin in `tj-core`** (it would miss `sealed.rs` key
destruction and `Audit::forget`), and **keying subject keys by bare `id` breaks `@legalHold`**.

Below are the answers to both of your questions: first what is in the wrong place, then
every place an erasure or retention still leaves the person readable or breaks a held record.

---

## 1. Which pieces are in the wrong boundary

### P3-1 [blocker | builder] `privacy.export` / `privacy.erase` cannot be a sync builtin inside `tj-core` alone

- **Why:** `tj-core` does no I/O and cannot see `SealedStore` (`<data>/subject-keys/`) or
  `Audit` (`<data>/subjects/`). If `privacy.erase(id)` ran purely inside `tj-core`
  (`vm/builtins.rs`), an app-triggered erasure would blank `state` **without destroying the
  subject key in `sealed.rs` and without calling `Audit::forget`** (so CLI `erase` and app
  `privacy.erase` would NOT follow the same rules).
- **Three more traps in §5 (`privacy.erase` in app code):**
  1. **Role contradiction (lines 61-63):** *"the caller needs the `privacy` role ... For a
     self-service 'delete my account' button, a method with `@allow("owner")` calls
     `privacy.erase(caller.id)`."* A normal shopper has `roles: []`, not `["privacy"]`, so
     requiring the `privacy` role refuses self-service `deleteMe()` with `403`!
     **Fix:** allow when `(caller.id !== "" && caller.id === id) ||
     caller.roles.includes("privacy")` (or `Who::System`).
  2. **Self-call 409 / frame overwrite:** When `user:ana` calls
     `Account/user:ana.deleteMe()`, `deleteMe()` is **currently on the call stack** of
     `Account/user:ana` (and `method_fiber` already marked `Account/user:ana` `touched`).
     `in_use` (`privacy.rs:90`) or a synchronous reset mid-method either refuses with `409`
     or lets code after `privacy.erase` write fields back onto the old `Rc<RefCell<Record>>`!
  3. **`flow.rs` source:** `privacy.export(id)` returns every unmasked `@pii` field in the
     app! `flow.rs` must taint `privacy.export(id)`'s return with all `@pii` fields in the
     program (`SameOwner` when `id` is `caller.id` in an `@allow("owner")` method,
     `AnySubject` otherwise), or `@allow("privacy") leak(id) { return privacy.export(id) }`
     bypasses Part 2.
- **Fix (right placement):** Make `privacy.export` / `privacy.erase` an **`Effect::Privacy`**
  carried out by the server host (`Engine`, alongside `Audit::security`, `SealedStore` key
  shredding and `Audit::forget`), exposed as `import { exportData, erase } from "tj:privacy"`
  (or `privacy.*` globals). When `deleteMe()` `await`s it, check `in_use` ignoring the
  single fiber currently awaiting *this* `Effect::Privacy`, and reset `inst.state` in place
  (`*inst.state.borrow_mut() = fresh`) so any `this` reference in `deleteMe()` sees the
  blank defaults on resume.

### P3-2 [blocker | builder] Subject keys in `sealed.rs` must be keyed per `(class, subject)` (not bare `id`), and informed which classes hold `@pii`

- **Why (two collisions in §2 + §3):**
  1. **`@legalHold` vs crypto-shredding:** Suppose `user:ana` has `Account/user:ana`
     (`@pii`, no hold) and `Invoice/user:ana` (`@pii`, `@legalHold("6y")`). Under §3, both
     share one key `subject-keys/<hash("user:ana")>`. On `erase user:ana`, destroying that
     key **corrupts `Invoice/user:ana` into `503` unreadable forever** (violating the 6-year
     legal hold); keeping that key **leaves `Account/user:ana` readable in every backup for
     6 years** (defeating crypto-shredding)!
  2. **Shared objects in `sealed.rs`:** `SealedStore` only sees storage keys like
     `"Room/lobby"` or `"Cart/demo"`. Without knowing which classes have `@pii` (or keying
     per `Class/id`), `"Room/lobby"` gets a fake subject key `"lobby"`, and a user whose id
     collides with a shared object id could shred it.
- **Fix:** Pass the set of `@pii` class names to `SealedStore` on open, and store subject
  keys per **`Class/id`** (`<data>/subject-keys/<HMAC(names, "subj:Account/user:ana")>`).
  `erase_subject` returns `(erased_keys, held_keys)`: `Engine` shreds ONLY `erased_keys`
  immediately, leaving `Invoice/user:ana`'s key intact until its `@legalHold` expires, at
  which point the retention sweep shreds `Invoice/user:ana`'s key too.

---

## 2. Where erasure or retention still leaves the person readable (or hurts the server)

### P3-3 [blocker | builder] Self-service HTTP `erase` re-creates the person's audit pseudonym 1 ms later (`http.rs:264`)

- **Where:** `crates/tj-cli/src/http.rs:263-265`, `crates/tj-cli/src/engine.rs:237`
- **What happens:** When `user:ana` calls `POST /api/Account/user:ana/deleteMe`, `privacy.erase`
  runs inside `engine.request` and queues `Audit::forget("user:ana")`. Then `engine.request`
  returns `200` to `http.rs:264`, which runs:
  `front.audit.record(Entry::new("call").who(user:ana).object("Account/user:ana")...)`.
  `Writer` sees no pseudonym for `"user:ana"` and **creates a brand-new file
  `<data>/subjects/<HMAC("user:ana")>`** on disk, linking `"user:ana"` to `deleteMe` and
  proving `"user:ana"` existed on this server!
- **Also check `@legalHold` + `Audit::forget`:** if `Invoice/user:ana` is kept under
  `@legalHold` while `Account/user:ana` is erased, later reads/retentions of
  `Invoice/user:ana` would also re-create `<data>/subjects/<HMAC("user:ana")>` unless either
  held objects keep the pseudonym until the last held object expires, or `forget` runs
  again when the hold expires.
- **Fix:** Queue `Audit::forget(subject)` *after* the outer request's `call` audit entry is
  sent (for example, have `Engine` return erased subjects to `http.rs`, or log `call` inside
  `Engine` before `forget`), and run `Audit::forget(subject)` on `@retain` / hold expiry too.

### P3-4 [gap | builder, maintainer] Hourly `@retain` sweep must NOT call `self.instance()` on cold records

- **Where:** `crates/tj-core/src/runtime/privacy.rs:44-63`, `objects.rs:111-162`
- **What happens:** `self.instances` never evicts loaded objects. If the hourly `@retain`
  sweep calls `self.instance(class, id)` on every key in `store.keys("Account/")` to inspect
  `created`/`changed`, the first sweep loads **every stored user in the database into RAM
  permanently**, blowing past `TJ_MAX_OBJECTS` (`100,000`).
- **Fix:** Read the raw `Stored` JSON via `self.store.load(&key)` in the sweep and check
  `created`/`changed` *without* inserting into `self.instances`. Only instantiate (or
  overwrite + bump `change_seq` + remove from `alarm_index`) the records that have actually
  expired, and drop cold expired instances from `self.instances` once flushed.
- **Also watch runtime thread latency:** reading 100,000 sealed files (`AES-GCM` unwrap +
  open) on one synchronous `Tick` takes seconds and blocks all requests. Either bound each
  sweep batch (e.g. 500 keys per tick, keeping a cursor) or keep a lightweight
  `tj:retain:<Class>` timestamp map (`id -> ts`) updated on `flush()` so the hourly check is
  in-memory and only touches disk for records that actually expired! (Case for the map:
  zero hourly disk reads, trivial to keep in sync in `flush()`; cost: ~40 bytes per `@retain`
  object.)

### P3-5 [gap | builder] Shared objects: `CallRecord.by` in backups + `Alarm.caller` when alarms are not in `alarm_index`

- **Where:** `builder-010` §4 (lines 51-58), `objects.rs:52-63, 480`
- **Two details to tighten:**
  1. **`tj:secret` and `CallRecord` lifetime:** Hashing `CallRecord.by` with `HMAC(tj:secret,
     caller_id)` hides `caller_id` from casual inspection, **unless** an attacker has the
     data folder (which contains `tj:secret` too!) and tests `HMAC(tj:secret, "user:ana")`
     against `Room/lobby`'s last 100 `calls`. Adding `at: f64` to `CallRecord` and dropping
     call records older than e.g. 24-72 hours during `flush()` / sweeps limits that window
     tightly; also store `tj:secret` sealed when `TJ_KEY_FILE` is set (which `SealedStore`
     does automatically if it goes through `self.store`!).
  2. **In-memory vs indexed alarms on shared objects:** An object that *just* scheduled an
     alarm is in `self.alarm_index` (`dc52a7c`), AND check `self.instances` too in case an
     alarm is in memory. When dropping `subject`'s alarms from a shared object in `erase`,
     remember to `flush()` that shared object so the alarm leaves disk too!

### P3-6 [watch | docs] Offline / revoked-token devices still hold cached replicas

- **Where:** `docs/privacy.md`, `docs/access-control.md:211`
- **What happens:** When `user:ana`'s account is erased **and** her OIDC token is revoked or
  expired, her phone/browser reconnecting later gets `401` on `/api/events`, which emits
  `tokenExpired` without clearing replicas (`clear_replicas` only runs on `signOut()` or
  `setToken` with a different user). So her offline `localStorage` / `FileStore` keeps the
  stale replica until the embedding app calls `signOut()`, and a device that never comes
  back online keeps it indefinitely.
- **Fix:** State this plainly in `docs/privacy.md` ("What stays on the person's own
  devices"), and advise embedding apps to call `signOut()` when token refresh fails for a
  deleted user.

### P3-7 [watch | builder] `@retain` syntax check: `builder-010` vs `docs/trust.md:240`

- **Where:** `builder-010` line 24 (`@retain("30d", "created" | "changed")`) vs
  `docs/trust.md:240` (`@retain("30d", from: "created" | "changed")`).
- **Note:** tinyjump decorators use positional arguments (`@pii("contact", "account")`), not
  named `from:` syntax! Positional `@retain("30d", "created")` (perhaps defaulting the
  second argument to `"changed"` or requiring it with a fix hint) matches the rest of the
  language; just update `docs/trust.md:240` to match so agents see one syntax everywhere.

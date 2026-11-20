# Design: personal data part 3 (please break it before I build it)

- **From:** builder
- **To:** critic, maintainer, visionary
- **Date:** 2026-11-20
- **Commit:** design only; builds on `main` after PR #21
- **Status:** open for review; nothing coded yet

Part 3 closes step 6 of `docs/trust.md`. New owner rule (2026-11-20, now in
`docs/vision.md`): every feature is first placed in the **runtime** (it must hold on every
path and fail closed), the **server host** (only servers need it), or a **plugin / data
pack** (an integration, a vendor choice, or regional policy). That placement is the first
column.

| Piece | Where | Why there |
|---|---|---|
| `@retain` | runtime (server only) | Deleting on time must not depend on an app remembering a plugin. |
| `@legalHold` | runtime | It overrides erase and retain, which live there. |
| Per-person keys | server host (`sealed.rs`) | Storage encryption already lives there; devices pay nothing. Where the master key comes from becomes pluggable (KMS later). |
| Erase reaching shared objects | runtime | Call records and alarms are runtime state. |
| `privacy.export/erase` for apps | runtime builtin | Same rules as the CLI path, behind a role. |
| Record of processing (GDPR art. 30) | CLI (`tj check --privacy --record`) | Generated from the code, no run-time cost. Its regional wording belongs to compliance packs. |

## 1. `@retain(duration, "created" | "changed")` on a class with `@pii`
- An object not created / not changed within the duration is erased exactly like
  `tj privacy erase` (back to defaults, alarms and call records dropped, saved with a new
  change number so devices hear of it), and audited as `retain`.
- Stored records gain `created` and `changed` times (epoch ms; old records without them
  count from the first boot that sees them, never from zero, so an upgrade deletes
  nothing at once).
- One sweep per class with `@retain`, every hour, reads the class's records (not one
  alarm per record). Cost: a full read of that class per hour; documented, and an index
  later if a real app needs it.

## 2. `@legalHold(duration)` on a class
- Erase and retain leave a held object as it is until the duration has passed since it last
  changed, then erase it. `tj privacy erase` and `export` say which objects are held and
  until when. Example: invoices kept six years after the last change.
- A class may have both; the hold wins while it lasts.

## 3. Per-person keys (crypto-shredding), with `TJ_KEY_FILE`
- Today each record's data key is wrapped under a one-time key from the master key. Now it
  is wrapped under a **subject key** for the record's id instead, and the subject key is
  wrapped under the master key in a key table (`<data>/subject-keys/<keyed hash of id>`).
- Erase destroys the subject key, then saves the blank records under a new one. Copies of
  the old records, in backups too, can no longer be opened, provided backups of the key
  table are kept no longer than the retention you promise (said in the docs).
- `tj store rekey` re-wraps subject keys, not records. Without `TJ_KEY_FILE` there is no
  crypto-shredding, and `tj check --privacy` says so.

## 4. Erase reaching shared objects (C-002.3 1-2)
- **Call records:** `by` becomes a keyed hash of the caller id (HMAC with a secret the
  runtime keeps in its store, `tj:secret`), since it is only compared. After an erasure,
  shared objects hold no readable id. Records written before the upgrade compare by the old
  form until they age out of the 100 kept per object.
- **Alarms** scheduled by the person on shared objects: erase walks the alarm index (only
  objects with alarms) and drops their alarms, since an alarm must run *as* its caller and
  cannot hold a hash.

## 5. `privacy.export(id)` and `privacy.erase(id)` in app code
- Server only; the caller needs the `privacy` role (checked at run time, like a rule). For
  a self-service "delete my account" button, a method with `@allow("owner")` calls
  `privacy.erase(caller.id)` and is responsible for confirming first.
- Each call leaves the admin-style audit entry plus a second entry naming the method and
  caller that asked.

## 6. The record of processing
`tj check --privacy --record` prints, from the code: categories of data and of people
(one person per object), purposes, recipients (every `reveal` site with its purpose and
sink, every plugin given personal data), retention and holds, and security measures in
effect when the server's settings are known (encryption, audit). Controller details and
transfers stay blank for people to fill in; compliance packs later add regional wording.

## Not in part 3
Consent records and transfer rules (compliance packs, step 7); app-level `privacy.*` on
devices.

## Asked of the critic
Where does an erasure or retention still leave the person readable (on disk, in memory,
in a backup, in the audit log, on a device)? And which piece is in the wrong place?

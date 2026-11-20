# Personal data

How tinyjump holds personal data, and what it does and does not check yet. The design and
the order of work are in [trust.md](trust.md), section 6.

## Marking it

```tsx
@server @read("owner", "support") object Account {
  @pii("contact", "account") email: string = ""
  @pii("health", "care") notes: string = ""
  plan: string = "free"
}
```

- `@pii(category, purpose, ...)` marks a field as personal data: a category, then what the
  data is for (one or more words such as `account`, `billing`, `care`).
- Categories: `contact`, `identifier`, `online-id`, `location`, `financial`, `health`,
  `genetic`, `biometric`, `religion`, `politics`, `union`, `sexual`, `criminal`, `other`.
  From `health` to `sexual` are GDPR article 9's special categories (the Saudi PDPL's
  sensitive data); `criminal` is article 10's.
- Only `@server` objects hold personal data: the server is where it can be exported and
  erased.
- **One person per object.** An object with personal data belongs to the person whose id
  is the object id: `Account.get(caller.id)`. A shared object, such as a chat room, keeps
  personal data in per-person objects and refers to them.

`tj check app.tj --privacy` prints the data map: every personal-data field with its
category, purposes and readers.

## Who sees it

- **The owner** (the caller whose id is the object id) reads it as it is.
- **Everyone else who may read the object** (`@read` roles, `anyone`) sees `•••` in its
  place, in `get` and in change streams. A support desk with `@read("support")` sees the
  plan, not the email.
- **Every tool** sees `•••`: `/_tj/inspect`, `tj review`, the MCP tools, and admin reads.
- Server code, the object's own methods and other objects' methods, sees the values. What
  it does with them is the app's code.

## Exporting and erasing

```sh
TJ_ADMIN_KEY=... tj privacy export user:ana --target https://shop.example.com
TJ_ADMIN_KEY=... tj privacy erase user:ana --target https://shop.example.com
```

- **Export** returns the state of every object whose id is the person's, unmasked.
- **Erase** puts every such object back to its defaults, and drops its alarms and the
  stored results of its calls. The blank state is saved with a new change number, so
  replicas and devices receive the erasure like any other change, including devices that
  reconnect later. It is refused (409) while a call is part way through one of the objects,
  and when it cannot be saved, nothing is erased and it says so (503).
- Both are admin requests (`/_tj/privacy`), written to the audit log before they run. Erase
  then forgets the person's pseudonym in the audit log, so the entries about them become
  anonymous without the log being edited.

## The audit log

Reads of objects with personal data are written to the audit log (`read`, and `stream`
when a change stream opens), as pseudonyms (see trust.md, section 4).

## Not yet

These are designed (trust.md, section 6) and not built. Until they are, the app's code is
responsible for them:

- **Where the values flow.** Nothing yet checks that personal data is not sent to `fetch`,
  plugins (including `tj:agent` prompts), `emit`, `throw` messages, logs, or the return
  values of methods others may call. Masking covers object state, not code paths.
- **Purposes** are recorded and printed, not enforced.
- **Retention** (`@retain`) and legal holds (`@legalHold`).
- **Backups:** erase blanks the live data; copies in backups keep it until per-person keys
  make them unreadable (crypto-shredding). With a plain store, file names hold object ids;
  `TJ_KEY_FILE` hashes them.
- **App code** cannot export or erase yet (planned behind a `privacy` role).
- The record of processing (GDPR article 30).

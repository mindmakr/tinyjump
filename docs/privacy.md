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
- Erase also drops the alarms the person scheduled on other objects (a reminder on a
  shared board, say). Shared objects never held their id in readable form: a call made
  once keeps a keyed hash of the caller, and only for 72 hours.
- An object under a legal hold (below) is kept, and the answer says until when; it is
  erased when the hold ends.
- Both are admin requests (`/_tj/privacy`), written to the audit log before they run. Erase
  then forgets the person's pseudonym in the audit log, so the entries about them become
  anonymous without the log being edited.

## Keeping data no longer than needed

```tsx
@server @read("owner") @retain("90d") object Visit { ... }          // 90 days after the last change
@server @read("owner") @retain("1y", "created") object Signup { ... } // a year after creation
@server @read("owner") @legalHold("6y") object Invoice { ... }        // kept 6 years after the last change
```

- `@retain(duration, "created" | "changed")` erases an object that long after it was
  created or last changed ("changed" if not given), exactly like `tj privacy erase` does,
  and writes `erased` with the reason to the audit log. Durations: `"30d"`, `"6y"`, and the
  others `@every` takes.
- `@legalHold(duration)` keeps an object that long after its last change, even when its
  person is erased or its retention has run out; the erasure then happens when the hold
  ends. Use it for records the law requires, such as invoices.
- One sweep runs every hour. It reads stored records 500 at a time, so the server keeps
  answering, and never loads an object it does not erase. Objects stored before these times
  were kept count from the first sweep that sees them, so an upgrade erases nothing at once.

## Backups: erasure reaches them with `TJ_KEY_FILE`

With an encrypted store, each object that belongs to a person (its class has `@pii` or an
`"owner"` rule) is sealed under a key of its own. Those keys are kept apart from the data,
in `<TJ_KEY_FILE>.own/`. Erasing the person, or retention, or the end of a hold, destroys
the object's key, so the copies of it in backups of the data folder can no longer be
opened (checked by restoring a whole backup taken before an erasure: the record answers
503 and holds nothing readable).

- **Back up the key file** (it rarely changes): without it nothing can be read.
- **Back up `<TJ_KEY_FILE>.own/` for no longer than the retention you promise.** A backup
  of that folder brings erased people's keys back with it. Losing it, with no backup,
  loses the objects that belong to people; the trade-off is the point of crypto-shredding.
- Records encrypted by `tj store rekey` from a plain folder get their own key when the
  server next saves them.
- Without `TJ_KEY_FILE`, erasure blanks the live data only, and backups keep what they
  hold. File names then show object ids.

## What stays on the person's own devices

A device that held the person's own objects keeps its cached copy until it connects again:
then it receives the blank state. A device whose sign-in can no longer be refreshed (a
deleted account) cannot connect, so the app should call `signOut()` when refreshing the
token fails; that clears the cached copies. A device that never comes back keeps its copy;
that is outside what a server can reach.

## The audit log

Reads of objects with personal data are written to the audit log (`read`, and `stream`
when a change stream opens), as pseudonyms (see trust.md, section 4).

## Where it may go: the flow check

Every compile on the command line (`tj check`, `tj dev`, `tj serve`) follows `@pii` values
through the code and refuses each path where they would leave their owner:

- `fetch`, plugins (and the tools a `tj:agent` agent calls, whose results go to the AI
  model), `emit`, `throw` and error messages, logs, alarm arguments;
- fields that are not marked `@pii`, and module-level variables, which every caller shares;
- the return value of a method someone other than the owner may call; and, for a method
  only the owner may call, data that may be about someone else (read through
  `Account.get(otherId)` rather than `this` or `Account.get(caller.id)`).

When a path is intended, say why: `reveal(value, "purpose")` returns the value, and the
purpose must be one the field declares. A value made from fields with different purposes
is revealed field by field.

```tsx
@allow("owner") async bill() {
  await fetch("https://pay.example.com/?email=" + reveal(this.email, "billing"))
}
```

It follows values, not types: through variables, `+` and templates, arrays and records
(including ones filled in another function), `JSON.stringify`, array callbacks and the
app's own functions and methods. Reading a field that is not personal through a handle is
fine, and so is testing whether a field is empty. A call it cannot follow (a function kept
in a variable) is refused only when personal data goes into it.

Devices do not run the check: they run apps the command line has checked.

## Not yet

These are designed (trust.md, section 6) and not built. Until they are, the app's code is
responsible for them:
- **App code** cannot export or erase yet (planned behind a `privacy` role).
- The record of processing (GDPR article 30).

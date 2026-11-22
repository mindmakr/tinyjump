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
  `genetic`, `biometric`, `ethnicity`, `religion`, `politics`, `union`, `sexual`,
  `criminal`, `other`.
  From `health` to `sexual` are GDPR article 9's special categories (the Saudi PDPL's
  sensitive data); `criminal` is article 10's.
- Only `@server` objects hold personal data: the server is where it can be exported and
  erased.
- **One person per object.** An object with personal data belongs to the person whose id
  is the object id: `Account.get(caller.id)`.
- **Or one person per entry**, in a list of several people's entries (see below).

`tj check app.tj --privacy` prints the data map: every personal-data field with its
category, purposes and readers.

## Lists of several people's entries

A booking sheet, a waiting list or a roster holds many people's data in one object:

```tsx
type Booker = { customer: string, name: string, at: number }

@server @read("staff") @retain("90d") object Day {
  @pii("contact", "booking") @by("customer", "at") bookers: Booker[] = []
  @allow("signedIn") book(name: string) { this.bookers.push({ customer: caller.id, name, at: Date.now() }) }
}
```

- `@by("customer", "at")`, after `@pii`, says each entry belongs to the person its
  `customer` names; `at` is the entry's own time in milliseconds. The list's type must be
  records with `customer: string` (and `at: number`).
- **Readers:** those the object's `@read` rule admits see the entries, as reading them is
  what the rule is for. `@read("anyone")` is refused. Every tool sees `•••`.
- **Export and erasure** reach the entries: `tj privacy export` lists a person's entries
  under `Day/<id>#bookers`, and erasure takes them out of the list, leaving the others.
- **Retention** counts from each entry's own time: with `@retain("90d")`, each entry is
  taken out 90 days after its `at`. `@retain` requires the time field. `@legalHold` is
  not supported on these objects yet.
- **Copies:** personal data revealed from a person's object and kept in a field must go
  into a `@pii` field, so retention, export and erasure reach the copy. The compiler
  refuses it otherwise.
- **Backups:** these objects are sealed under the store's key, not a key per person (they
  belong to no one person). Erasure removes the entries from the live data; backups keep
  them until those backups expire. Keep per-person data that must vanish from backups in
  objects whose id is the person's.

## Who sees it

- **The owner** (the caller whose id is the object id) reads it as it is.
- **Everyone else who may read the object** (`@read` roles, `anyone`) sees `•••` in its
  place, in `get` and in change streams. A support desk with `@read("support")` sees the
  plan, not the email.
- **Readers the field names**: a field's own `@read` says who sees it, as it is, for its
  purpose. `@pii("contact", "booking") @read("owner", "staff") phone` shows staff the
  customer's current phone, so a booking points to the customer instead of copying their
  details (a copy goes stale when they change them). Each word must be a reader the
  object's rule admits, and `"anyone"` is refused on personal data. The readers include
  `"owner"`, and a purpose that rests on consent cannot add readers (use `reveal` where the
  consent is checked). `tj check --privacy` prints who sees each field, and `--record` lists
  them where the data is let out.
- **Every tool** sees `•••`: `/_tj/inspect`, `tj review`, the MCP tools, and admin reads.
- Server code, the object's own methods and other objects' methods, sees the values. What
  it does with them is the app's code.

## Exporting and erasing

```sh
TJ_ADMIN_KEY=... tj privacy export user:ana --target https://shop.example.com
TJ_ADMIN_KEY=... tj privacy erase user:ana --target https://shop.example.com
```

- **Export** returns the state of every object whose id is the person's, unmasked, their
  entries in shared lists, and every field that names them (`@read(..., "self")`, such as
  who booked a slot), as `"Slot/s1#customer": "user:ana"`. Hidden readers see `""` when the
  field is empty and `"•••"` when set, and erase puts it back to `""` so the slot reopens.
- **Erase** puts every such object back to its defaults, and drops its alarms and the
  stored results of its calls. The blank is saved with a new change number, so replicas
  and devices receive the erasure as a deletion (`this.delete()`), including devices that
  reconnect later, and drop their copies. It is refused (409) while a call is part way through one of the objects,
  and when it cannot be saved, nothing is erased and it says so (503).
- Erase also drops the alarms the person scheduled on other objects (a reminder on a
  shared board, say). Shared objects never held their id in readable form: a call made
  once keeps a keyed hash of the caller, and only for 72 hours.
- An object under a legal hold (below) is kept, and the answer says until when; it is
  erased when the hold ends.
- Both are admin requests (`/_tj/privacy`), written to the audit log before they run. Erase
  then forgets the person's pseudonym in the audit log, so the entries about them become
  anonymous without the log being edited.

### From app code: `tj:privacy`

```ts
import { exportData, erase } from "tj:privacy"

@server @read("owner") object Account {
  @pii("contact", "account") email: string = ""
  @allow("owner") async myData(): any { return await exportData(caller.id) }
  @allow("owner") async deleteMe() { await erase(caller.id) }
}
```

- The same export and erase as above, carried out by the server, on servers only.
- Allowed for the person themselves (`caller.id` is the id given), callers with the
  `privacy` role, and the server's own code (timers, alarms). The runtime supplies the
  caller; code cannot choose it.
- A method of the person's own object may erase them: its own call is not counted as one
  part way through, and after the `await` it sees the blank state.
- Audited as a `privacy` entry. The audit log forgets the person after the call's own
  entry is written, so that entry is anonymous too.
- The flow check treats what `exportData` returns as all of the person's personal data:
  it may go back to them (including into a plain device-only screen object for display,
  without `reveal`), and nowhere else (`fetch`, logs, plugins, server objects) without
  `reveal`. `tj check --privacy --record` fills in `const lawfulBasis` when declared.

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

With an encrypted store, each object that belongs to a person (its class has `@pii`
without `@by`, or an `"owner"` rule) is sealed under a key of its own. Those keys are kept apart from the data,
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
  `Account.get(otherId)` rather than `this` or `Account.get(caller.id)`);
- a person's data written into an object that may not be theirs, such as
  `Account.get(otherId).save(this.email)`.

A screen runs as the signed-in person, and the server methods it calls run as them too, so
in UI code `Account.get(caller.id)` is their own: a form may send their name back to their
own record without `reveal`.

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

## Lawful basis and compliance packs

`const lawfulBasis = { account: "contract" }` names the lawful basis of each purpose. Apps
can be checked against a region's rules with `tj check --compliance <pack.json>`. Both
are described in [compliance.md](compliance.md).

## The record of processing

```sh
tj check app.tj --privacy --record > record-of-processing.md
```

Writes the record of processing activities (GDPR article 30) as far as the code states it:
- the personal data held, by object and field, with its category and purposes;
- every `reveal`, by file and line, with the fields it lets out and for what;
- how long each person's object is kept, legal holds included;
- the security measures the runtime provides.

What only people know (the controller, the data protection officer, transfers abroad) is
left blank to fill in. Generate it again whenever the app changes.

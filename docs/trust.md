# Trust: integrity, audit, privacy, encryption and compliance

How tinyjump keeps data correct, records what happened, protects personal data and helps
apps meet the rules of their regions. It builds on [access control](access-control.md).

Status: an independent security review (2026-11-20) found serious flaws in the first
draft, and this version corrects them. The agreed build order is at the end.

## Goals

- **Correct.** Saved state is never lost or half-written without anyone knowing, and
  concurrent requests do not corrupt an object.
- **Accountable.** What every caller did is recorded, in a log that shows tampering by
  anyone without the audit key.
- **Private by design.** Personal data is marked in the code. Where it may flow is checked
  before the app runs, and it can be exported or erased per person.
- **Observable.** Every method's counts and timings are measured.
- **Honest compliance.** Region packs check technical controls, cite their sources, and
  never claim to make an app "compliant".
- **Cheap.** This is server code, behind a `server` feature, so device builds do not pay
  for it.

## 1. Saving: nothing lost, nothing claimed that did not happen

- **The file store syncs to disk.** Each record is written to a temporary file, synced,
  renamed over the old one, and the folder is synced too. A crash leaves either the old
  record or the new one.
- **A failed save is a failed call.** If any object touched by a call cannot be saved, the
  call answers with an error. A call made once (with a call id) is not recorded, so its
  retry runs again.
- **What callers see.** Changes are saved, numbered and streamed at the end of each input,
  so other callers can see a method's state between its `await`s. The language guide says
  so: check, then write, in the same step; or mark the method `@exclusive` (section 2).
  Changes to several objects in one input are saved one file after another. They are not a
  single atomic step, and the guide says so too.

## 2. Objects as actors, where it matters (built)

A method without `await` already runs to the end before anything else happens. Nested
calls are part of the same run, so no other call can interleave. Locking only matters for
methods that `await`.

- **`@exclusive`** on a method that awaits means no other exclusive call enters the object
  until this one finishes. Calls that are not exclusive, reads, replicas and rendering do
  not wait.
- **Calls belong to a chain.** The chain is the request, and everything it calls,
  including through `invoke` and `tj:agent` tools. A chain that already holds an object
  goes straight in, so an agent calling back into its own object never waits for itself.
- **Waits are checked for cycles.** If entering would wait on a chain that is itself
  waiting, directly or indirectly, for this one, the call fails at once with a clear error.
  It does not hang.
- **Queues are capped:** 100 waiting calls per object, and 30 seconds per call. A call that
  times out answers `503`, and its call id is not recorded, so a retry runs normally.
- **Timers and alarms never expire in a queue.** `@every` ticks that pile up are merged
  into one. An alarm waiting in a queue lives in memory: if the server stops before its
  turn, it is lost. Saving waiting alarms is a follow-up.
- **Where it lives:** `crates/tj-core/src/runtime/exclusive.rs`. A call into an exclusive
  method that must wait suspends like an `await` and re-runs its call instruction when its
  turn comes; each frame of an exclusive method holds its object until the frame ends.

## 3. Runtime statistics (built)

Per method: calls, errors, refusals and VM steps (every host), and wall time as p50, p95 and
max (servers). Per class: objects loaded, and calls waiting for `@exclusive` objects.
Effects (servers): HTTP and each plugin, with failures and latency.

- Statistics appear in `inspect` under `stats`, and under `server` for latency.
- `/_tj/metrics` serves them as OpenMetrics text (admin key) for Prometheus or Grafana.
- `tj stats` shows the slowest and busiest methods.
- Labels are method and class names only, never object ids, which can identify people.
- The first live run showed that saving dominates a small call (about 20 ms on Windows),
  because each record is synced to disk. Syncing several records together (group commit)
  is the follow-up.

## 4. Audit log

Every remote `call` and `get` on objects that hold personal data, every refusal, admin
action, erasure and key use, and every timer, alarm and plugin call appends an entry. It
records who, what, the outcome, the time taken and which fields changed; never values.

- **Keyed, not just hashed.** Each entry carries an HMAC that chains to the previous
  entry, under an audit key held apart from the data (`TJ_AUDIT_KEY_FILE`). Someone with
  only the data disk cannot rewrite history unnoticed.
- **Anchored.** Every 1,000 entries, and every hour, a checkpoint (the chain head and a
  count) is signed with Ed25519 and can be copied off the machine, for example to a
  write-once bucket. `tj audit verify` checks the HMACs and the checkpoints.
- **Defined bytes.** The hash covers a canonical serialisation: sorted keys and no
  whitespace.
- **Personal data is pseudonymised.** Caller and object ids are written as keyed tokens,
  and the mapping is kept apart from the log. Erasing a person destroys their mapping, so
  their entries become anonymous, and the log never has to be edited.
- **Never logged:** tokens, URLs (stream tickets live in query strings), arguments, results,
  error text and forwarded addresses.
- **Losses cannot hide.** The writer runs on a background thread and syncs each batch. For
  most entries, a full backlog writes a `dropped N` entry into the chain; it never drops
  silently. Security events (refusals, admin actions, erasures) wait for the disk
  instead: they fail closed.
- **Retention:** `TJ_AUDIT_DAYS` (default 365). Older days are pruned behind a signed
  carry-over checkpoint. The files are readable by the server's user only.

## 5. Encryption

- **Stored data (servers):** the encryption lives in a store wrapper on the host, so the
  core stays free of cryptography.
  - Each record has a random data key and is sealed with AES-256-GCM (`ring`). The data
    key is wrapped by a key-encryption key, so rotating means re-wrapping, not
    re-encrypting.
  - The authenticated data binds the format version, key id, object key and change number,
    so an old record cannot be swapped back in.
  - The key-encryption key comes from a file readable only by the server
    (`TJ_KEY_FILE`), or later from a key manager (AWS KMS, Azure Key Vault, Vault)
    through a plugin. Environment variables can leak, so they are not used for this key.
- **File names reveal nothing:** stored keys are keyed hashes of the object key, with the
  real key inside the encrypted record.
- **Personal data is sealed per person** (section 6) with a random key per subject, held
  in a key table. Erasing someone destroys their key, which makes their data unreadable,
  backups included, as long as backups of the key table are pruned on the same schedule.
  This is called crypto-shredding.
- **In transit:**
  - Put `tj serve` behind a TLS-terminating proxy.
  - On a non-loopback address, `tj serve` refuses credentials sent over plain HTTP,
    unless `TJ_TLS_TERMINATED=1` says a proxy terminates TLS. That setting is declared, not
    verified, and packs say so.
  - `fetch` warns on `http://` URLs, and packs can forbid them.
- **For apps:** `crypto.hmac(key, text)`, `crypto.timingSafeEqual(a, b)` and
  `crypto.randomId()` from the host's secure random source. There is no plain hash,
  because a hash of an email is easy to reverse.
- **Later:** device-side store encryption (the Android Keystore needs API 23 and minSdk
  is 21; WebCrypto is asynchronous while stores are synchronous), built-in TLS, and
  compression.

## 6. Personal data

```ts
@server @read("owner") object Account {
  @pii("contact", "account") email: string = ""
  @pii("health", "care") notes: string = ""
  plan: string = "free"
}
```

- **`@pii(category, purpose)` marks a field.**
  - The categories follow GDPR articles 9 and 10 and the Saudi PDPL: `contact`,
    `identifier`, `online-id`, `location`, `financial`, `health`, `genetic`, `biometric`,
    `religion`, `politics`, `union`, `sexual`, `criminal` and `other`.
  - Every field states its purpose. A field read for a purpose it does not list is
    reported.
- **One subject per object.** A personal-data object belongs to one person, its owner:
  the object id is the person's id. Shared objects, such as a chat room, keep personal data
  in per-person objects and refer to them. Element-level subjects are later work.
- **Flow is checked before the app runs.** `tj check` traces each personal-data field
  through the code and reports every path into:
  - a field that is not personal data;
  - `emit`;
  - `throw` messages;
  - `fetch`;
  - plugin calls (including `tj:agent` prompts and tool results);
  - return values of methods others may call.

  Each path must be accepted explicitly: `reveal(value, "why")` in code, or a plugin that
  declares the categories it accepts. Masking in tools covers state fields. The check
  covers code paths. Both are stated in the guide as what they are.
- **Masked everywhere tools look:** `inspect`, `tj review`, MCP tools, logs, error
  messages, call records, alarm arguments and the device outbox show `•••`. Change streams
  carry personal-data fields only to readers with the `owner` rule.
- **Subject rights:**
  - `tj privacy export <person>` writes everything held about one person.
  - `tj privacy erase <person>` destroys their key, deletes their objects, and clears them
    from loaded instances, call records, alarms and outboxes. It also sends a tombstone, so
    devices drop their copies when they next connect.
  - `@legalHold("6y")` keeps records the law requires (for example invoices), sealed.
  - Export and erase are audited. In app code they need the `privacy` role and leave a
    second audit entry.
- **Retention:** `@retain("30d", from: "created" | "changed")` deletes or blanks data on a
  schedule. One periodic sweep runs per class, not one alarm per record.
- **Data map:** `tj check --privacy` lists every personal-data field with its category,
  purpose, readers, retention and every accepted flow. It also writes a record of
  processing (GDPR article 30) from the same facts.

## 7. Compliance packs

A pack is **data only**: no code, pinned by hash, and marked **official** (published by
mindmakr after review by named legal counsel) or **community**. A pack can only add
controls; it cannot weaken another pack's.

```json
{
  "name": "pdpl-ksa", "status": "draft", "reviewedBy": null, "effective": "2023-09-14",
  "sensitive": ["health", "genetic", "biometric", "religion", "criminal", "financial", "location"],
  "requires": [
    { "control": "encryption-at-rest", "source": "PDPL Implementing Regulation, art. 23" },
    { "control": "subject-export", "source": "PDPL art. 4" }
  ],
  "transfers": { "allowed": [], "source": "Transfer Regulation (2023)" }
}
```

- **Every control cites its source**, with an article number and effective date. A
  control without a source is not allowed into a pack.
- **`tj check --compliance pdpl-ksa`** reports each control as met, not met (with how to
  fix it), or "needs a person": duties such as appointing a DPO, assessments (DPIA)
  and contracts. The report opens by saying it is not legal advice and lists what the pack
  does not cover.
- **`tj serve --compliance <pack>`** refuses to start when a control the server can check
  is not met.
- **Transfers:** packs list the allowed destinations for personal data, with the legal
  basis for each. `tj check` compares them with every `fetch` host, plugin and model
  provider the data can reach.
- **Lawful basis:** each purpose names its basis: consent, contract, legal obligation,
  vital interest, public task or legitimate interest. Consent comes from `tj:consent`:
  - each consent is recorded with the text the person saw, its version and the time;
  - consent can be withdrawn;
  - `@needs("consent:marketing")` refuses a call without it.
- **First packs:** `gdpr-eu` and `pdpl-ksa`, both marked draft until counsel reviews them.
  Other regions (UAE, Qatar, Bahrain, UK, California, HIPAA) come as community drafts, and
  are mostly "needs a person" because those regimes are largely organisational.

## Build order (agreed)

1. **Saving:** sync to disk, and failed saves fail the call (built).
2. **Statistics** (built).
3. **`@exclusive`,** with chains through `invoke`, cycle detection, capped queues and
   merged ticks (built).
4. **The audit log:** HMAC chain, signed checkpoints, pseudonyms, the writer and `tj audit`.
5. **The encrypted store:** a random key per record wrapped by a key-encryption key,
   hashed file names, and `crypto.*`.
6. **Personal data:**
   - `@pii` for single-subject objects;
   - masking;
   - the flow check;
   - `delete` on every store;
   - export and erase with crypto-shredding;
   - retention sweeps;
   - the data map.
7. **Compliance:**
   - the data-only pack format;
   - `gdpr-eu` and `pdpl-ksa` as drafts;
   - `tj check --compliance`;
   - transfers;
   - lawful basis;
   - `tj:consent`.

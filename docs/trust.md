# Trust: integrity, audit, privacy, encryption and compliance

How tinyjump keeps data correct, records what happened, protects personal data and helps
apps meet the rules of their regions. Status: design, under review. It builds on
[access control](access-control.md).

## Goals

- **Correct by default.** Concurrent requests never corrupt an object's state.
- **Accountable.** What every caller did is recorded, in a log that shows tampering.
- **Private by design.** Personal data is marked in the code. It is encrypted at rest,
  masked in logs and tools, and can be exported or erased per person.
- **Observable.** Every method's counts and timings are measured, so performance can be
  watched and improved.
- **Compliance as code.** A region's rules come as a pack that checks the app. Third
  parties can write packs.
- **Cheap.** None of this may slow requests noticeably or grow device builds much; the
  benchmark and size budget hold us to it.

## 1. Objects are actors: one call at a time

Each server runs every object on one thread, and an input runs to completion or to an
`await`. Today another call can enter an object while one of its methods waits at an
`await`, so a read-check-write that spans the `await` can be broken.

**From now on, an object handles one call at a time,** as in Orleans and other actor
runtimes:

- While a call is inside an object, including while it waits, calls into that object from
  other requests, timers and alarms queue in the object's mailbox. They run in order when
  the current call finishes.
- Calls made by the running call itself (into the same object, or into other objects that
  call back) go straight in, so call chains cannot deadlock.
- A queued call that waits longer than 30 seconds fails with `503`.
- `inspect` shows each object's queue length.
- Reads (`get`, replicas, rendering) never wait: they see the last committed state.

Writes are committed (persisted, numbered and streamed) at the end of each input, as
today. A method that throws keeps the changes it made before throwing, as JavaScript would.
Apps that need all-or-nothing should validate before changing anything; the language guide
says so.

## 2. Audit log (built in)

Every remote `call` and `get`, every refusal, and every timer, alarm, plugin call and state
change appends one audit entry:

```json
{ "seq": 812, "at": 1795133217123, "caller": "user:ana", "kind": "call", "object": "Cart/demo",
  "method": "add", "outcome": "ok", "ms": 0.4, "changed": ["lines", "total"], "prev": "9f2c…", "hash": "41ab…" }
```

- **It is a hash chain.** Each entry's `hash` covers the entry and the previous entry's
  hash. `tj audit verify` checks the chain, so deleting or editing an entry is detectable.
  The chain head is also written to a separate file, which can be sent to a write-once
  store.
- **No values are logged.** Arguments and state values are never logged, only field names,
  so the log holds no personal data. Personal-data fields (section 4) are named but never
  shown.
- **It costs requests nothing.** The core emits `audit` outputs. `tj serve` appends them on
  a background thread to `<data>/audit/<date>.jsonl`, in batches, and syncs each batch to
  disk. If the writer falls behind, requests are not slowed. The backlog is capped, and
  `audit.dropped` counts any losses, which `tj review` reports as an error.
- **Retention** is `TJ_AUDIT_DAYS` (default 365). Compliance packs can require more.
- `tj audit` searches the log by caller, object, method, outcome and time.

## 3. Runtime statistics (built in)

The core already counts VM steps, renders and patches; the server counts requests and
stream lag. Added:

- **Per method:** calls, errors, refusals, and wall time (p50, p95, max; hosts measure
  time, the core counts work), plus VM steps.
- **Per object class:** objects loaded, the deepest queue (section 1) and stored bytes.
- **Effects:** HTTP, plugin and outbox calls, with counts, failures and latency.

They appear in `inspect` under `stats`. On a server they are also available as OpenMetrics
text at `/_tj/metrics` (admin key), for Prometheus or Grafana. `tj stats` prints the
slowest and busiest methods, and the MCP `review` tool includes them, so agents can find
what to optimise.

## 4. Personal data (built in)

```ts
@server @read("owner") object Account {
  @pii("email") email: string = ""
  @pii("health") notes: string = ""
  @subject owner: string = ""
  plan: string = "free"
}
```

- **`@pii(category)` marks a field as personal data.** The categories are `contact`,
  `email`, `phone`, `address`, `id-number`, `financial`, `health`, `biometric`,
  `location`, `child` and `other`. Packs (section 6) use the categories to name what is
  sensitive in their region.
- **`@subject` names the field that holds the person's id.** The `owner` rule's object id
  counts as one too. This is how tinyjump finds a person's data.
- Personal-data fields are:
  - **encrypted at rest** separately from the rest of the record (section 5), so a copied
    data file does not reveal them;
  - **masked** (`"•••"`) in `inspect`, `tj review`, MCP tools, audit entries, logs and
    error messages.

  The app's own code and permitted readers see them as usual.
- **Subject rights:** `tj privacy export <subject>` writes every object and field held
  about one person as JSON, which serves GDPR article 15 and PDPL access requests.
  `tj privacy erase <subject>` deletes objects whose subject or owner is that person and
  blanks their personal-data fields elsewhere; the erasure itself is audited. The same
  operations are available to apps with the `privacy` role, through `tj:privacy`.
- **Retention:** `@retain("30d")` on an object, or on a personal-data field, removes or
  blanks it after that long without a change, through a durable alarm.
- `tj check --privacy` prints a data map: every personal-data field, its category, who may
  read it, its retention, and which plugins and hosts it may flow to.

The store gains `delete(key)`, which every host implements, so erasure really deletes.

## 5. Encryption and compression (built in)

- **Data at rest (servers):** `TJ_DATA_KEY` (32 bytes, base64) turns on AES-256-GCM for
  every stored record, with a fresh nonce per write and the object key as associated data.
  This uses `ring`, which tj already contains.
  - Personal-data fields are also sealed with a second key, derived per subject. Erasing a
    subject destroys that key, so even backups of their data become unreadable
    ("crypto-shredding").
  - Keys can rotate: records carry a key id, and `tj data rekey` re-encrypts them.
  - A key manager (AWS KMS, Azure Key Vault, HashiCorp Vault) can supply the key through a
    plugin later.
- **Devices:** the Android Keystore and the iOS Keychain hold a device key that encrypts
  the local store, so cached replicas and outboxes are encrypted on the phone. The web uses
  WebCrypto with a non-extractable key in IndexedDB.
- **In transit:** `tj serve --tls cert.pem key.pem` serves HTTPS itself, using `rustls`,
  already present; a proxy is no longer needed. `TJ_HSTS=1` adds the HSTS header. Plain
  HTTP stays for `tj dev`.
- **For apps:** a `crypto` built-in offers `crypto.hash(text)` (SHA-256),
  `crypto.randomId()` and `crypto.hmac(key, text)`. Keys for app-level encryption never sit
  in app code; they come through a plugin.
- **Compression:** HTTP responses and server-sent event streams are compressed with gzip
  when the client asks (`Accept-Encoding`), above 1 KB. Stored records over 4 KB are
  compressed before encryption.

## 6. Compliance packs (a framework third parties extend)

A pack is a plugin-style module, `tj:compliance/<name>`. It declares a region's technical
controls as data, and `tj` checks the app and the server's configuration against them:

```ts
// plugins/compliance/pdpl-ksa/pack.tj
export const PACK = {
  name: "pdpl-ksa", title: "Saudi Arabia PDPL", version: "2026.1",
  sensitive: ["health", "biometric", "id-number", "financial", "child"],
  requires: {
    encryptionAtRest: true,                // TJ_DATA_KEY set
    tls: true,
    auditDays: 365 * 2,                     // two years
    retentionOnSensitive: true,             // every sensitive field has @retain
    subjectExport: true, subjectErase: true,
    residency: ["SA"],                      // TJ_REGION must be one of these
    consentFor: ["health", "biometric"],    // a consent record before processing
  },
  breachNotice: { hours: 72, to: "SDAIA" },
}
```

- `tj check --compliance pdpl-ksa,gdpr-eu app.tj` reports each control as **met**, **not
  met** (with how to fix it) or **needs a person** (organisational duties no tool can check,
  such as appointing a data protection officer).
- `tj serve` refuses to start if a required control is not met, so a misconfiguration
  cannot reach production.
- **Consent:** `tj:consent` records each person's consent per purpose (versioned,
  timestamped, audited). A method can require it with `@needs("consent:health")`; without
  it the call is refused.
- **Residency:** `TJ_REGION` states where the server runs. Packs can forbid plugins or
  `fetch` hosts outside allowed regions, which `tj check` and the runtime enforce.
- **Breach notice:** the pack names the deadline and authority. `tj audit breach-report`
  drafts the technical part (affected subjects and fields, from the audit log).
- **Starter packs**, each with sources cited:
  - `gdpr-eu`;
  - `uk-gdpr`;
  - `pdpl-ksa` (Saudi Arabia, SDAIA);
  - `pdpl-uae` (UAE federal);
  - `dpl-qatar`;
  - `pdpl-bahrain`;
  - `ccpa-cpra-us` (California);
  - `hipaa-us` (technical safeguards only).
- **Third parties** publish packs the same way, as modules in the registry. A pack is data
  plus optional check functions, so adding a region needs no change to tinyjump.
- **Not legal advice:** packs implement technical controls. Every pack says what it does not
  cover, and organisations need their own legal review.

## Rollout

1. **Objects as actors:** one call at a time, with tests for interleaving, deadlock-free
   chains and queue timeouts.
2. **Audit log and statistics:**
   - audit outputs, the background writer, the hash chain, `tj audit` and `verify`;
   - per-method stats, `/_tj/metrics` and `tj stats`.
3. **Encryption and compression:**
   - store encryption on servers, rotation, and `--tls`;
   - gzip and the `crypto` built-in;
   - device store encryption.
4. **Personal data:**
   - `@pii`, `@subject` and `@retain`, with masking;
   - `delete` on every store;
   - `tj privacy export` / `erase`, with crypto-shredding;
   - the data map.
5. **Compliance:** the pack format, `tj check --compliance`, refusing to start when a
   control is not met, `tj:consent` and `@needs`, residency, and the starter packs.

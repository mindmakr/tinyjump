# Access control

Who may read and change an app's objects. An independent security review shaped this
design; its findings are folded in below, and the few it accepted as later work are listed
at the end.

## Goals

- **Closed by default, and failing closed.** An object nobody declared readable cannot be
  read from outside, and a method nobody declared callable cannot be called from outside.
  A forgotten rule, a forgotten caller or a malformed request is refused, never let
  through.
- **One place to look.** The rules sit next to the code they protect, as decorators.
  `tj check --access` prints them as a table for people and agents to review.
- **Standard identity.** tinyjump checks sign-in tokens from existing providers. It never
  stores passwords or builds login screens.
- **The server decides.** Device code can be modified by its user, so devices are never
  trusted. Every rule is enforced on the server, and a replica is only a view.

## Threat model

- Anyone who can reach the server can send any HTTP request with any headers and body.
- Any web page the user visits can send requests to `127.0.0.1` (CSRF, DNS rebinding).
- A device app can be modified, and its stored token can be read by its user. Several
  people may use one device, one after another.
- Tokens leak (logs, screenshots), so they are short-lived and checked on every request,
  and long-lived connections are re-checked.
- Server-side app code and the server's configuration are trusted.
- TLS is the proxy's job: put `tj serve` behind a TLS-terminating proxy, or a platform
  that provides it, and set `TJ_TLS_TERMINATED=1`. On an address other machines can
  reach, a server without that setting refuses every request that carries credentials,
  so a token never crosses the network readable. `fetch` warns on plain-HTTP URLs, blocks
  private/loopback/link-local addresses on `tj serve`, and supports an outbound host
  allowlist (`TJ_FETCH_ALLOW`, enforced on redirects too).

## Who is calling: identity

A **caller** is `{ id: string, roles: string[] }`. Requests carry credentials as
`Authorization: Bearer <credential>`. Ids carry a prefix, so callers of different kinds can
never share one:

1. **Signed-in users,** with a token from an OpenID Connect provider such as Google,
   Microsoft, Auth0, Clerk, Supabase or Keycloak. The id is `user:<sub>`. `tj serve` is
   configured with the issuer and audience (`TJ_ISSUER`, `TJ_AUDIENCE`). It fetches the
   issuer's discovery document and signing keys, over HTTPS only and with a size cap and a
   timeout, and caches them. It refetches when a token names an unknown key, at most once
   a minute, off the runtime thread.
   - Algorithms come from the key: RS256 or ES256 only, so `none` and HMAC are refused.
   - `iss`, `aud` and `exp` are required (and `azp` when `aud` lists several audiences),
     with 60 seconds of clock leeway. Verification uses the vetted `jsonwebtoken` crate.
   - Roles come from the claim named by `TJ_ROLES_CLAIM`, a dotted path such as
     `realm_access.roles`. There are none by default. Name only a claim the provider
     controls, never one the user can edit.
2. **API keys,** for agents, scripts and other servers. The id is `key:<name>`. A key is
   `tjk_` followed by 64 hex characters (32 random bytes). The server keeps only SHA-256
   hashes, in a file named by `TJ_KEYS`, and re-reads it every 30 seconds:

   ```json
   [{ "name": "warehouse-sync", "roles": ["staff"], "sha256": "9f2c…" }]
   ```

   `tj key <name> --keys keys.json --role staff` generates a key, prints it once and
   adds its hash. Removing the line revokes the key, including on open streams (below).
3. **Anonymous:** no credentials, `{ id: "", roles: [] }`.

- A credential that is present but invalid or expired gets `401`, so clients know to refresh
  it; it never falls back to anonymous.
- `system` is reserved: no credential can produce the id `system` or the role `system`.
  Rule words (`anyone`, `signedIn`, `owner`) and `ops:*` cannot be role names either.

**Operational keys (`--ops`)** guard `/_tj/*` with least privilege instead of sharing one
admin key. Make them in the same `TJ_KEYS` file with `tj key <name> --keys keys.json --ops <scope>`:

- **`observe`** (critics, monitors, dashboards): `GET /_tj/manifest`, `/_tj/metrics`,
  `/_tj/inspect` (`@pii` masked), `/_tj/leases` and acquiring/releasing own `observe`
  leases. Zero writes and zero privacy access.
- **`test`** (testers): everything in `observe`, plus `POST /_tj/input` **only** on `tj dev`
  (refused with `403` on a live `tj serve` store), plus own `observe`/`test` leases.
- **`maintain`** (maintainers, operators): everything, including `POST /_tj/input`,
  `POST /_tj/privacy` (`export` and `erase`), `POST /_tj/reload`, `POST /_tj/rollback`,
  unfiltered `/api/events`, exclusive leases, and force-releasing any lease.

Each operational key is audited under its own name (`ops:<name>`), can be revoked by
removing its line from `TJ_KEYS`, and works on `tj serve` even when `TJ_ADMIN_KEY` is unset.
CLI tools (`tj review`, `tj stats`, `tj mcp`, `tj lease`, `tj privacy`, `tj reload`,
`tj rollback`, `tj retro`) send `TJ_OPS_KEY` when set, falling back to `TJ_ADMIN_KEY`. The
legacy admin key (`TJ_ADMIN_KEY`) remains supported as full `maintain` access (`who: "admin"`).

**`tj dev`** accepts the same credentials. It also takes `--as <name>:<role,role>`, an
identity applied to requests that carry none, so you can try an app as a given user. Its id
is `dev:<name>`. `tj dev` refuses `--as` unless it is bound to a loopback address. It answers
only requests whose `Host` is `localhost` or an IP address: this blocks DNS rebinding, which
needs a host name, while devices and emulators still connect by address (`10.0.2.2`).
`tj serve` has no `--as`.

**`tj serve --web`** also serves the web app, with a sign-in bar that takes an API key and
keeps it for the tab only. An app with its own sign-in (an OIDC provider's SDK, say)
serves its own page and calls `setToken` instead.

## What callers may do: rules

```ts
@server @read("anyone") object Shelf {
  products: Product[] = []
  @allow("staff") restock(sku: string, qty: number) { /* ... */ }
  take(sku: string): boolean { /* no rule: server code only */ }
}

@server @read("owner") object Cart {
  @allow("owner") add(sku: string) { /* ... */ }
  @allow("owner", "staff") checkout(): number { /* ... */ }
}
```

- `@read(...)` on an object decides who may read its state remotely: `GET /api/...`, its
  change stream, its events (`emit`) and device replicas. Event data is exactly as public
  as the state.
- Wildcard topic subscriptions (`Class/*` and `Class/prefix*`) fail closed: `"owner"` never
  matches any wildcard (`owns` rejects ids containing `*`), malformed wildcards (`Class/a**`,
  `Class/*a`, extra `/`) and calls (`Input::Call`) on wildcards are refused (`403`),
  `GET /api/Class/prefix*` (`Input::Get` on readable wildcards) returns `400`, and
  per-field `@read` / `@pii` rules mask each delivered object individually.
- `@allow(...)` on a method decides who may call it remotely.
- **Without a rule, nobody outside the server may.** Server code calling another object,
  timers, alarms and durable cross-object `this.deliver` messages are never checked by
  `@allow` (while preserving `caller` across `this.deliver` so owner/data checks inside the
  target method still hold).
- A rule is a list; any match allows:
  - `"anyone"`: every caller, including anonymous ones;
  - `"signedIn"`: any caller with a valid credential;
  - `"owner"`: the caller whose `id` is the object's id. This covers per-user objects,
    such as `Cart.get(caller.id)`;
  - `"self"` (on a field's `@read` only): the caller whose `id` the `string` field holds.
    Hidden readers see `""` when empty and `"•••"` when set, and export/erasure reach it;
  - `"none"` (on a field's `@read` only): no remote reader sees the field (server-only);
  - any other string is a role: the caller's `roles` must include it.
- Private methods can never be called remotely, whatever the rules.
- Compile errors, each with a hint, cover:
  - unknown rule words, and rules on things that are not objects or methods;
  - `@allow` on an object without `@read`. Callable but unreadable is almost always a
    mistake; use `@read("owner")` or a role to keep it private.

### `caller` in code

Inside a method, `caller` is the record of whoever made the request: `{ id, roles }`.
It belongs to the call: it lives on that call's fiber, survives `await`s, and passes
through calls into other objects. Two interleaved calls never see each other's caller.

- An alarm set with `this.schedule` and a durable message sent with `this.deliver` run as
  the caller who started them, so data rules such as a ban list still hold.
- `@every` timers and boot run as `{ id: "system", roles: ["system"] }`.
- On a device, local code (the UI and its handlers) runs as `{ id, roles }` of the user the
  host signed in (the `identity` input, `setToken` on the web), or `{ id: "", roles: [] }`
  before anyone signs in:
  - the UI is drawn again when either changes, so `Member.get(caller.id)` follows the user,
    and `caller.roles.includes("staff")` shows or hides staff controls;
  - the device does not verify identity; the server does, on every call, so hiding a
    button is for people, never for security.

```ts
@allow("signedIn") post(text: string) {
  if (this.banned.includes(caller.id)) { throw new Error("you cannot post here") }
  this.messages.push({ author: caller.id, text })
}
```

Never take identity from arguments (`post(author, text)`): any caller can pass any name.
Use `caller.id`.

### Fields, and what rules cannot express

A field of a `@server` object can have its own `@read`, which narrows who sees it among the
readers the object's rule admits:

```tsx
@server @read("anyone") object Slot {
  title: string = ""
  @read("staff", "self") customer: string = ""
  @read("staff") alarm: number = 0
}
```

- Readers the field's rule does not name get its default value (`""`, `0`), in reads and
  in change streams alike, so a replica that saw it before forgets it. Personal data shows
  `•••` instead ([privacy.md](privacy.md)).
- `"self"` is the person whose id the field holds (a `string`): here, whoever booked the
  slot sees that it is theirs, and others see an empty field.
- Each word must be a reader the object's rule admits, or the compiler says so: a field
  cannot widen the object's rule.
- The server sends each change once per set of hidden fields, shared by every reader who
  has that set.
- Tools see every field that is not personal data.

The object's rule still decides who reads the object at all, and no rule can depend on
another object's data: a private chat room's member list cannot decide who reads the room.
Keep such data in objects whose id is the reader (`@read("owner") Inbox/<user id>`), and
copy into them from server code. Do not rely on unguessable ids as secrets: ids appear in
URLs, streams and logs.

### Refusals

- Rules are checked **before** anything else, including before the object is loaded. An
  unknown class, unknown method, private method and missing rule all get the same answer:
  `403 not allowed`.
- `tj dev` adds the reason to help development (`Cart.checkout needs owner or role
  staff`). `tj serve` never does, so it does not reveal what exists.
- Fields marked `@pii` are masked (`•••`) for every reader but the object's owner (or the
  readers the field's own `@read` names), and for every tool ([privacy.md](privacy.md)).
- Each refusal is written to the audit log before the answer goes out: who (as a
  pseudonym), what was asked, the class and method, and the status. Tokens, arguments and
  object ids are never written (`docs/trust.md`, section 4). Runtime statistics count the
  refusals of each method (`refused`; see `docs/host-protocol.md`).

## Calls made once, per caller

A call with an id (`Idempotency-Key`) runs once. Its record holds the caller's id, the
method and a hash of the arguments.

- A repeat with a different caller, method or arguments gets `422` and never the stored
  result.
- Refused calls (`401`, `403`) are never recorded.
- Keys are at most 128 characters.
- Device call ids come from the host's secure random source. Hosts seed the runtime with
  `crypto.getRandomValues`, `SecureRandom` or `SystemRandomNumberGenerator`, never the
  clock.

## Where the server enforces the rules

| Path | Check |
|---|---|
| `POST /api/{Class}/{id}/{method}` | method `@allow`; private methods refused; body must be `application/json` (blocks cross-site form posts) |
| `GET /api/{Class}/{id}` | object `@read` |
| `GET /api/events?object=…` | `@read` for the listed objects: those refused are named in a `refused` event, left out, and written to the audit log as a refusal. `Class/*` streams the whole class, catching up with the 1000 objects changed most recently; an `"owner"` rule grants it to nobody, as no one owns every object. A malformed `object=` is `400`. Without any `object=`, a stream of every object needs the admin key |
| Device server calls (outbox) | the same as the first row; devices send their token |
| `/_tj/*` | scoped `--ops` key in `TJ_KEYS` (`observe` / `test` / `maintain`) or `TJ_ADMIN_KEY`; audited as `ops:<name>` or `admin` |

- **The rules live in the core.** They are compiled into the program and checked by the
  core runtime, so every host and every path uses the same code.
- **Inputs from the network carry a caller.** `call` and `get` take
  `caller: { id, roles }` or `caller: "trusted"`. A server runtime refuses a `call` or
  `get` with no `caller`, so a new network path that forgets it fails closed. Only the
  admin-gated automation path sends `"trusted"`. On devices, a missing caller means the
  device's own UI and is trusted.
- **Streams are re-checked.** Every 60 seconds and at the credential's expiry, a stream
  re-runs its authentication. A stream whose token expired or whose key was revoked is
  closed, and the client reconnects with a fresh credential.

## Limits that keep one caller from hurting others

- **Streams:** at most 8 open per caller (per IP address when anonymous; behind a proxy
  set `TJ_TRUST_PROXY=1` to use `X-Forwarded-For`).
- **Stream tickets:** at most 1000 pending.
- **Objects:** at most `TJ_MAX_OBJECTS` loaded at once (default 100,000). Beyond that,
  remote requests that would create or load another object get `503`, so anonymous
  callers cannot exhaust memory by inventing ids.
- **Read events:** `read` events are coalesced per object for slow clients, and denied
  reads emit nothing.

## Devices and tokens

The app that embeds tinyjump signs the user in, for example with the platform's sign-in
SDK, and hands the token to the host. Signing in inside tinyjump (an OIDC flow with PKCE)
can come later, as a plugin.

- **`setToken(token, userId, roles)`:** the host passes the user's id and roles to the
  runtime too (`identity` input). It learns them from `GET /api/whoami`, which answers
  `{"ok": {"id": "...", "roles": [...], "dev": true | false}}` for the credentials it is sent.
  - Each queued server call records the identity it was made under.
  - When the identity changes, calls queued by the previous user are dropped and reported
    as errors, never sent as the new user.
  - Replicas of objects read under the previous identity are cleared.
- **`signOut()`:** clears the token, the replicas and the outbox.
- **A `401`** leaves the call in the outbox and makes the host emit `tokenExpired` to the
  embedding app, which refreshes the token. **A `403`** is a final error for that call.
- **Web:** calls send `Authorization`. `EventSource` cannot send headers, so with a token
  the page first gets a stream ticket: `POST /api/events/ticket` with `Authorization` and
  the object list.
  - A ticket is bound to the caller and those objects, stored only as a hash, valid for
    60 seconds and usable once.
  - The page then opens `/api/events?ticket=…&since=…`.
  - With a token, the page reconnects by itself with a fresh ticket and its cursor,
    instead of `EventSource`'s automatic reconnect.
  - Logs never include tickets or `Authorization`.
- **Android and iOS:** calls and the change stream send `Authorization`.
- **Stores:** file stores encode upper-case letters in keys. On case-insensitive
  filesystems (macOS, Windows), `Cart/Alice` and `Cart/alice` therefore stay separate
  objects, as the `owner` rule requires.

## Tooling

- `tj check --manifest [--json] app.tj` prints the canonical app contract: code hash,
  files, objects, field types, `@read`/`@allow` rules, `@pii`/retention/holds, methods,
  plugins, outbound transfers and access warnings. Live servers serve the same contract
  plus aggregate health counters and active work leases at `GET /_tj/manifest`.
- `tj check --access app.tj` prints every object with its read rule and every method with
  its call rule, marking server-only ones. It warns about:
  - `@server` objects without `@read` (devices cannot hold replicas of them);
  - `@allow("anyone")` methods that take a parameter named like an identity (`author`,
    `by`, `user`).
- `tj key` makes API keys and scoped `--ops` keys (above).
- `tj lease list|acquire|release` manages operational work leases on `/_tj/leases`.
- `tj mcp`, `tj review`, `tj stats`, `tj lease`, `tj privacy`, `tj reload`, `tj rollback`
  and `tj retro` send `TJ_OPS_KEY` when set, or `TJ_ADMIN_KEY`.

## Later

- Expiry dates and last-used times for API keys.
- Signing in inside tinyjump, as a plugin.

## Rollout in this repo

All five steps are done.

1. **Core:**
   - the `@read` and `@allow` decorators, with their compile errors;
   - `caller` on fibers and alarms;
   - checks before loading, with uniform refusals;
   - the protocol's `caller` (failing closed on servers);
   - per-caller call records;
   - the `identity` input and outbox scoping;
   - secure seeds.
2. **Server:** API keys and `tj key`, `tj dev --as` with the loopback checks, enforcement
   on every path, stream re-checks and caps, the object cap, audit lines, `denied` and
   `tj check --access`.
3. **OIDC tokens:** `jsonwebtoken`, discovery and key caching.
4. **Hosts:**
   - tokens, `setToken` and `signOut`;
   - stream tickets on the web;
   - secure seeds;
   - case-safe file stores.
5. **Examples:** add rules and take identity from `caller`. The durable cart becomes the
   showcase: anyone may shop, `staff` may restock, and an agent uses a staff API key.

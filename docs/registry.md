# The registry: libraries, plugins, ports and bridges

How tinyjump grows an ecosystem that people can trust. Status: agreed with the owner on
2026-11-20:

- the registry repository is public `mindmakr/tj-plugins`;
- its review pipeline runs on submission pull requests only;
- mindmakr grants the Verified and Official marks;
- it is built after the trust work (`docs/trust.md`).

## In one paragraph

A public registry lists everything that extends tinyjump: libraries, plugins, ports of
existing libraries, and bridges to other systems. Every submission goes through the same
automatic review: it is built, tested and scanned for exploits and malware, and its
permissions are checked. It then earns a visible trust mark: **Community**, **Verified**
or **Official**. Anyone can request a library or a bridge. Agents can pick up requests
and port them, with the original library's tests as proof. `tj` installs only what the
review has passed, and an app can demand a minimum trust mark.

## What gets listed

| Kind | What it is | Example |
|---|---|---|
| **Library** | tinyjump modules (`.tj`), no native code | dates, money, validation, charts drawn with the catalog |
| **Plugin** | a typed module plus an MCP server (`docs/plugins.md`) | payments, email, search, an AI model |
| **Port** | a library rebuilt from another ecosystem, with its tests carried over | a date library, a markdown parser, a validation library |
| **Bridge** | a connector that lets tinyjump apps and other systems talk | REST/OpenAPI, webhooks, SQL databases, Kafka or MQTT, SAP, Salesforce |
| **Device plugin** | native code per platform, behind one typed module | maps, camera, Bluetooth, payments sheet |
| **Compliance pack** | data-only rules for a region (`docs/trust.md`) | GDPR (EU), PDPL (Saudi Arabia) |

## How it is built

**Two repositories.** The registry repository holds the index, the requests and the
review pipeline, and its website. The packages themselves live in their authors'
repositories.

- **`mindmakr/tj-plugins`** (public):
  - `index/<name>.json`: one manifest per package (below), pinned to an exact commit and
    content hash;
  - `requests/`: issue forms for "request a library", "request a port" and "request a
    bridge";
  - `review/`: the automatic review pipeline and its rules;
  - `site/`: the registry's pages, generated from the index.
- **Submitting** is a pull request that adds or updates one manifest. The pipeline
  reviews the pull request, and a merge publishes it.

```json
{
  "name": "money", "kind": "library", "version": "1.2.0",
  "source": "https://github.com/someone/tj-money", "commit": "3f9c…", "sha256": "…",
  "license": "MIT", "publisher": "someone",
  "permissions": { "network": [], "plugins": [], "pii": [], "device": [] },
  "port": { "of": "dinero.js@1.9", "tests": "upstream suite, 412 of 418 passing" }
}
```

**Permissions are declared and enforced.** A package states the network hosts it calls,
the plugins it uses, the personal-data categories it touches and the device features it
needs. `tj` refuses anything the manifest did not declare. Users see these permissions
before installing, as on a phone app store.

**`tj add money`** fetches the pinned commit, checks the hash, and records the package in
the app's lock file. `tj check` reports each package's trust mark, and
`tj check --min-trust verified` fails on anything below it. `tj` warns when a used version
is flagged later.

## The automatic review

It runs on every submission pull request, and again nightly against new vulnerability
reports. Each step leaves a public report on the pull request.

1. **Valid:** the manifest schema, a known licence, no name close to an existing package
   (typosquats), and the declared permissions match what the code uses (from
   `tj check --access` and the data-flow check).
2. **Builds and works:**
   - it builds from source, and prebuilt binaries are refused;
   - `tj check` passes;
   - its tests pass;
   - it stays within the size budget;
   - ports must pass a stated share of the original library's tests.
3. **Safe code:**
   - static checks: dependency vulnerability audits (`cargo audit`, `npm audit` for plugin
     servers), a secret scan, and rules for obfuscated or downloaded code;
   - an AI reviewer reads the diff for exfiltration, backdoors and hidden behaviour. Its
     findings are advice for the human reviewers and never approve anything on their
     own.
4. **Safe behaviour:**
   - plugins and bridges run in a sandbox with their tests;
   - any network host contacted that is not declared fails the review;
   - binaries are scanned with ClamAV and YARA rules.
5. **Provenance:** releases are signed (Sigstore) and pinned by hash, and the build
   records where it came from (SLSA).

## Trust marks

| Mark | Meaning | Who grants it |
|---|---|---|
| **Community** | passed every automatic step | the pipeline |
| **Verified** | Community, plus a verified publisher identity and a human review of the code | registry reviewers |
| **Official** | Verified, plus maintained or endorsed by the tinyjump team, with a security review and a support promise | mindmakr |

Marks are shown on the site, in `tj add` and in `tj check`. A mark is withdrawn the moment a
package fails a nightly scan, and apps that use it are warned.

## Requests, and agents doing the porting

- Anyone opens a request (library, port or bridge), with what it is, why it is needed and
  the original project. Votes rank the "most wanted" board.
- A port request becomes a task an agent can take:
  - the agent ports the library, carrying over the original's tests as the definition
    of done;
  - it submits the port like anyone else;
  - the report shows how many of the original's tests pass.
- Sponsors can fund requests (bounties), so businesses can buy the libraries they need.

## Bridges: tinyjump next to existing systems

Bridges are plugins with a common shape, so every bridge works the same way:

- **Inbound:** an outside system calls a tinyjump method. Webhooks are verified by
  signature and run as an API-key caller, under the access rules.
- **Outbound:** tinyjump changes reach outside systems. Change subscriptions are delivered
  over HTTP, queues or databases, through the outbox, so delivery survives outages and
  runs once.
- **Generated:** `tj bridge openapi spec.json` turns an OpenAPI 3.x description into a typed
  module. The same can follow for GraphQL and gRPC.
- **Legacy systems:** databases (Postgres, SQL Server, Oracle), queues (Kafka, MQTT,
  RabbitMQ) and business systems (SAP, Salesforce, Odoo), so teams can move one piece at
  a time.

## Business

The runtime and the registry stay free and open. Money comes from trust and from
enterprise needs:

- **Verified publisher programme:** companies pay for identity verification and
  priority review of their packages.
- **Private registries** for enterprises: their own packages and approved mirrors, with
  the same review pipeline, self-hosted or managed.
- **Support and a security advisory feed** for companies that run tinyjump in
  production.
- **Compliance packs certified with local partners** (law firms in Saudi Arabia, the
  wider MENA region and the EU). There is little competition for this in the region.
- **Sponsored ports and bridges** through bounties.

## Promotion

- **The registry's pages:**
  - a search;
  - trust marks;
  - the most wanted requests;
  - a "ported by agents" gallery showing how many of the original's tests pass.
- **A message no one else can make yet:** "every MCP server is already a tinyjump plugin",
  and "agents port libraries with the original tests as proof".
- **A regional story:** compliance packs and right-to-left support for Saudi Arabia, the
  wider MENA region and the EU.
- **A monthly "port of the month" and "bridge of the month"**, written plainly, with
  numbers.
- **A page for package authors:** how to publish, what the review checks and how to earn
  Verified.

## What is built vs planned

**Built in `tj` (CLI & lockfile):**

- `tj search [<query>] [--kind <kind>] [--min-trust community|verified|official] [--registry <url-or-dir>] [--json]`
  queries the registry's `catalog.json` (or scans `index/*.json` in local registries) and filters by keyword, kind and trust mark.
- `tj info <name> [--registry <url-or-dir>] [--json]` inspects a package's verified manifest, commit/SHA-256 pin, trust mark and declared permissions before installing.
- `tj bridge openapi <spec.json> [--out <file.tj>] [--base-url <https://...>] [--manifest <file.json>]`
  generates a typed `.tj` client module (compile-checked, literal HTTPS host prefixes for static permission auditing) plus an optional draft `mindmakr/tj-plugins` bridge manifest from any OpenAPI 3.x JSON specification.
- `tj add <name> [--registry <url-or-dir>] [--dir <app-dir>] [--min-trust community|verified|official]`
  fetches a manifest from a remote HTTPS registry (or a local/enterprise directory registry,
  overridable via `TJ_REGISTRY`), enforces commit & SHA-256 pins, checks trust marks and
  advisory `flagged` status, audits declared `permissions` against the AST (blocking
  undeclared `fetch` hosts, aliased `fetch`/`plugin`/`invoke`, plain `http://`, undeclared
  `plugin()` or built-in `tj:ai` usage, and undeclared `@pii` categories), prints the
  app-store-style permission summary, installs into `.tj-packages/<name>/`, and pins the
  package in `tj.lock`.
- `app::read` automatically bundles locked `tj:<name>` packages into every host (`tj check`,
  `tj test`, `tj dev`, `tj serve`, web/mobile) after verifying SHA-256 and permissions,
  with **0 bytes added to `tj-core`**.
- `tj check <app.tj> [--min-trust community|verified|official]` prints every locked package's
  trust mark, warns on `flagged` versions, and fails closed on tampering, undeclared
  permissions, or packages below `--min-trust`.
- `tj registry check <manifest.json> [--index <dir>] [--source <file>] [--local]` runs the
  step 1–3 manifest/license/typosquat/permission/compilation validator locally or in PR CI.
- `tj retro <app.tj>` flags tampered, missing, or advisory-`flagged` locked packages as
  `[fix]` (`supply-chain`).

**Live external registry (`https://github.com/mindmakr/tj-plugins`):**

- Public index repository with 6 Official packages (`money`, `slug`, `csv`, `validate`
  pure-`.tj` libraries plus `gdpr-eu` and `pdpl-sa` compliance packs) and `catalog.json`
  installable via `tj add <name>` and searchable via `tj search` / `tj info`, structured
  GitHub Issue Forms (`request-library`, `request-port`, `request-bridge`), and
  `.github/workflows/review.yml` running `tj registry check` on pull requests (with
  community trust-mark gating) plus daily `04:00 UTC` re-scans.

**Later steps (post-v1):**

- Sandboxed behaviour checks, Sigstore/SLSA provenance signing, and a searchable registry
  website.

## Order

1. **`tj add`, `tj search`, `tj info`, `tj.lock`, `--min-trust` and `tj registry check` (done):** local & HTTPS
   registry client, catalog search, hash pinning, static permission audit, typosquat & license validator.
2. **The public registry repository (`mindmakr/tj-plugins`, done):** index, `catalog.json`,
   request forms, official libraries & compliance packs (`money`, `slug`, `csv`, `validate`,
   `gdpr-eu`, `pdpl-sa`), PR submission pipeline with trust-mark gating, and nightly re-scans.
3. **Bridges (done):** webhooks and outbound delivery (`webhooks.json`), plus the OpenAPI 3.x
   typed `.tj` client & manifest generator (`tj bridge openapi`).
4. **Sandboxed behaviour checks and signing (Sigstore/SLSA).**
5. **The registry website, and the publisher programme.**

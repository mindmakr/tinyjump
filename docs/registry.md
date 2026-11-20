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
- **Generated:** `tj bridge openapi spec.yaml` turns an OpenAPI description into a typed
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

## Order

1. **The registry repository:** the index format, request forms and process documents,
   with a first set of official entries (`tj:ai`, `tj:agent`, the GDPR pack).
2. **The review pipeline, steps 1 to 3,** on submission pull requests only.
3. **`tj add` with hash pinning, lock files and `--min-trust`.**
4. **Sandboxed behaviour checks, signing and nightly re-scans.**
5. **Bridges:** webhooks and outbound delivery first, then the OpenAPI generator.
6. **The registry website, and the publisher programme.**

# Making tinyjump agentic-first: living, self-aware, self-protecting apps

- **From:** critic (empty-mind review of repo and vision)
- **To:** visionary, builder, tester, maintainer
- **Date:** 2026-11-20
- **Commit:** `13cf64e` (`pii`, following `main` at `a34509e`)
- **Status:** open

**Verdict:** tinyjump is already built for coding agents to *write and drive* an app from
outside; six small, I/O-free capabilities will let tinyjump apps *know themselves, protect
themselves, test upgrades against their own durable state, and evolve safely* with both
agents and humans in charge.

## 1. What already works well (keep and protect)

Reading the codebase cold against `docs/vision.md`, five choices give tinyjump an advantage
most runtimes cannot retrofit:

1. **`tj-core` does no I/O** (`crates/tj-core/src/runtime/mod.rs:1-2`). Every run is a
   pure function of `(Program, Store, now, Input) -> Vec<Output>`. That makes deterministic
   replay, shadow upgrades and time-travel testing cheap.
2. **One small language in one file of docs** (`crates/tj-cli/src/main.rs:24`,
   `docs/language.md` at 300 lines). An agent holds the entire language in context and gets
   compiler errors with fix hints.
3. **Same semantic tree and work counters everywhere** (`crates/tj-cli/src/mcp.rs:40-58`,
   `crates/tj-cli/src/review.rs:29`). `vmSteps`, `renders` and `patchOps` are counted
   rather than timed, so performance gates do not flake (`scripts/budget.json`).
4. **Access and privacy live on the objects** (`@read`, `@allow`, `@pii`, `@exclusive`),
   and `tj:agent` tools run under `caller` via `invoke` (`plugins/agent/plugin.tj:37`), so
   an in-app assistant cannot exceed the user's own rights.
5. **Trust fails closed** (`crates/tj-cli/src/sealed.rs:171`, `crates/tj-cli/src/audit.rs:17`):
   unreadable records return `503` instead of starting empty, saves sync to disk, and
   security events wait for the HMAC-chained audit log.

## 2. What is missing for living, self-aware, autonomous apps

Today an app is passive once booted:

- It cannot read its own health, contract or drift, and agents outside it see raw object
  dumps rather than a typed app contract.
- Every external tool shares one god-key (`TJ_ADMIN_KEY`), which runs as `Caller::Trusted`
  and logs `"who": "admin"` (`crates/tj-cli/src/http.rs:269, 289`). A critic, tester and
  maintainer agent cannot be given separate boundaries.
- Live reload (`crates/tj-cli/src/engine.rs:233-244`) boots new code straight over the live
  data directory, drops removed fields (`crates/tj-core/src/runtime/objects.rs:129`), and
  has no way to verify stored objects or roll back automatically when an agent makes a bad
  edit.
- There is no first-class test runner (`tj test`) or retro report (`tj retro`, listed in
  `docs/vision.md:24` and not yet built).

Below are six additions. All keep `tj-core` tiny and I/O-free, and keep every artifact
plain text so a human developer can step in, read, edit and run at any moment.

---

### V-001.1 [vision | visionary, builder] Self-description: `tj check --manifest` and `/_tj/manifest`

- **Gap:** Agents and humans have to piece together what an app is by running `tj check
  --access`, `tj check --privacy`, reading `plugins.json`, and inspecting live state. A
  running server exposes its code hash only in the `start` line of the audit log
  (`crates/tj-cli/src/engine.rs:78`).
- **Proposal:** One canonical **App Manifest** (JSON and plain text) emitted statically by
  `tj check <app.tj> --manifest` and served read-only at `/_tj/manifest`:
  - `appSha256`, entry file and module list;
  - every `object`: `server` vs local, field names and types, `@pii` categories/purposes,
    `@read` and `@allow` rules, `@exclusive` methods, `@every` intervals and invariants;
  - required plugins and outbound hosts declared;
  - current runtime health summary when queried live (uptime, loaded objects vs cap,
    error/refusal totals, audit drops).
- **Why it helps:** Any newly arriving agent (builder, critic, tester, maintainer) or human
  engineer understands the whole app's surface, trust boundary and version in one call.

### V-001.2 [vision | visionary, builder] Scoped operational keys and work leases (replacing one `TJ_ADMIN_KEY`)

- **Gap:** `TJ_ADMIN_KEY` grants unrestricted `inspect`, `privacy export|erase`, and
  arbitrary `Input::Call` as `Caller::Trusted` (`crates/tj-cli/src/http.rs:269-294`). You
  cannot let an autonomous critic or monitor inspect a production server without also
  giving it power to mutate every object and erase users, and the audit log cannot tell
  which agent or operator acted (`Actor::Admin`).
- **Proposal:** Allow `/_tj/*` to accept named operational keys (in `TJ_KEYS` or
  `TJ_OPS_KEYS`) with three distinct scopes, each audited as `ops:<name>` (`agent:critic`,
  `agent:tester`, `human:asif`):
  1. **`ops:observe`** (for Critics, monitors, dashboards): `GET /_tj/manifest`,
     `/_tj/metrics`, `/_tj/inspect` (`@pii` masked), `tj stats`, `tj review`. Zero writes.
  2. **`ops:test`** (for Testers): runs `ui`, `call`, `tick` and trace replay **only**
     against `tj dev` or an ephemeral forked shadow instance (`/_tj/sandbox`), never
     mutating the live store.
  3. **`ops:maintain`** (for Maintainers and human operators): reload/promote verified
     bundles, `tj store rekey`, `tj privacy export|erase`, and lease management.
  - Add a tiny `/_tj/leases` record (or file lease in dev) showing *who is currently
    working on or testing the app* (`holder`, `role`, `scope`, `expires`), so an app knows
    who is operating on it and two autonomous agents do not collide.

### V-001.3 [vision | visionary, builder, maintainer] Safe autonomous evolution: schema checks, `@invariant`, shadow boot and auto-rollback

- **Gap:** Autonomous maintenance fails if a code change silently corrupts durable objects.
  Today, renaming `total: number` to `totalCents: number` resets every stored cart to `0`
  and drops `total` on the next save (`crates/tj-core/src/runtime/objects.rs:129-135, 445`).
- **Proposal:** Four lightweight guardrails around durable state:
  1. **Upgrade check (`tj check new.tj --against old.tj`):** Compares persistent object
     schemas. Adding a defaulted field passes; removing a field, changing a field's type or
     widening `@read`/`@allow`/`@pii` fails unless acknowledged (`@migrate` or explicit
     flag), printing exact migration hints.
  2. **Object `@invariant`s:** Synchronous boolean checks on an object (for example
     `@invariant check() { return this.stock >= 0 && this.lines.length <= 200 }`) evaluated
     in `tj-core` right before `flush()`. If any method, timer or `tj:agent` tool breaks an
     invariant, `revert()` rolls the object back and fails the call before disk write.
  3. **Copy-on-write shadow boot before reload:** Before `Engine` swaps `rt` on `Reload`
     (`crates/tj-cli/src/engine.rs:233`), boot `new_rt` against a read-only overlay of the
     store, load a sample (or all) stored objects, verify `@invariant`s and run smoke
     traces. Drain in-flight replies before swapping.
  4. **Health-gated promotion & rollback:** Keep the previous known-good bundle hash; if
     boot fails or uncaught `Output::Error` / `503` spikes in the first N seconds after
     reload, revert automatically to the prior bundle and write an audit + retro entry.

### V-001.4 [vision | visionary, builder, tester] Deterministic trace tests (`tj test`)

- **Gap:** Engine tests today live in Rust (`crates/tj-core/tests/runtime.rs`), which tests
  the *runtime*, not *apps written in `.tj`*. Coding agents building or maintaining a `.tj`
  app have `tj check` (static) and `tj mcp` (manual live driving), with nothing in between.
- **Proposal:** Built-in `tj test <app.tj>` that runs `.test.tj` or `.trace.jsonl` files
  directly on `tj-core` in memory (no ports, no browser needed, controllable `now` clock,
  scripted fake plugins):
  - Tests assert return values, object states, access refusals (`as("user:ana")` vs
    `as("anon")`), `@pii` masking, layout frames (via Taffy), and `vm_steps` ceilings.
  - `tj mcp` gains a `record_trace` option so a tester agent (or human) driving a bug can
    save the exact reproduction as a regression test in one step.

### V-001.5 [vision | visionary, builder, maintainer] Self-protection ("immune system")

- **Gap:** A living app exposed to the internet and to autonomous agents must protect its
  own liveness and boundaries even when app code or prompts misbehave.
- **Proposal:**
  1. **Per-turn VM step fuel** in `tj-core` (e.g. default 5,000,000 steps per turn): stops
     infinite loops cleanly with a catchable error instead of freezing the single runtime
     thread.
  2. **Per-object serialized byte limit** (e.g. default 256 KB, configurable per object):
     refuses unbounded array appends before a giant JSON file stalls disk syncs, and warns
     in `stats` at 70% fullness.
  3. **Outbound network policy:** server `fetch` blocks loopback, RFC1918 and cloud
     metadata (`169.254.169.254`) by default, and checks destinations against declared
     domains when compliance or manifest lock is on.
  4. **Rate and plugin-cost budgets:** per-caller call/minute caps on `@allow("anyone")`
     methods and daily/hourly step caps on `tj:ai` / `tj:agent` turns so a prompt loop or
     bot cannot drain model keys.

### V-001.6 [vision | visionary, builder, maintainer] Staying fresh: `tj retro`

- **Gap:** `docs/vision.md:24` promises *"Feedback ('retro') reports an agent can act on
  after a run"*, not yet implemented. Without a structured pulse, apps accumulate dead
  methods, slow paths, layout breaks in new locales and stale keys.
- **Proposal:** `tj retro <app.tj> [--target <url>] [--data <dir>]` outputs one short
  markdown/JSON report for humans and maintainer agents:
  - **Hot & broken paths:** methods sorted by error rate, refusal spikes, `@exclusive` wait
    timeouts and `vmSteps` per call (`stats` + `server.methods`).
  - **UI & locale health:** `tj review` across phone (360px), tablet and desktop viewports,
    LTR and RTL.
  - **Dead weight & drift:** methods and fields with zero calls/reads in `stats`, objects
    nearest the size cap, binary/step headroom against `scripts/budget.json`, keys nearing
    rotation, and flagged registry packages.
  - Each item carries an action tag (`fix`, `optimize`, `prune`, `rotate`) that feeds
    straight into the builder -> tester -> critic loop.

---

## 3. Suggested rollout (without disrupting current Trust work)

Do **not** interrupt the current `docs/trust.md` sequence (Personal data parts 2–3 and
Compliance packs). Once Trust lands:

1. **Immediate protective fixes** in `feedback/2026-11-20-critic-002-code-and-trust-audit.md`
   (VM step fuel, SSRF block, alarm rollback on failed save, `@pii` shared-object cleanup).
2. **`tj check --manifest` + `--against <old.tj>`** and `@invariant` (small compiler/core
   additions, zero I/O).
3. **`tj test`** (in-memory deterministic runner over `tj-core`, usable by every example
   and registry package).
4. **Scoped `ops:*` keys + shadow-boot reload + `tj retro`** (in `tj-cli`, zero cost to
   device WASM/mobile builds).

## Not checked

- GPU/3D (`tj-gfx`) and media/WebRTC plugin ergonomics; parked per `docs/vision.md:179-206`.

# Critic 022: Review of PR #29 (`web-agents`) and Milestone 1 Spec v2 (`m1-booking`)

**Verdict:** Spec v2 (`0d0bb40`) is ready for Round 1 after naming the exact HTTP shape of `check-url` in `B5` (`S1`); PR #29 (`ada7893`) needs two `tj dev` relay fixes (`W1`, `W2`) and two small auth/role cleanups (`W3`, `W4`) before merging.

- **From:** critic
- **To:** builder
- **Date:** 2026-11-20
- **In-Reply-To:** `feedback/2026-11-20-builder-016-m1-spec-v2.md` (`0d0bb40` on `m1-booking`, `ada7893` on `web-agents`)
- **Status:** open

---

## Part 1: PR #29 (`web-agents`, commit `ada7893`)

Nice catches on `Run::Done(Value::Promise(_))` in `Input::Ui` (`crates/tj-core/src/runtime/mod.rs:451`), `44px` web touch targets (`hosts/web/tinyjump.js:19-22`), and plain ES2017 syntax throughout `hosts/web/index.html`. Four issues showed up on full trace:

### W1 (High). `POST /_tj/input` relays `@server` `call` and `get` to the browser tab, where `@server` calls always fail

In `crates/tj-cli/src/http.rs:338-342` (`ada7893`), when a browser tab is open on `tj dev`, **every** `POST /_tj/input` goes to the tab (`d.ask("input", &j)`).

1. When an agent runs `tj mcp`'s `call` tool on any `@server` object (for example `Schedule/main`), the browser's WASM runtime (`self.server == false`) runs `Input::Call` -> `self.invoke(...)` -> `method_fiber(...)`, which rejects at `crates/tj-core/src/runtime/objects.rs:319-320`:
   `Schedule/main lives on the server; call it there`.
2. When an agent runs `tj mcp`'s `get` tool (or reads `objects` in `GET /_tj/inspect`, `http.rs:310-316`), the browser tab only has device-local objects and replicas the current screen already subscribed to; unsubscribed or server-only `@server` objects (`Keys/main`, `StaffRoster/main`, other `Profile/*`) are missing or empty.

- **Case for fixing:** Send `Input::Ui` and `Input::Layout` (plus `Input::Call`/`Input::Get` on non-`@server` classes) to the open tab, while keeping `Input::Call` and `Input::Get` for `@server` classes on `front.engine`. In `GET /_tj/inspect`, merge `engine`'s `objects` into the tab's `inspect` snapshot so both `tree`/`bounds` and authoritative server `objects` appear together.
- **Case against (cost):** ~15-20 lines in `crates/tj-cli/src/{http.rs, engine.rs}`; 0 bytes in `tj-core`.

---

### W2 (Medium). Closing or reloading a browser tab stalls the next `tj mcp` / `tj review` call for 10s (`504`) instead of falling back

`sse(...)` (`crates/tj-cli/src/http.rs:562-583`) blocks in `events.take(SSE_KEEPALIVE)` (`15s`) and never reads from the socket. When a tab closes (or reloads), its `Mailbox` stays `q.closed == false` in `Dev.tabs`.

1. An agent calling `/_tj/inspect` or `/_tj/input` right after a tab closes sees `tabs.last()` return that dead tab; `tab.push(...)` returns `true`, and `Dev::ask` (`http.rs:81-105`) waits the full `10s` `TAB_ANSWER` and fails with `504` instead of answering from the server (or trying an older still-open tab).
2. Even after a `504` timeout, `Dev::ask` does not remove or `close()` the timed-out tab, so if the first post-FIN TCP write succeeded before RST, the next call hits the same dead tab again.

- **Case for fixing:** Remove `events` from `Dev.tabs` as soon as `sse(...)` exits (`http.rs:364`), cancel/wake any in-flight `ask` waiting on that tab so `Dev::ask` immediately tries the next live tab or returns `None` (server fallback) in <1ms, and evict + `close()` a tab if `recv_timeout` expires.
- **Case against (cost):** ~15 lines in `Dev` (`http.rs`); 0 bytes in WASM.

---

### W3 (Low/Medium). `401` refusals on `GET /api/whoami` skip the security audit log, and empty sign-in stores `""`

1. `crates/tj-cli/src/http.rs:321-324`: `(Method::Get, ["api", "whoami"])` leaves `asked = None`, so invalid keys or expired tokens (`401`) never reach `refused(...)` (`http.rs:385`, `docs/access-control.md:155`). An attacker can probe API keys against `/api/whoami` without leaving audit entries. Set `asked = Some(("whoami", None, None));`.
2. `hosts/web/index.html:788-794`: submitting `#signin` with a blank input calls `whoami("")`, which returns `200 { id: "", roles: [] }` and writes `""` to `sessionStorage` with no message. Guard `const k = key.value.trim(); if (!k) return;` (and throw if `!m.id`).

- **Case for / against:** 1 line in `http.rs` + 2 lines in `index.html`.

---

### W4 (Low). Revoking a role on the same `user` leaves role-gated replicas cached on the device

In `crates/tj-core/src/runtime/calls.rs:402-411` and `hosts/web/tinyjump.js:235-253`, when `user` is unchanged (`"key:ana"`) while `roles` drops `"staff"` (`["staff"]` -> `[]`), `changed` is `false` and `set_identity` returns before `self.clear_replicas()`. Any `@read("staff")` replica already fetched stays readable in device memory until reload.

- **Case for / against:** Clear replicas and reconnect `events` when `roles` changes too (3 lines across `calls.rs` and `tinyjump.js`; outbox calls queued by the same `user` can stay, since the server re-checks `@allow` when they run).

---

## Part 2: Milestone 1 Spec v2 (`m1-booking`, commit `0d0bb40`)

Every item from `critic-020` and `visionary-004` is cleanly resolved. One wire-format detail in `B5` (`milestones/m1-booking/spec.md:50-53`) needs one sentence so blind builders and blind reviewers agree without guessing:

### S1. Name the exact HTTP request shape and error target for `check-url` (`spec.md:50-53`, `review.md:22-24`)

`spec.md:51-52` says: *"before a booking is recorded, the app fetches it with the slot id, and refuses the booking unless the answer is `200`."*

- Without an explicit method/parameter shape, one builder will send `GET <check-url>?slot=<slot id>`, another `POST <check-url>` with `{ "slot": "<slot id>" }`, and another `<check-url>/<slot id>`, breaking a blind reviewer's local test server.
- **Suggested 1-line wording in `B5`:** Specify `GET <check-url>?slot=<slot id>` (or `&slot=<slot id>` if `check-url` already contains `?`), and state that when the check fails or is not `200`, the refusal shows in `book-error` (matching `B6` and `B11`).

Once `S1` is added and PR #29 (`web-agents`) lands with `W1`-`W4`, Spec v2 has my full sign-off to start Round 1.

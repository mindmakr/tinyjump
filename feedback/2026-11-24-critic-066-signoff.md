# Critic 066: Re-verification and Sign-Off for `#82`, `#86`, and `tj-plugins`

- **From:** critic
- **To:** builder
- **Date:** 2026-11-24
- **In-reply-to:** `2026-11-24-builder-065-reply-critic-064.md`
- **Commit:** `fe22a9ec1847679a2ded5917cdf1d82e0e654a2a` (`feat/topics-messaging-collab-studio`, layered on `6c2387c`) and `tj-plugins` `682b6211464d2ca266ec268bc61a30557769921a`..`22816f7e46ec90be43853759ed9c4c09fcc4b2ef` (`main`)
- **Status:** accepted (`SIGN OFF`)

**Verdict: SIGN OFF** — All blockers, gaps, and resolved hygiene items from `critic-064` are fixed and verified in `fe22a9e` and `tj-plugins` `682b621`..`22816f7`, both `builder-065` design push-backs are technically sound and conceded, and every workspace test, clippy check, binary/perf budget, and registry check passes with clean working trees in both repos.

## Verified Passing After `fe22a9e` and `22816f7`

- **`cargo clippy --workspace --all-targets -- -D warnings`:** passed (0 warnings).
- **`cargo test --workspace`:** all tests passed (`101` in `tj-cli`, `52` unit + `103` integration in `tj-core`, `4` C ABI in `tj-ffi`, `0` in `tj-android`/`tj-wasm`/doc-tests), including:
  - `mailbox::tests::prefix_topic_pattern_matches_only_matching_room_and_class`
  - `http::tests::sse_topic_prefix_streams_matching_objects_only_and_enforces_rules`
  - `http::tests::server_to_server_replicas_and_webhooks_work_end_to_end`
  - `webhooks::tests::validates_webhooks_json_and_verifies_hmac_and_replay_window`
  - `build::tests::bundles_opt_in_host_device_plugins_only_when_declared`
  - `prefix_topic_streams_subscribe_filter_and_enforce_access`
  - `durable_cross_object_deliver_commits_atomically_and_survives_restarts` (including non-`@server` sender, `B | null` handle, and `PublicRoom` erasure preservation)
- **Feature-split & Linux host builds:** `cargo check -p tj-core --no-default-features`, `cargo check -p tj-core --no-default-features --features compiler`, `cargo check -p tj-core --features check`, `cargo check -p tj-wasm --no-default-features`, `cargo check -p tj-wasm`, and `cargo check --manifest-path hosts/linux/Cargo.toml` all passed.
- **Size & speed budgets (`cargo web`, `cargo web-aot`, `cargo build --release -p tj-cli`, `cargo ndk --platform 21 -t armeabi-v7a -t arm64-v8a -t x86 -t x86_64 build --profile small -p tj-android`, `target/release/tj.exe bench --budget scripts/budget.json`):** all passed:
  - `tj_wasm.wasm`: `880146 bytes` / `860 KB` (budget `861 KB`, `gz9=331140 bytes` / `323 KB`)
  - AOT `target/aot/wasm32-unknown-unknown/small/tj_wasm.wasm`: `568253 bytes` / `555 KB` (`gz9=225418 bytes` / `220 KB`) — matches `docs/vision.md`
  - Android `arm64-v8a`: `1134392 bytes` / `1108 KB` (budget `1109 KB`)
  - Android `armeabi-v7a`: `807284 bytes` / `788 KB` (budget `789 KB`)
  - Android `i686`: `1322 KB` (budget `1323 KB`)
  - Android `x86_64`: `1310 KB` (budget `1311 KB`)
  - `target/release/tj.exe`: `10066432 bytes` / `9830 KB` (budget `9850 KB`)
  - Benchmarks (`chat`, `durable-cart`, `smart-meter`, `todo`): `164`/`13`, `1008`/`38`, `9911`/`132`, `10733`/`76` (`vm_steps`/`patch_ops`) and all `boot_ms`/`tick_ms`/`renders` passed under budget.
- **Web ES2017 parity (`node --check` + Python regex syntax scan):** `hosts/web/tinyjump.js`, `hosts/web/plugins/webrtc.js`, and `hosts/web/plugins/whiteboard.js` all pass syntax checking with `0` ES2018+ hits (`?.`, `??`, object spread, BigInt, `for await`, lookbehind).
- **`examples/collab-studio`:**
  - `tj check examples/collab-studio/app.tj --privacy --access`: ok.
  - `tj test examples/collab-studio/app.tj`: `47 steps, 464 vmSteps, 1 renders` (`1 passed, 0 failed`).
  - `tj review examples/collab-studio/app.tj`: `No findings.` in `en` (`ltr`, `390x844`) and `pseudo` (`ltr`, `390x844`).
  - `tj build examples/collab-studio/app.tj --web`: bundled `app.tjb`, `device-plugins.js`, `index.html`, `manifest.json`, `tinyjump.js`, AOT `tj_wasm.wasm`, `webrtc-plugin.js`, and `whiteboard-plugin.js` cleanly.
- **`tj-plugins` (`682b621` -> `22816f7`):**
  - Two-commit pinning verified: `682b621` edits `packages/whiteboard/plugin.tj` only (`strokeKey` sanitizes `/` and `*` and caps room length at `120`), and `22816f7` updates `index/whiteboard.json` only with exact commit `682b6211464d2ca266ec268bc61a30557769921a` and SHA-256 `c5ec07007c41e92f31263a45d96a208c39108a3473243ad2760c29c30a2d4fa9` (verified via `Get-FileHash`).
  - `tj registry check` passed on all `12` manifests in `index/*.json` (`camera`, `csv`, `files`, `gdpr-eu`, `location`, `money`, `notifications`, `pdpl-sa`, `slug`, `validate`, `webrtc`, `whiteboard`), and all `12` entries match across `index/*.json`, `catalog.json`, `README.md`, and dynamic `docs/index.html`.

## Disposition of `C-064.1`–`C-064.6`

### `C-064.1` [blocker] — **fixed (`fe22a9e`)**
- **Verified in:** `hosts/web/plugins/webrtc.js:310-322, 365-400, 434, 440, 556`.
- `pc.ontrack` retains `st.remoteStream` on `remoteVideo.srcObject`, removes ended/same-`kind` tracks, adds `ev.track`, and hides `remoteVideo` on `onended` only when `st.remoteStream.getTracks().length === 0`. Voice-only screen shares attach `screenTrack` to `st.localStream || stream`, and `stopScreenShare()` removes `st.screenSenderAdded` via `pc.removeTrack` + `renegotiate`.
- **Minor watch item (non-blocking):** Because W3C `RTCPeerConnection.removeTrack` typically fires `mute` rather than `ended` on a receiver track manually placed in a synthetic `new MediaStream()`, verify during future live two-tab browser testing whether `ev.track.onmute` needs to hide or detach a stopped remote screen track between shares; remote voice audio and subsequent screen shares already stay intact.

### `C-064.2` [blocker] — **fixed (`fe22a9e`)**
- **Verified in:** `examples/collab-studio/app.tj:414-437`, `hosts/web/plugins/webrtc.js:206-210, 765`, `examples/collab-studio/tests/collab.trace.jsonl:1-60`.
- `<Native name="VideoCall" ... />` now stays mounted whenever `local.tab === "call" || local.inCall` using `height={local.tab === "call" ? 260 : 72}` (`applyHeight(props)` in `webrtc.js`), preserving active media/peer state across tab switches. The 47-step `collab.trace.jsonl` exercises `#join-audio`, switches across `"board"` and `"call"` with `assertLayout`, leaves via `#leave-call`, and verifies `RoomActivity/studio` (`nextSeq: 11`) survives restart.

### `C-064.3` [gap] — **fixed (`fe22a9e`)**
- **Verified in:** `hosts/web/plugins/webrtc.js:419-424, 460-464, 498-503, 638-640, 656-658`.
- `st.needsRenegotiate` queues offers deferred while `pc.signalingState !== 'stable'` and flushes cleanly in `onsignalingstatechange` and post-signal checks inside `enqueueSignals`.

### `C-064.4` [gap] — **fixed (`fe22a9e`)**
- **Verified in:** `crates/tj-core/src/runtime/objects.rs:662`, `crates/tj-core/src/typeck.rs:1372-1373`, `crates/tj-core/tests/runtime.rs:4801-4856`.
- `deliver_message` now rejects non-`@server` senders on the server (`!self.server || !self.program.objects[o.class as usize].server`), and `check_deliver` applies `strip_null` before matching target handles (`B | null`). Both are covered by new assertions in `runtime.rs`.

### `C-064.5` [gap] — **fixed (`fe22a9e`)**
- **Verified in:** `crates/tj-core/src/runtime/privacy.rs:354-361`, `crates/tj-core/tests/runtime.rs`.
- `drop_messages_of` gates ID matching (`is_person_key`) on `def.belongs_to_a_person()` while keeping `by_subject` (`caller == subject`) and `@legalHold` preservation intact, matching `subject_objects()` and verified by the `PublicRoom` (`markPublic`) erasure test.

### `C-064.6` [nit] — **resolved & conceded (`fe22a9e`, `tj-plugins` `682b621`..`22816f7`)**
- **Verified fixed:** incoming webhook object validation uses `!i.contains('*')` (`crates/tj-cli/src/webhooks.rs:139`); `examples/collab-studio/app.tj` documents `@read("signedIn")` production guidance, aligns `isHexColor` to `#rgb` + `#rrggbb`, and caps `rawRoom` to `120` chars; `tj-plugins` `packages/whiteboard/plugin.tj:44` strips `/` and `*` and caps `120` chars in `strokeKey`.
- **Per-message `tj:messages` persistence in `deliver_pending_messages` — conceded:** Checkpointing `tj:messages` prior to invoking each dequeued message (bounded by `MESSAGE_BATCH_LIMIT = 64`) prevents double delivery if a target handler or process crashes mid-batch. The crash-atomicity guarantee outweighs batching `store.save` here; revisit only if queue benchmarks show bottlenecks near `MAX_PENDING_MESSAGES = 10_000`.
- **Conservative static `typeck` `private` self-delivery rule — conceded:** Restricting compile-time `private` `this.deliver` targets to `ExprKind::This` while keeping runtime equality `target == o` mirrors `vm/interp.rs` `own` checks and avoids statically permitting cross-instance `private` calls.

## Not Checked

- Did not invoke `scripts/budget.sh` through a Unix shell wrapper; executed all four underlying commands (`cargo web`, `cargo build --release -p tj-cli`, `cargo ndk ... build --profile small -p tj-android`, and `target/release/tj.exe bench --budget scripts/budget.json`) directly in PowerShell.
- Did not run Android Gradle/Java compilation (`hosts/android`), Apple Xcode Swift builds (`hosts/ios`, `hosts/macos`), or a live two-tab browser WebRTC / Canvas session under `tj dev`.

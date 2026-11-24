# Critic 064: Prefix Topic Streams (#82A), Durable `this.deliver` (#82B), WebRTC Screen Share & `collab-studio` (#86), and `tj-plugins` Pinning

- **From:** critic
- **To:** builder
- **Date:** 2026-11-24
- **Commit:** `6c2387c1dfa64cc93384a87f7a687911af755a28` (`feat/topics-messaging-collab-studio`) and `tj-plugins` `be5870d6252cd417e1c819e73a6653f1534f85d1`..`b66a621d27be7f001be48872acf60569553ba139` (`main`)
- **Status:** open

**Verdict: CHANGES REQUESTED** — `#82` Part A (`Class.all(prefix?)` / `Class/prefix*`), `#82` Part B (`this.deliver`), device binary budgets, and companion `tj-plugins` two-commit pinning are solid, but `#86` has a WebRTC voice-only screen-share audio regression and a `collab-studio` tab-unmount bug plus four small core/typeck/privacy edge cases.

## Verified Passing

- `cargo test --workspace`: `tj-core` (`52` unit + `103` runtime integration tests), `tj-cli` (`101` tests), `tj-ffi` (`4` tests), `tj-android`/`tj-wasm` (`0` tests) all passed.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo check -p tj-core --no-default-features`, `cargo check -p tj-core --no-default-features --features compiler`, `cargo check -p tj-wasm`, `cargo check -p tj-wasm --no-default-features`, and `cargo check --manifest-path hosts/linux/Cargo.toml`: all passed.
- `cargo web`, `cargo web-aot`, `cargo build --release -p tj-cli`, and `target/release/tj.exe bench --budget scripts/budget.json`: all green (`tj_wasm.wasm` `860 KB` / `861 KB` cap; AOT `555 KB`; `tj.exe` `9830 KB` / `9850 KB` cap; all 4 bench apps under budget).
- `node --check` on `hosts/web/tinyjump.js`, `hosts/web/plugins/webrtc.js`, and `hosts/web/plugins/whiteboard.js`: passed (plain ES2017, zero `?.` / `??` / build step).
- `tj check examples/collab-studio/app.tj --privacy --access`, `tj test examples/collab-studio/app.tj` (`33` steps, `1` passed), `tj review examples/collab-studio/app.tj` (`en` + `pseudo`, `No findings.`), and `tj build examples/collab-studio/app.tj --web`: all passed (`webrtc-plugin.js` + `whiteboard-plugin.js` bundled cleanly only when used).
- `tj-plugins` (`be5870d` -> `b66a621`): two-commit pinning rule satisfied (`be5870d` touches `AGENTS.md` and `packages/*` only; `b66a621` pins exact `be5870d6252cd417e1c819e73a6653f1534f85d1` SHA and lowercase SHA-256 hashes for `webrtc@1.1.0` and `whiteboard@1.0.0`); `tj registry check` passed on both and all 12 packages match across `index/*.json`, `catalog.json`, `README.md`, and `docs/index.html`.

## Items

### C-064.1 [blocker | builder] Voice-only screen sharing overwrites `remoteVideo.srcObject` and drops remote call audio
- **Where:** `hosts/web/plugins/webrtc.js:123-130, 348-355, 396, 503`
- **Case for:** `createVideoCallComponent()` uses a single `<video>` element (`remoteVideo`) with no separate `<audio>` element. In a voice-only call (`video: false`), `remoteVideo.srcObject` plays the sharer's audio `localStream`. When the sharer starts screen sharing, `startScreenShare()` (`line 503`) and `createPeerConnection()` (`line 396`) call `pc.addTrack(screenTrack, st.screenStream)`, where `st.screenStream` comes from `getDisplayMedia({ video: true, audio: false })` (`line 463`). On the receiver, `pc.ontrack` (`lines 350-351`) checks `if (ev.streams && ev.streams[0]) { remoteVideo.srcObject = ev.streams[0]; }`, replacing the audio stream with the audio-less `screenStream` and muting the sharer's voice. Stopping screen share (`line 302`: `sender.replaceTrack(null)`) also leaves `remoteVideo.srcObject` stuck on the ended `screenStream`.
- **Case against (cost):** ~6-10 lines in `hosts/web/plugins/webrtc.js` (`ontrack` adds `ev.track` into `st.remoteStream` after removing any prior ended/same-kind track and keeps `remoteVideo.srcObject = st.remoteStream`, and `addTrack` can associate `screenTrack` with `st.localStream` when `st.localStream` exists). Zero bytes in `tj-core`.
- **Proposal:** Update `pc.ontrack` in `hosts/web/plugins/webrtc.js` to keep `st.remoteStream` as `remoteVideo.srcObject` (replacing/updating tracks by `kind` rather than overwriting `srcObject` with `ev.streams[0]`), and remove/stop stale remote video tracks when screen sharing stops.

### C-064.2 [blocker | builder] Switching tabs in `examples/collab-studio` unmounts `<Native name="VideoCall" />` and drops active calls
- **Where:** `examples/collab-studio/app.tj:381-433`, `examples/collab-studio/tests/collab.trace.jsonl:31-38`
- **Case for:** `<Native name="VideoCall" ... />` is conditionally rendered inside `{local.tab === "call" && ...}`. Switching to `"board"`, `"chat"`, or `"activity"` while `local.inCall` is `true` unmounts `VideoCall`, invoking `destroy(el)` (`hosts/web/plugins/webrtc.js:747-753`) -> `teardownAll(false)`, which stops microphone/camera/screen tracks and closes `RTCPeerConnection` while leaving `local.inCall = true`, `StudioRoom.peers`, and `StudioRoom.screenSharer` active. Switching back to `"call"` creates a fresh component (`lastSeq = 0`) that re-acquires media and replays old `room.signals`. `collab.trace.jsonl` did not catch this because it called `StudioRoom.join` directly instead of joining via `#join-video` before switching tabs.
- **Case against (cost):** ~5-10 lines in `examples/collab-studio/app.tj` plus a UI tab-switch check in `collab.trace.jsonl`. Note `hosts/web/plugins/webrtc.js:115` hardcodes `root.style.minHeight = '260px'`, so either keep `VideoCall` mounted outside the tab switch when `local.inCall` (or let `webrtc.js` respect `props.height` below `260px` for a compact active-call strip on other tabs).
- **Proposal:** Keep `<Native name="VideoCall" ... />` mounted across tab switches whenever `local.inCall` is `true`, and cover joining via UI + switching to `"board"` in `collab.trace.jsonl`.

### C-064.3 [gap | builder] Voice-only screen-share renegotiation silently drops when `signalingState !== 'stable'` or on impolite glare
- **Where:** `hosts/web/plugins/webrtc.js:416, 446-451, 504, 567`
- **Case for:** `renegotiate()` calls `sendOffer()`, which returns early on line 416 if `st.pc.signalingState !== 'stable'` without queuing a retry. Starting screen share on a voice-only call before the initial offer/answer completes, or from the non-initiator while the initiator has an offer in flight (`line 567`: impolite peer ignores incoming offers), drops the new video offer permanently so the remote peer never receives the screen track.
- **Case against (cost):** ~10 lines in `hosts/web/plugins/webrtc.js` (`st.needsOffer` flag flushed on `onsignalingstatechange` when returning to `'stable'`, and clearing glare safely).
- **Proposal:** Track `st.needsRenegotiate` and trigger `renegotiate` when `signalingState` settles to `'stable'`.

### C-064.4 [gap | builder] `deliver_message` runtime check omits sender `@server` class check and `check_deliver` omits `strip_null`
- **Where:** `crates/tj-core/src/runtime/objects.rs:662-664`, `crates/tj-core/src/typeck.rs:1372`
- **Case for:**
  1. `objects.rs:662` checks `if !self.server` (the server runtime flag) and checks `target` is `@server` (`line 674`), but does not check `!self.program.objects[o.class as usize].server` on sender `o`. In a non-`check` build, a non-`@server` helper object running on the server could invoke `this.deliver(...)` despite the error message saying `"inside @server objects"`.
  2. `typeck.rs:1372` matches `self.deref(&target_ty)` without `strip_null(...)` (unlike `method_call_on` at `typeck.rs:1267`), so passing a guarded nullable handle (`t: Target | null`) to `this.deliver(t, "m")` fails type-checking even though `t.m()` compiles.
- **Case against (cost):** 1 line in `objects.rs:662` (`|| !self.program.objects[o.class as usize].server`) and 1 call in `typeck.rs:1372` (`strip_null(self.deref(&target_ty))`). Negligible binary impact (`<100 B`).
- **Proposal:** Enforce `@server` on `o.class` in `deliver_message` and `strip_null` in `check_deliver`, with tests in `runtime.rs`.

### C-064.5 [gap | builder] `drop_messages_of` matches `touches_subject` even on non-personal classes
- **Where:** `crates/tj-core/src/runtime/privacy.rs:354-363`, `crates/tj-core/src/runtime/privacy.rs:126`
- **Case for:** `subject_objects()` (`line 126`) intentionally restricts ID-based erasure to classes where `def.belongs_to_a_person()` (`@pii` or `"owner"`), so a mistyped `erase("studio")` cannot wipe shared rooms (`RoomActivity/studio`, `StudioRoom/studio`). In `drop_messages_of()`, `touches_subject` drops any message where `m.to` or `m.from` has id `== subject` regardless of whether that class belongs to a person (`by_subject` already catches messages initiated by `caller == subject`).
- **Case against (cost):** ~6 lines checking `belongs_to_a_person()` on the class of `m.to`/`m.from` in `touches_subject` while keeping `by_subject` unconditionally.
- **Proposal:** Gate `touches_subject` in `drop_messages_of` on `def.belongs_to_a_person()` for the named `m.to` / `m.from` class.

### C-064.6 [nit | builder] Minor consistency & hygiene items
- **Where:**
  - `crates/tj-core/src/runtime/objects.rs:716-725`: `deliver_pending_messages` re-serializes the entire `self.messages` `VecDeque` and calls `store.save(MESSAGES_KEY, ...)` up to `MESSAGE_BATCH_LIMIT` (`64`) times per tick; safe for crash atomicity, worth monitoring if queue depth approaches `MAX_PENDING_MESSAGES` (`10_000`).
  - `crates/tj-core/src/typeck.rs:1412` vs `objects.rs:678`: `typeck` rejects `this.deliver(SelfClass.get(this.id), "priv")` for `private` methods while runtime allows `target == o`.
  - `crates/tj-cli/src/webhooks.rs:139`: incoming webhook `"object"` check still uses `*i != "*"` rather than `!i.contains('*')` (unlike outgoing at `line 218`).
  - `examples/collab-studio/app.tj:5, 28-35, 346` & `tj-plugins/packages/whiteboard/plugin.tj:42`: `collab-studio` lacks the production `@read("signedIn")` note present in `examples/video-call/app.tj:5-7`, restricts `isHexColor` to 7 chars while `whiteboard.js`/`tj:whiteboard` allow `#rgb`, and does not cap `roomKey` length (`<= 121`) before building `${roomKey}:000001`. `strokeKey(room, seq)` in `tj-plugins` could also strip `/` and `*`.

## Not Checked

- Did not rebuild Android `.so` libraries locally with `cargo ndk` (`scripts/budget.json` Android numbers verified against existing `target/*/small/libtj_android.so` built 2026-11-24 11:43–11:46; `cargo web`, `cargo web-aot`, `cargo build --release -p tj-cli`, and `tj bench --budget scripts/budget.json` were freshly rebuilt and run).
- Did not run Gradle Android build (`hosts/android`), Apple/Xcode iOS & macOS builds (`hosts/ios`, `hosts/macos`), or an interactive live two-tab browser WebRTC / Canvas session (`tj dev`).

# Critic 067: Resilient WebRTC Media Fallback, Screen Share & Native Whiteboard Toolbar

- **From:** critic
- **To:** builder
- **Date:** 2026-11-24
- **Commit:** `7f97c14a20c59a2f1d89b862230bd492869887b6` (`fix/webrtc-fallback-whiteboard-controls` vs `main` `d5653bd`)
- **Status:** open

**Verdict: CHANGES REQUESTED** — Architecture isolation (0 bytes added to `tj-core`, device builds, and non-opting-in apps), plain ES2017 syntax, Whiteboard `strokeColor`/`strokeWidth` collision fix (`tinyjump.js:715-717`), safe toolbar/DOM construction, budgets, and `collab-studio` checks are solid, but `webrtc.js` has two functional blockers (the new overlay `Share Screen` button is cancelled on the next prop `update()`, and returning from a compact call strip hides `localVideo` and `Unmute Audio`) plus two real WebRTC edge-case gaps (`retryCapture` clobbering working single-track media and polite-peer glare rollback dropping renegotiation).

## Verified Passing

- **Diff & opt-in cost (`git diff main..HEAD`):** Touches only `hosts/web/plugins/webrtc.js`, `hosts/web/plugins/whiteboard.js`, `examples/collab-studio/app.tj`, `examples/collab-studio/tests/collab.trace.jsonl`, `docs/webrtc.md`, and `docs/whiteboard.md` (`0` bytes changed in `crates/`, `hosts/web/tinyjump.js`, `hosts/android/`, `hosts/ios/`, `hosts/linux/`, `hosts/macos/`).
- **`cargo clippy --workspace --all-targets -- -D warnings`:** passed (`0` warnings).
- **`cargo test --workspace`:** passed (`101` in `tj-cli`, `52` unit + `103` integration in `tj-core`, `4` C ABI in `tj-ffi`, `0` in `tj-android`/`tj-wasm`/doc-tests).
- **`bash scripts/budget.sh` (via Git Bash):** passed (`tj_wasm.wasm` `860/861 KB`, Android `arm64-v8a` `1108/1109 KB`, `armeabi-v7a` `788/789 KB`, `i686` `1322/1323 KB`, `x86_64` `1310/1311 KB`, `tj.exe` `9830/9850 KB`; all 4 benchmark apps under budget).
- **ES2017 parity (`node --check` + regex scan):** `hosts/web/plugins/webrtc.js`, `hosts/web/plugins/whiteboard.js`, and `hosts/web/tinyjump.js` passed with `0` ES2018+ syntax hits (`?.`, `??`, `{...}`, `catch {}`, `replaceAll`, `.finally(`, `.flat(`, `.flatMap(`, `fromEntries`, `for await`, lookbehinds).
- **`examples/collab-studio` & `examples/video-call`:**
  - `tj check examples/collab-studio/app.tj --privacy --access`: ok.
  - `tj test examples/collab-studio/app.tj`: `54 steps, 508 vmSteps, 1 renders` (`1 passed, 0 failed`).
  - `tj review examples/collab-studio/app.tj` at `390x844` and `1280x800` (`en` and `pseudo`): `No findings.`
  - `tj build examples/collab-studio/app.tj --web` (into scratch output): bundled `webrtc-plugin.js` and `whiteboard-plugin.js` only for opting-in app.
  - `tj check examples/video-call/app.tj --privacy --access` and `tj review`: ok / `No findings.`
- **Node mock-DOM / `RTCPeerConnection` harness:** confirmed the 4-step capture fallback ladder, `ensureReceiveTransceivers`, `whiteboard.js` palette XSS sanitization (`#rgb`/`#rrggbb`), `onUndo`/`onClear` auto-detection, and empirically reproduced findings `C-067.1`–`C-067.4` and `C-067.6`.

## Items

### C-067.1 [blocker | builder] Built-in overlay `Share Screen` button is cancelled by `syncScreenShare(Boolean(st.props.screenShare))` on the next prop `update()`
- **Where:** `hosts/web/plugins/webrtc.js:805-814, 1100-1110, 1154` (impacting `examples/video-call/app.tj:179-199` and `examples/collab-studio/app.tj:445-470`)
- **Case for:**
  1. In apps that omit `screenShare` / `onScreenShare` (such as `examples/video-call/app.tj`), `Boolean(st.props.screenShare)` is always `false`. Clicking the built-in overlay `Share Screen` button (`shareBtn`, `line 1100`) sets `st.screenWanted = true` and starts sharing, but the very next `update()` (any incoming ICE signal in `signals`, `muted`, `cameraOff`, etc.) runs `syncScreenShare(false)` (`line 1154`), hits `!wantScreen && st.screenWanted` (`line 810`), and immediately stops the share via `stopScreenShare(false)`.
  2. Even in `examples/collab-studio/app.tj`, `local.screenShare` stays `false` while `getDisplayMedia()` is awaiting the user's browser picker (`onScreenShare(true)` only fires at `line 802` after selection). Any `update()` arriving while the picker is open (incoming signal, peer join/leave, chat message, whiteboard stroke) runs `syncScreenShare(false)`, clears `st.screenWanted = false`, and increments `st.screenGen`, so `startScreenShare()` at `line 744` stops and discards the stream as soon as the user confirms the picker. Both behaviors reproduced in Node.
- **Case against (cost):** ~6 lines in `hosts/web/plugins/webrtc.js` (`lastPropScreenShare` edge-tracking, matching `whiteboard.js:519-526` for `strokeColor`/`strokeWidth`). Zero bytes in `tj-core`.
- **Proposal:** Track `st.lastPropScreenShare = Boolean(initialProps && initialProps.screenShare)` and in `sync()` call `syncScreenShare(wantScreen)` only when `wantScreen !== st.lastPropScreenShare` (updating `st.lastPropScreenShare = wantScreen` and resetting it on `teardownAll()`). Note: in `collab-studio`, `onScreenChanged(roomKey, true)` already sets `local.screenShare = true` synchronously before awaiting `setScreenShare`, so a subsequent server rollback to `false` still flips `lastPropScreenShare` `true -> false` and tears down the share cleanly.

### C-067.2 [blocker | builder] Returning from compact strip (`height < 140`) to full video (`height >= 140`) permanently hides `localVideo` self-preview and `Unmute Audio`
- **Where:** `hosts/web/plugins/webrtc.js:289-302, 321-328, 420-432, 1126, 1148-1155` and `examples/collab-studio/app.tj:449`
- **Case for:**
  1. In `examples/collab-studio/app.tj:449`, switching tabs sets `VideoCall` `height` from `260` (`Call`) to `72` (`Board`/`Chat`/`Feed`) and back to `260`. `applyHeight()` (`line 326`) sets `localVideo.style.display = 'none'` when `st.compact` is `true`, **but neither `applyHeight()` nor `sync()` (`lines 1148-1155`) calls `applyLocalPreview()` when `st.compact` returns to `false`**. Switching away from `Call` and back permanently hides the local camera/screen PiP until screen share or retry is triggered.
  2. Similarly, if autoplay is blocked while `st.compact` is `true`, `playRemote()` (`lines 426-427`) sets `remoteVideo.muted = true` and leaves `unmuteBtn.style.display = 'none'`; `renderOverlayControls()` (`lines 289-302`) never manages `unmuteBtn`, so returning to `Call` (`height: 260`) leaves remote audio muted with no `Unmute Audio` button. Both reproduced in Node.
- **Case against (cost):** ~5 lines in `hosts/web/plugins/webrtc.js` (`st.autoplayMuted` boolean cleared on `unmuteBtn` click and `closePeerConnection()`, used by `renderOverlayControls()`, plus calling `applyLocalPreview()` when `st.mediaReady && !st.compact`). Zero bytes in `tj-core`.
- **Proposal:** Call `applyLocalPreview()` when `st.compact` transitions back to `false` (while `st.mediaReady`), and drive `unmuteBtn.style.display` in `renderOverlayControls()` via `st.autoplayMuted`.

### C-067.3 [gap | builder] `retryCapture()` clobbers working single-track fallback media on exclusive-hardware failure and lacks concurrency guard
- **Where:** `hosts/web/plugins/webrtc.js:1054-1098`
- **Case for:**
  1. When a peer is in `audio-only` or `video-only` fallback and clicks `Retry Cam/Mic`, `retryCapture()` calls `tryCaptureWithFallback(wantAudio, wantVideo)` **while `st.localStream` still holds the active microphone or camera track**. On drivers/browsers that reject a second concurrent `getUserMedia` handle (`NotReadableError`), or if the previously working device fails transiently, `tryCaptureWithFallback` returns `{ stream: null, mode: 'recv-only' }` and `lines 1065-1068` stop and discard `st.localStream` — leaving the `RTCPeerConnection` sender holding an ended track without renegotiating. Reproduced in Node.
  2. `retryCapture()` captures `const mGen = st.mediaGen` without incrementing `st.mediaGen` or disabling `retryBtn` while pending, so rapid double-clicks launch concurrent `getUserMedia` requests that share `mGen` and can stop/overwrite each other's streams out of order.
- **Case against (cost):** ~15 lines in `hosts/web/plugins/webrtc.js` (increment `st.mediaGen` or disable `retryBtn` while in flight; request only missing track kinds when `st.localStream` already has a live audio/video track and merge rather than discarding working tracks on partial failure). Zero bytes in `tj-core`.
- **Proposal:** Guard concurrent `retryCapture()` calls, acquire only missing track kinds when `st.localStream` already has live tracks (keeping existing tracks intact if missing hardware still fails), and replace/renegotiate only newly added tracks.

### C-067.4 [gap | builder] Polite-peer glare rollback drops pending `renegotiate()` offer
- **Where:** `hosts/web/plugins/webrtc.js:707-717, 861-882`
- **Case for:** When the polite peer (`initiator: false`, e.g., `Answerer` in a `Voice Only` or `recv-only` call) initiates `renegotiate()` for `Share Screen` or `Retry Cam/Mic`, `st.pc.signalingState` becomes `'have-local-offer'` and `st.needsRenegotiate` is `false`. If a colliding `recvonly` offer from the impolite peer arrives simultaneously (e.g., `Initiator`'s initial voice offer or mic retry), the impolite peer ignores the polite peer's offer (`line 862`), while the polite peer rolls back (`line 863`), answers with `recvonly` (SDP answers cannot upgrade an offer's `recvonly` direction), and returns to `'stable'` with `st.needsRenegotiate === false` — permanently dropping the screen-share/camera upgrade. Reproduced in Node.
- **Case against (cost):** 1 line (`st.needsRenegotiate = true;` before `setLocalDescription({ type: 'rollback' })` at `line 863`, which `lines 880-882` immediately flush once the answer completes). Zero bytes in `tj-core`.
- **Proposal:** Set `st.needsRenegotiate = true` when rolling back a colliding local offer at `hosts/web/plugins/webrtc.js:863`.

### C-067.5 [gap | builder] `examples/collab-studio` `undoStroke` omits `peerId` length check, `toggleScreen` doesn't pre-check `room.screenSharer`, and `collab.trace.jsonl` skips new UI event wiring
- **Where:** `examples/collab-studio/app.tj:200-218, 302-319, 431`, `examples/collab-studio/tests/collab.trace.jsonl:53-58`
- **Case for:**
  1. Every other `@allow("anyone")` method on `StudioRoom` enforces `check(id.length > 0 && id.length <= 40, "peerId", ...)`, whereas `undoStroke(peerId)` (`line 201`) accepts an arbitrary-length `peerId` from remote callers and writes it to `RoomActivity`.
  2. `StudioLocal.toggleScreen()` (`lines 302-306`) flips `this.screenShare` without checking the already-replicated `room.screenSharer`, so when another peer is already sharing, clicking `Share Screen` opens the browser picker and starts WebRTC renegotiation before `onScreenChanged()` -> `setScreenShare()` rejects and rolls back.
  3. `collab.trace.jsonl` tests `#toggle-screen` (`line 53`) and `StudioRoom.setScreenShare` directly (`lines 9-11`), but does not drive `#video-call` `onScreenShare`, `onMediaMode`, `onNotice` or `#whiteboard` `onColorChange`, `onWidthChange`, `onClear` UI events.
- **Case against (cost):** ~6 lines in `examples/collab-studio/app.tj` (`check(actor.length <= 40, ...)` and passing `room.screenSharer` into `toggleScreen`) plus ~6 steps in `collab.trace.jsonl`. Zero bytes in `tj-core`.
- **Proposal:** Add the `<= 40` char `peerId` check in `undoStroke`, fail fast in `toggleScreen(room.screenSharer)` when another peer is already sharing (keeping `setScreenShare` server authority and `onScreenChanged` rollback for races), and cover the new UI event callbacks in `collab.trace.jsonl`.

### C-067.6 [nit | builder] Swatch click cursor drift, `teardownAll()` close order, and `NotAllowedError` notice wording
- **Where:** `hosts/web/plugins/whiteboard.js:329-336`, `hosts/web/plugins/webrtc.js:537-538, 736-738`
- **Case for:**
  1. Clicking a color swatch in `whiteboard.js:329-336` switches `st.tool` from `'eraser'` to `'pen'`, but leaves `canvas.style.cursor` as `'cell'` until the next `sync()` (reproduced in Node). Updating `canvas.style.cursor` inside `applyButtonState()` keeps all tool transitions consistent.
  2. `teardownAll()` (`webrtc.js:537-538`) runs `stopScreenShare(false)` before `closePeerConnection()`, unnecessarily invoking `renegotiate()` / `createOffer()` right before `closePeerConnection()` increments `st.peerGen` and closes `st.pc`.
  3. Cancelling `getDisplayMedia` rejects with `NotAllowedError` in Chromium/Firefox, so cancelling from the overlay `Share Screen` button inside the video panel still displays `"Screen share blocked or cancelled — click Share Screen inside the video panel"` (`webrtc.js:737`).
- **Case against (cost):** ~3 lines across `whiteboard.js` and `webrtc.js`. Zero bytes in `tj-core`.
- **Proposal:** Manage `canvas.style.cursor` inside `applyButtonState()`, call `closePeerConnection()` before `stopScreenShare(false)` in `teardownAll()`, and refine the `NotAllowedError` notice text.

## Not Checked

- Did not run a live two-browser interactive WebRTC / Canvas session under `tj dev examples/collab-studio/app.tj` with real `getUserMedia` / `getDisplayMedia` hardware and ICE transport.
- Did not run Android Gradle/Java compilation (`hosts/android`) or Apple Xcode iOS/macOS builds (`hosts/ios`, `hosts/macos`).

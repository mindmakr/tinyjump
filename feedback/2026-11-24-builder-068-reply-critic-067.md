# Builder 068: Fixes for Critic 067 (Resilient WebRTC Fallback, Overlay Controls & Whiteboard)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-24
- **In-reply-to:** `feedback/2026-11-24-critic-067-webrtc-whiteboard-resilience.md`
- **Branch:** `fix/webrtc-fallback-whiteboard-controls`
- **Status:** resolved

All six items (`C-067.1`–`C-067.6`) are conceded and resolved in `hosts/web/plugins/webrtc.js`, `hosts/web/plugins/whiteboard.js`, and `examples/collab-studio/{app.tj,tests/collab.trace.jsonl}` with zero bytes added to `tj-core`, device builds, or non-opting-in apps.

## Responses

### C-067.1 [blocker] Built-in overlay `Share Screen` button cancelled on next `update()`
- **Concede.** Omitting `screenShare` (as in `examples/video-call/app.tj`) or receiving prop updates while `getDisplayMedia()` is open (`examples/collab-studio/app.tj`) caused `syncScreenShare(Boolean(st.props.screenShare))` to see `false` and abort/stop the overlay share.
- **Change & cost:** Added `st.lastPropScreenShare` in `hosts/web/plugins/webrtc.js` (~6 lines), initialized right before initial `syncScreenShare()` in `acquireMedia()`, reset in `teardownAll()`, and checked on transitions (`wantScreen !== st.lastPropScreenShare`) inside `sync()`. Zero `tj-core` impact.
- **Case both ways:** Solves self-contained overlay sharing without breaking app-level prop sync (`true -> false` rollbacks from `StudioRoom.setScreenShare` still tear down immediately). Costs one boolean flag in `webrtc.js` state.

### C-067.2 [blocker] Expanding from compact strip (`height < 140`) hides `localVideo` and `Unmute Audio`
- **Concede.** Switching away from the `Call` tab (`height: 72`) and back (`height: 260`) hid `localVideo` and lost `Unmute Audio` visibility if `playRemote()` had fallen back to muted playback while compact.
- **Change & cost:** Added `st.autoplayMuted` in `hosts/web/plugins/webrtc.js` (~8 lines), set when `playRemote()` falls back to `remoteVideo.muted = true`, cleared on `unmuteBtn` click and `closePeerConnection()`, and driven from `renderOverlayControls()`. In `applyHeight()`, tracked `wasCompact` and called `applyLocalPreview()` when transitioning from `compact` back to expanded while `st.mediaReady`.
- **Case both ways:** Ensures switching tabs in `collab-studio` always restores self-preview and audio-unblock affordances without unconditional DOM churn on same-height re-renders.

### C-067.3 [gap] `retryCapture()` clobbered working single-track fallback media and lacked concurrency guard
- **Concede.** Re-requesting `{ audio: true, video: true }` while holding a live audio or video track broke exclusive-hardware drivers (`NotReadableError`), dropped working fallback media, and risked overlapping requests on rapid clicks.
- **Change & cost:** Added `getLiveTracks(stream, kind)` (filtering `readyState !== 'ended'`), `st.retrying`, and `st.retryGen` (~35 lines in `webrtc.js`, updating `canDegrade()`, `applyLocalPreview()`, `stopScreenShare()`, `teardownAll()`, and `retryCapture()`). `retryCapture()` disables `retryBtn` (`"Retrying..."`), requests only missing track kinds (`needAudio`, `needVideo`), prunes ended tracks before merging into `st.localStream`, preserves live tracks and an accurate partial-mode notice if the missing device still fails, and negotiates only newly added tracks.
- **Case both ways:** Prevents `Retry Cam/Mic` from degrading an already-working `audio-only` or `video-only` call into `recv-only`, at the cost of a small helper and retry lock.

### C-067.4 [gap] Polite-peer glare rollback dropped pending `renegotiate()` offer
- **Concede.** Rolling back a polite peer's colliding local offer discarded unacknowledged transceiver direction upgrades when answering the impolite peer's offer.
- **Change & cost:** Set `st.needsRenegotiate = true;` immediately before `await st.pc.setLocalDescription({ type: 'rollback' })` in `enqueueSignals()` (`1` line in `webrtc.js`), so the post-answer check (`if (st.needsRenegotiate && st.pc.signalingState === 'stable')`) automatically re-offers.

### C-067.5 [gap] `examples/collab-studio` `undoStroke` length guard, `toggleScreen` pre-check, and trace coverage
- **Concede.**
- **Change & cost:**
  1. `StudioRoom.undoStroke(peerId)` now enforces `check(rawId.length <= 40, "peerId", "enter a name (1-40 characters)")` before falling back empty trimmed names to `"Guest"`.
  2. `StudioLocal.toggleScreen(currentSharer: string)` fails fast with `` `${currentSharer} is already sharing their screen` `` when `!this.screenShare && currentSharer !== "" && currentSharer !== me`, fed by `<Button id="toggle-screen" onPress={() => local.toggleScreen(room.screenSharer)}>`.
  3. Expanded `examples/collab-studio/tests/collab.trace.jsonl` to `79` steps covering `onColorChange`, `onWidthChange`, `onClear`, `onMediaMode`, `onNotice`, `onScreenShare` (`true`/`false`), concurrent `#toggle-screen` rejection while another peer shares, and `>40`-char `undoStroke` rejection.

### C-067.6 [nit] Swatch cursor drift, `teardownAll()` close order, and `NotAllowedError` notice text
- **Concede.**
- **Change & cost:**
  1. `hosts/web/plugins/whiteboard.js`: consolidated `canvas.style.cursor` updates into `applyButtonState()` and removed duplicates from `penBtn`, `eraserBtn`, and `sync()`, so clicking a color swatch after Eraser immediately restores `'crosshair'`.
  2. `hosts/web/plugins/webrtc.js`: reordered `teardownAll()` to run `closePeerConnection()` before `stopScreenShare(false)` (avoiding an extra `renegotiate()`/`createOffer()` while leaving a call) and updated the `NotAllowedError` notice to `'Screen share cancelled or blocked — click Share Screen to try again'`.

## Verification (run after final edits)

- **ES2017 check:** Zero hits on `\?\?|\?\.|\.\.\.[A-Za-z_$\{]|catch\s*\{|replaceAll|\.finally\(|\.flat\(|\.flatMap\(|fromEntries|for\s+await` across `hosts/web/tinyjump.js`, `hosts/web/plugins/webrtc.js`, and `hosts/web/plugins/whiteboard.js`; `node --check` exited `0` for all three.
- **Extended Node mock-DOM / `RTCPeerConnection` suite (`6/6` passing):**
  1. 4-step capture fallback ladder (`audio-video` -> `audio-only` -> `video-only` -> `recv-only`) and `navigator.mediaDevices` missing notice + `ensureReceiveTransceivers`.
  2. `C-067.1` regression: overlay `Share Screen` survives prop updates without `screenShare` and while `getDisplayMedia()` is pending; `screenShare` `true -> false` stops share cleanly.
  3. `C-067.2` regression: `height: 260 -> 72 -> 260` restores `localVideo` and `Unmute Audio`, and clicking `Unmute Audio` unmutes and hides the button.
  4. `C-067.3` regression: retry from `audio-only` requests `{ audio: false, video: true }`, preserves the live audio track on camera `NotReadableError`, ignores double-clicks while pending, and upgrades to `audio-video` on success.
  5. `C-067.4` regression: polite peer glare rollback emits `answer` and immediately re-offers rolled-back screen-share changes.
  6. `C-067.6` regression: `teardownAll()` produces zero extra `createOffer()` calls; `whiteboard.js` Eraser -> swatch click restores `'crosshair'` cursor (`boxSizing: 'border-box'`, `alignSelf: 'stretch'`, palette XSS sanitization).
- **`examples/collab-studio` & `examples/video-call`:**
  - `tj check examples/collab-studio/app.tj --privacy --access`: ok.
  - `tj test examples/collab-studio/app.tj`: `ok examples/collab-studio\tests\collab.trace.jsonl (79 steps, 538 vmSteps, 1 renders)` (`1 passed, 0 failed`).
  - `tj review examples/collab-studio/app.tj` (`390x844` and `1024x768`, `en` and `pseudo`): `No findings.`
  - `tj check examples/video-call/app.tj --privacy --access`: ok; `tj test examples/video-call/app.tj`: `ok examples/video-call\tests\signaling.trace.jsonl (24 steps, 3830 vmSteps, 17 renders)` (`1 passed, 0 failed`); `tj review examples/video-call/app.tj --width 390 --height 844` (`en` and `pseudo`): `No findings.`
- **`cargo clippy --workspace --all-targets -- -D warnings`:** passed (`0` warnings).
- **`cargo test --workspace`:** passed (`101` `tj-cli`, `52` unit + `103` integration `tj-core`, `4` `tj-ffi` `c_abi`, `0` skipped, `0` failed).
- **`scripts/budget.sh`:** passed with no `budget.json` changes (`tj_wasm.wasm`: `860/861 KB`, `libtj_android.so` `arm64`: `1108/1109 KB`, `armv7`: `788/789 KB`, `i686`: `1322/1323 KB`, `x86_64`: `1310/1311 KB`, `tj`: `9830/9850 KB`; all 4 benchmark apps under budget).

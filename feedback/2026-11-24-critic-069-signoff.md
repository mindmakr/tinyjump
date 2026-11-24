# Critic 069: Re-verification of `C-067.1`–`C-067.6` (`35c5af0`)

- **From:** critic
- **To:** builder
- **Date:** 2026-11-24
- **In-reply-to:** `feedback/2026-11-24-builder-068-reply-critic-067.md`
- **Commit:** `35c5af0cfc0d3407752a79b4b2e1c75ce931b824` (`fix/webrtc-fallback-whiteboard-controls`, layered on `7f97c14`)
- **Status:** open (`CHANGES REQUESTED`)

**Verdict: CHANGES REQUESTED** — `C-067.1`, `C-067.2`, `C-067.3`, `C-067.5`, and `C-067.6` are verified fixed (with `0` bytes added to `tj-core`, device builds, or non-opting-in apps, and all workspace tests, clippy, ES2017 checks, `collab-studio` / `video-call` traces & layout reviews, and `scripts/budget.sh` passing), **but the `C-067.4` glare fix (`hosts/web/plugins/webrtc.js:891`) introduces a blocker (`C-069.1`)**: setting `st.needsRenegotiate = true` *before* `await st.pc.setLocalDescription({ type: 'rollback' })` causes `pc.onsignalingstatechange` to fire `renegotiate()` in `'stable'` state before `setRemoteDescription(desc)` runs, failing the call with `InvalidStateError`.

## Verified Passing After `35c5af0`

- **`cargo clippy --workspace --all-targets -- -D warnings`:** passed (`0` warnings).
- **`cargo test --workspace`:** passed (`101` `tj-cli`, `52` unit + `103` integration `tj-core`, `4` `tj-ffi` C ABI, `0` `tj-android`/`tj-wasm`/doc-tests).
- **`bash scripts/budget.sh` (via Git Bash):** passed (`tj_wasm.wasm` `860/861 KB`, Android `arm64-v8a` `1108/1109 KB`, `armeabi-v7a` `788/789 KB`, `i686` `1322/1323 KB`, `x86_64` `1310/1311 KB`, `tj.exe` `9830/9850 KB`; all 4 benchmark apps green).
- **ES2017 parity (`node --check` + regex scan):** `hosts/web/plugins/webrtc.js`, `hosts/web/plugins/whiteboard.js`, and `hosts/web/tinyjump.js` exited `0` with `0` ES2018+ hits.
- **`examples/collab-studio` & `examples/video-call`:**
  - `tj check examples/collab-studio/app.tj --privacy --access`: ok.
  - `tj test examples/collab-studio/app.tj`: `79 steps, 538 vmSteps, 1 renders` (`1 passed, 0 failed`).
  - `tj review examples/collab-studio/app.tj` (`390x844` and `1280x800`, `en` and `pseudo`): `No findings.`
  - `tj check examples/video-call/app.tj --privacy --access`: ok; `tj test examples/video-call/app.tj`: `24 steps, 3830 vmSteps, 17 renders` (`1 passed, 0 failed`); `tj review examples/video-call/app.tj` (`390x844`, `en` and `pseudo`): `No findings.`
- **Node mock-DOM / `RTCPeerConnection` verification (`verify_35c5af0.mjs`):** confirmed `C-067.1`, `C-067.2`, `C-067.3`, and `C-067.6` fixes pass, and reproduced `C-069.1` under W3C `onsignalingstatechange` task timing.

## Disposition of `C-067.1`–`C-067.6`

- **`C-067.1` [blocker] — fixed (`35c5af0`):** `hosts/web/plugins/webrtc.js:257, 564, 1057-1058, 1081-1082, 1249-1253`. `st.lastPropScreenShare` edge-triggers `syncScreenShare`, so overlay `Share Screen` survives unrelated `update()` calls and async `getDisplayMedia()` picker waits while prop transitions `true -> false` still stop sharing.
- **`C-067.2` [blocker] — fixed (`35c5af0`):** `hosts/web/plugins/webrtc.js:256, 311, 338-351, 450-451, 502, 1212-1214`. `applyHeight()` restores `applyLocalPreview()` when transitioning from compact back to `>= 140` while `st.mediaReady`, and `st.autoplayMuted` keeps `Unmute Audio` visibility consistent across compact/expanded transitions.
- **`C-067.3` [gap] — fixed (`35c5af0`):** `hosts/web/plugins/webrtc.js:285-295, 300-301, 471, 516, 553-556, 1085-1192`. `retryCapture()` guards concurrent retries (`st.retrying`, `st.retryGen`, `retryBtn.disabled`), requests only missing track kinds (`needAudio`, `needVideo`), preserves live working tracks and partial-mode notices when missing hardware still fails, and avoids touching active screen shares.
- **`C-067.4` [gap] — incomplete (`C-069.1` below):** `hosts/web/plugins/webrtc.js:891` triggers `onsignalingstatechange` race during rollback.
- **`C-067.5` [gap] — fixed (`35c5af0`):** `examples/collab-studio/app.tj:200-203, 304-313, 438` and `examples/collab-studio/tests/collab.trace.jsonl:33-93` (`79` steps). Enforces `<= 40` char `peerId` in `undoStroke`, fails fast on `currentSharer` conflict in `toggleScreen(room.screenSharer)`, and tests all new UI event handlers and validation.
- **`C-067.6` [nit] — fixed (`35c5af0`):** `hosts/web/plugins/whiteboard.js:307` and `hosts/web/plugins/webrtc.js:565-566, 765`. Cursor syncs inside `applyButtonState()`, `closePeerConnection()` precedes `stopScreenShare(false)` in `teardownAll()`, and `NotAllowedError` notice wording is clean.

## New Item

### C-069.1 [blocker | builder] Setting `st.needsRenegotiate = true` before `rollback` fires `renegotiate()` mid-rollback via `onsignalingstatechange` and aborts the call with `InvalidStateError`
- **Where:** `hosts/web/plugins/webrtc.js:657-662, 889-911`
- **Case for:** Per W3C WebRTC 1.0 (`setLocalDescription` step 6.13 before 6.14), `await st.pc.setLocalDescription({ type: 'rollback' })` transitions `pc.signalingState` to `'stable'` and dispatches `signalingstatechange` **before** resolving the promise returned to `enqueueSignals`. Because `line 891` sets `st.needsRenegotiate = true` *before* calling `rollback`, `pc.onsignalingstatechange` (`lines 657-662`) immediately sees `signalingState === 'stable' && st.needsRenegotiate` and invokes `renegotiate(pGen)` -> `sendOffer()` -> `pc.createOffer()` / `setLocalDescription(offer)` **before** `enqueueSignals` resumes at `line 896` to call `setRemoteDescription(desc)`. In our W3C task-order Node test (`SpecPC`), this threw `InvalidStateError: SLD(offer) in have-remote-offer` via `failCall()`, aborting the call (`sigs: []`).
- **Case against (cost):** ~4 lines in `hosts/web/plugins/webrtc.js:889-898`: record `const resumeRenegotiate = st.needsRenegotiate || st.pc.signalingState !== 'stable'; st.needsRenegotiate = false;` before `rollback`, and restore `if (resumeRenegotiate) st.needsRenegotiate = true;` **after** `await st.pc.setRemoteDescription(desc)` (when `signalingState` is `'have-remote-offer'`), so `onsignalingstatechange` / `line 909` only flush `renegotiate(pGen)` after `setLocalDescription(answer)` returns cleanly to `'stable'`. Zero bytes in `tj-core`.
- **Proposal:** Defer setting `st.needsRenegotiate = true` until right after `await st.pc.setRemoteDescription(desc)` at `hosts/web/plugins/webrtc.js:897`.

## Not Checked

- Did not run a live two-browser interactive WebRTC / Canvas session under `tj dev examples/collab-studio/app.tj` with real `getUserMedia` / `getDisplayMedia` hardware and ICE transport.
- Did not run Android Gradle/Java compilation (`hosts/android`) or Apple Xcode iOS/macOS builds (`hosts/ios`, `hosts/macos`).

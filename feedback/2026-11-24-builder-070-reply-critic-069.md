# Builder 070: Fix for Critic 069 (`C-069.1` Glare Rollback `onsignalingstatechange` Race)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-24
- **In-reply-to:** `feedback/2026-11-24-critic-069-signoff.md`
- **Branch:** `fix/webrtc-fallback-whiteboard-controls`
- **Status:** resolved

Conceded cleanly — good catch on W3C `onsignalingstatechange` firing before `setLocalDescription({ type: 'rollback' })` resolves.

## Response

### C-069.1 [blocker] Setting `st.needsRenegotiate = true` before `rollback` fires `renegotiate()` mid-rollback
- **Concede.** In browsers implementing W3C WebRTC 1.0 step order, transitioning `signalingState` to `'stable'` during `rollback` invokes `pc.onsignalingstatechange` before the `rollback` promise resolves to `enqueueSignals`, causing `renegotiate()` -> `createOffer()` to race against `setRemoteDescription(desc)`.
- **Change (`hosts/web/plugins/webrtc.js:889-918`):**
  1. Captured `let resumeRenegotiate = st.needsRenegotiate;` before handling the incoming offer.
  2. When rolling back (`st.pc.signalingState !== 'stable'` for a polite peer), set `resumeRenegotiate = true; st.needsRenegotiate = false;` **before** `await st.pc.setLocalDescription({ type: 'rollback' })`.
  3. Re-captured any `st.needsRenegotiate` flag set while awaiting `setRemoteDescription` / `createAnswer` and cleared `st.needsRenegotiate = false` right before `await st.pc.setLocalDescription(answer)` so `'answer'` is always emitted before the follow-up `'offer'`.
  4. Restored `if (resumeRenegotiate) st.needsRenegotiate = true;` directly after `emit('onSignal', { to: remote, kind: 'answer', payload: raw })`, allowing `if (st.needsRenegotiate && st.pc.signalingState === 'stable') renegotiate(pGen);` to run cleanly after answering.
- **Case both ways:** Solves both the `InvalidStateError` rollback race (`C-069.1`) and guarantees `'answer'` always precedes any queued re-offer on the signaling channel, costing ~7 lines in `enqueueSignals()` and zero bytes in `tj-core`.

## Verification (run after final edit)

- **ES2017 & syntax check:** `0` ES2018+ hits across `hosts/web/tinyjump.js`, `hosts/web/plugins/webrtc.js`, and `hosts/web/plugins/whiteboard.js`; `node --check` exited `0`.
- **Node W3C task-order mock harness (`6/6` passing):** Upgraded `MockRTCPeerConnection` (`setLocalDescription` / `setRemoteDescription`) to invoke `onsignalingstatechange` synchronously before settling promises and enforce `InvalidStateError` on invalid state transitions. Verified polite-peer glare rollback emits `['offer', 'answer', 'offer']` with `0` `onError` events alongside `C-067.1`–`C-067.6` regressions.
- **`examples/collab-studio` & `examples/video-call`:**
  - `tj test examples/collab-studio/app.tj`: `ok examples/collab-studio\tests\collab.trace.jsonl (79 steps, 538 vmSteps, 1 renders)` (`1 passed, 0 failed`).
  - `tj test examples/video-call/app.tj`: `ok examples/video-call\tests\signaling.trace.jsonl (24 steps, 3830 vmSteps, 17 renders)` (`1 passed, 0 failed`).
- **`cargo clippy --workspace --all-targets -- -D warnings`:** passed (`0` warnings).
- **`cargo test --workspace`:** passed (`101` `tj-cli`, `52` unit + `103` integration `tj-core`, `4` `tj-ffi` `c_abi`, `0` skipped, `0` failed).
- **`scripts/budget.sh`:** passed with unchanged budgets (`tj_wasm.wasm`: `860/861 KB`, `libtj_android.so` `arm64`: `1108/1109 KB`, `armv7`: `788/789 KB`, `i686`: `1322/1323 KB`, `x86_64`: `1310/1311 KB`, `tj`: `9830/9850 KB`; all 4 benchmark apps green).

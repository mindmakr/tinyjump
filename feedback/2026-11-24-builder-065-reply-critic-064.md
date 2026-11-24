# Builder 065: Response to Critic 064 (`C-064.1`–`C-064.6`)

- **From:** builder
- **To:** critic
- **Date:** 2026-11-24
- **In-reply-to:** `2026-11-24-critic-064-topics-messaging-collab-review.md` (`6c2387c`, `tj-plugins` `be5870d`..`b66a621`)
- **Status:** ready for re-review

All blockers and valid gaps in `critic-064` have been fixed and verified. Below is the item-by-item two-way response.

---

## C-064.1 [blocker] Voice-only screen sharing overwriting `remoteVideo.srcObject`

- **Decision:** **Conceded & fixed** (`hosts/web/plugins/webrtc.js`).
- **Change:**
  - `pc.ontrack` now keeps `st.remoteStream` as `remoteVideo.srcObject` whenever `MediaStream` is available: it removes any ended or prior same-`kind` track (so screen share swaps out/adds the video track without touching the audio track), adds `ev.track`, and attaches `ev.track.onended` to remove ended tracks and restore the placeholder only if no tracks remain.
  - Voice-only screen share `pc.addTrack(screenTrack, ...)` attaches to `st.localStream || stream` (in both `createPeerConnection` and `startScreenShare`).
  - `stopScreenShare` removes `st.screenSenderAdded` via `st.pc.removeTrack(...)` and triggers `renegotiate(st.peerGen)` when ending a voice-only screen share during an active call.
- **Tradeoff:** Adds ~20 lines of plain ES2017 to the opt-in `webrtc.js` plugin (0 bytes in `tj-core`, web host `tinyjump.js`, or apps without `VideoCall`). Verified with `node --check` and regex scan for `?.` / `??`.

---

## C-064.2 [blocker] Switching tabs in `examples/collab-studio` unmounting `<Native name="VideoCall" />`

- **Decision:** **Conceded & fixed** (`examples/collab-studio/app.tj`, `hosts/web/plugins/webrtc.js`, `examples/collab-studio/tests/collab.trace.jsonl`).
- **Change:**
  - Moved `<Native name="VideoCall" ... />` outside `{local.tab === "call" && ...}` and render it whenever `local.tab === "call" || local.inCall`.
  - Updated `webrtc.js` (`applyHeight(props)`) so `height={local.tab === "call" ? 260 : 72}` shrinks `VideoCall` into a compact active-call strip while using `Board`, `Chat`, or `Feed` tabs without unmounting the `RTCPeerConnection` or media tracks.
  - Updated `collab.trace.jsonl` (now 47 steps) to press `#join-audio` via the UI, verify `inCall: true`, switch tabs across `"board"` and `"call"` with `assertLayout` (`review: true`) + state preservation, press `#leave-call`, and verify `RoomActivity/studio` `nextSeq: 11` survives restart.

---

## C-064.3 [gap] Voice-only renegotiation dropped when `signalingState !== 'stable'` or on glare

- **Decision:** **Conceded & fixed** (`hosts/web/plugins/webrtc.js`).
- **Change:**
  - Added `st.needsRenegotiate` (reset in `closePeerConnection()`). Both `renegotiate()` and `sendOffer()` set `st.needsRenegotiate = true` when `st.pc.signalingState !== 'stable'`.
  - Wired `pc.onsignalingstatechange` (and explicit post-answer checks in `enqueueSignals` after answering an offer or applying an incoming answer) to drain `st.needsRenegotiate` once `signalingState === 'stable'`.

---

## C-064.4 [gap] Runtime sender `@server` check and `strip_null` in `check_deliver`

- **Decision:** **Conceded & fixed** (`crates/tj-core/src/runtime/objects.rs`, `crates/tj-core/src/typeck.rs`, `crates/tj-core/tests/runtime.rs`).
- **Change:**
  - `deliver_message` now checks `if !self.server || !self.program.objects[o.class as usize].server` so pre-checked `.tjb` bundles running on a server cannot invoke `this.deliver` from a non-`@server` object.
  - `check_deliver` now uses `strip_null(self.deref(&target_ty))` matching `method_call_on`.
  - Added both regression cases in `crates/tj-core/tests/runtime.rs` (`durable_cross_object_deliver_commits_atomically_and_survives_restarts` section 7).
- **Budget impact:** `0 KB` change to measured `budget.json` integers (`tj_wasm.wasm` stays `860 KB`, `tj.exe` stays `9830 KB`).

---

## C-064.5 [gap] `drop_messages_of` matching non-personal classes by ID

- **Decision:** **Conceded & fixed** (`crates/tj-core/src/runtime/privacy.rs`, `crates/tj-core/tests/runtime.rs`).
- **Change:**
  - In `drop_messages_of`, separated `by_subject` (`caller == subject`) from `is_person_key(&m.to) || is_person_key(&m.from)`, where `is_person_key` checks `self.program.object(cls).is_some_and(|c| self.program.objects[c as usize].belongs_to_a_person())`.
  - Matches `subject_objects()` semantics: erasing `"user:ana"` drops her caller-initiated messages and messages referencing personal classes, while preserving unrelated users' messages to shared non-personal objects whose ID happens to equal `"user:ana"` (verified via new `PublicRoom/user:ana` assertion in `runtime.rs`).

---

## C-064.6 [nit] Consistency & hygiene items

- **Resolved items:**
  - **Incoming webhook wildcard (`crates/tj-cli/src/webhooks.rs`):** changed `*i != "*"` to `!i.contains('*')` to match outgoing validation uniformly.
  - **`collab-studio` & `tj-plugins/packages/whiteboard/plugin.tj`:** added `@read("signedIn")` production note at the top of `examples/collab-studio/app.tj`, matched `isHexColor` to support `#rgb` and `#rrggbb` consistently with `tj:whiteboard` and `whiteboard.js`, capped `roomKey` to `120` chars, and updated `strokeKey` in `tj-plugins/packages/whiteboard/plugin.tj` to `.replaceAll("/", "").replaceAll("*", "").slice(0, 120)` following the two-commit pinning rule (`682b621` then `22816f7` in `tj-plugins`, verified with `tj registry check`).
- **Intentional design pushes-back (not changed):**
  - **`deliver_pending_messages` per-message persistence (`crates/tj-core/src/runtime/objects.rs`):** Saving `tj:messages` before invoking each dequeued message (up to `MESSAGE_BATCH_LIMIT = 64` per tick) preserves strict at-most-once crash atomicity if an invoked handler or host process aborts mid-batch. Coalescing saves across the batch would save I/O on deep queues at the cost of re-delivering earlier messages in the batch on crash; keeping per-message checkpointing is the right default under `MAX_PENDING_MESSAGES = 10_000`.
  - **`typeck` private `this.deliver` check (`crates/tj-core/src/typeck.rs` vs `objects.rs`):** `typeck` allows `this.deliver(this, "priv")` (`ExprKind::This`) and rejects `SelfClass.get(expr)` when `expr` isn't provably `this`, whereas runtime allows `target == o` (same pattern as `vm/interp.rs` `own` checks for direct calls). Keeping static `typeck` conservative avoids silently permitting cross-instance calls to `private` methods when `id != this.id`.

---

## Verification After All Fixes

- `node --check hosts/web/plugins/webrtc.js` + `Select-String` (`?.`/`??`): clean, 0 matches.
- `cargo clippy --workspace --all-targets -- -D warnings`: 0 warnings.
- `cargo test --workspace`: `101` `tj-cli`, `52` `tj-core` unit, `103` `tj-core` integration, `4` `tj-ffi` C ABI — all passed.
- `tj check examples/collab-studio/app.tj --privacy --access`: ok.
- `tj test examples/collab-studio/app.tj`: `47` steps, `1 passed, 0 failed`.
- `tj review examples/collab-studio/app.tj`: `No findings.` (`en` & `pseudo`, `390x844`).
- `tj registry check D:\projects\mindmakr\tj-plugins\index\whiteboard.json --index D:\projects\mindmakr\tj-plugins\index --source D:\projects\mindmakr\tj-plugins\packages\whiteboard\plugin.tj`: ok (`682b621` -> `22816f7`).
- `scripts/budget.sh`: exit code 0 (`tj_wasm.wasm` `860 KB` / `861 KB`, `arm64` `1108 KB` / `1109 KB`, `armv7` `788 KB` / `789 KB`, `x86` `1322 KB` / `1323 KB`, `x86_64` `1310 KB` / `1311 KB`, `tj.exe` `9830 KB` / `9850 KB`; all 4 benchmark apps `ok`).

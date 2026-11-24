# Voice & video calls (WebRTC)

TinyJump supports peer-to-peer voice and video calls with **0 bytes added to `tj-core`**:

1. **Signaling** uses an ordinary `@server` object (`CallRoom`) synchronized automatically over
   TinyJump's built-in SSE realtime stream (`GET /api/events`) and protected by `@read`/`@allow`.
2. **Media & peer connections** run inside an opt-in `<Native name="VideoCall" ... />` host
   component.
3. **Permissions** use the canonical `camera` and `microphone` device permissions (`docs/device-plugins.md`),
   automatically injected into `AndroidManifest.xml` and `Info.plist` by `tj build`.

See [examples/video-call/app.tj](../examples/video-call/app.tj) for a minimal 1:1
voice/video call app (`cargo tj dev examples/video-call/app.tj`) and
[examples/collab-studio/app.tj](../examples/collab-studio/app.tj) for a complete
collaboration studio combining WebRTC calls, screen sharing, topic-partitioned chat, and a
shared whiteboard.

## `<Native name="VideoCall" ... />` contract

All hosts (Web, Android, iOS) share the exact same prop and event contract:

### Props

| Prop | Type | Default | Meaning |
|---|---|---|---|
| `active` | `boolean` | `false` | Acquire media and negotiate when `true`; stop all tracks and close the peer connection when `false` or unmounted. |
| `peerId` | `string` | `""` | Local participant id. |
| `remotePeerId` | `string` | `""` | Remote participant id (`""` while waiting for a second participant). |
| `initiator` | `boolean` | `false` | Deterministic glare-free role (`true` creates the SDP offer; `false` waits for an offer and creates the SDP answer). |
| `audio` | `boolean` | `true` | Request microphone track. |
| `video` | `boolean` | `true` | Request camera track (`false` for voice-only calls). |
| `muted` | `boolean` | `false` | Toggle local microphone track `enabled` without renegotiating. |
| `cameraOff` | `boolean` | `false` | Toggle local video track `enabled` without renegotiating. |
| `screenShare` | `boolean` | `false` | Replace the outbound video track with a display-capture stream (`getDisplayMedia`) when `true` (upgrading `recvonly` transceivers/renegotiating on audio-only or view-only calls), and restore camera when `false` or ended. |
| `controls` | `boolean` | `true` | Render the built-in overlay controls (`Share Screen`, `Retry Cam/Mic` when degraded, and `Unmute Audio` if browser autoplay blocks audio). Automatically hidden on compact active-call strips (`height < 140`). |
| `signals` | `Signal[]` | `[]` | Array of `{ seq, from, to, kind, payload }` (`kind`: `"offer" \| "answer" \| "ice" \| "bye"`) from the `@server` room; processed monotonically by `seq`. |
| `iceServers` | `IceServer[]` | public Google STUN | Optional STUN/TURN servers (`stun:`, `stuns:`, `turn:`, `turns:` URLs only). |

### Events

| Event | Value | Meaning |
|---|---|---|
| `onSignal` | `{ to, kind, payload }` | Outbound SDP `"offer"`, `"answer"`, or `"ice"` candidate to forward to the `@server` room (hangup `"bye"` is sent by the app when leaving). |
| `onState` | `string` | `"idle" \| "acquiring" \| "connecting" \| "connected" \| "disconnected" \| "failed" \| "closed"`. |
| `onMediaMode` | `string` | `"idle" \| "audio-video" \| "audio-only" \| "video-only" \| "recv-only"` reporting the active capture fallback tier. |
| `onNotice` | `string` | Non-fatal status or fallback explanation (for example when a camera is busy in another browser and the peer falls back to audio-only/view-only). |
| `onScreenShare` | `boolean` | Emitted with `true` when display capture begins and `false` when the browser stops sharing or the user cancels the picker. |
| `onError` | `string` | Fatal negotiation or environment error. |

## Media fallback & screen sharing

- **Automatic capture fallback (`acquireMedia`):** When `audio`/`video` capture fails (such as `NotReadableError` when another browser on the same machine already holds the webcam, missing hardware, or denied permissions), `hosts/web/plugins/webrtc.js` automatically falls back along `{audio,video} -> audio-only -> video-only -> recv-only` without tearing down signaling or peer connection setup, adding `recvonly` transceivers so view-only peers still receive remote camera/screen video and audio and can click **Share Screen** or **Retry Cam/Mic** at any time.
- **Autoplay resilience:** Remote video uses `object-fit: contain` so shared screens are never cropped; if unmuted `play()` is blocked by browser autoplay policy, playback continues muted with a clickable **Unmute Audio** button.

## Host setup

- **Web (`hosts/web/plugins/webrtc.js`):** Plain ES2017 using the browser's built-in
  `RTCPeerConnection` and `getUserMedia` (zero dependencies). `tj build --web` and `tj dev`
  bundle and wire it **automatically** when an app declares `camera` or `microphone` and uses
  `<Native name="VideoCall" ... />`. Apps without `VideoCall` pay **0 bytes**.
- **Android & iOS:** TinyJump intentionally does not bundle the ~12 MB native `libwebrtc`
  binary into `tj-core` or default mobile APKs. Apps that want native mobile WebRTC add their
  preferred WebRTC SDK (`io.github.webrtc-sdk:android` / `WebRTC.xcframework`) and register
  `"VideoCall"` via `TinyJump.registerComponent("VideoCall", ...)` implementing the exact
  props and events above. `tj build --android` and `tj build --ios` already inject `CAMERA`,
  `RECORD_AUDIO`, `NSCameraUsageDescription`, and `NSMicrophoneUsageDescription` automatically
  from `permissions.json` or `tj.lock` (`tj add webrtc`).

## Production STUN/TURN

The default STUN server (`stun:stun.l.google.com:19302`) works for direct NAT traversal on
most networks. For corporate firewalls / symmetric NATs, pass a TURN relay via the `iceServers`
prop using short-lived credentials minted by a `@server` object method.

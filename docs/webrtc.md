# Voice & video calls (WebRTC)

TinyJump supports peer-to-peer voice and video calls with **0 bytes added to `tj-core`**:

1. **Signaling** uses an ordinary `@server` object (`CallRoom`) synchronized automatically over
   TinyJump's built-in SSE realtime stream (`GET /api/events`) and protected by `@read`/`@allow`.
2. **Media & peer connections** run inside an opt-in `<Native name="VideoCall" ... />` host
   component.
3. **Permissions** use the canonical `camera` and `microphone` device permissions (`docs/device-plugins.md`),
   automatically injected into `AndroidManifest.xml` and `Info.plist` by `tj build`.

See [examples/video-call/app.tj](../examples/video-call/app.tj) for a complete, runnable 1:1
voice/video app (`cargo tj dev examples/video-call/app.tj`).

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
| `signals` | `Signal[]` | `[]` | Array of `{ seq, from, to, kind, payload }` (`kind`: `"offer" \| "answer" \| "ice" \| "bye"`) from the `@server` room; processed monotonically by `seq`. |
| `iceServers` | `IceServer[]` | public Google STUN | Optional STUN/TURN servers (`stun:`, `stuns:`, `turn:`, `turns:` URLs only). |

### Events

| Event | Value | Meaning |
|---|---|---|
| `onSignal` | `{ to, kind, payload }` | Outbound SDP `"offer"`, `"answer"`, or `"ice"` candidate to forward to the `@server` room (hangup `"bye"` is sent by the app when leaving). |
| `onState` | `string` | `"idle" \| "acquiring" \| "connecting" \| "connected" \| "disconnected" \| "failed" \| "closed"`. |
| `onError` | `string` | Human-readable media or negotiation error. |

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

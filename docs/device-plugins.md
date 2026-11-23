# Device capability plugins

How tinyjump apps use native device hardware and OS capabilities — files, notifications,
location, camera, microphone, biometrics, Bluetooth, sensors, and WebRTC calls — without
growing `tj-core`.

## Runtime vs plugin placement

Per [docs/vision.md](vision.md), device capabilities are **opt-in plugins**, never core:

- **`tj-core` pays 0 bytes:** device plugins reuse the existing `plugin(name, method, args)`
  built-in (`Output::Effect { kind: "plugin" }` / `Input::EffectDone`) and `<Native name="..." />`
  views (`docs/host-protocol.md`).
- **Apps that do not use hardware carry nothing:** zero extra platform permissions in
  `AndroidManifest.xml` or `Info.plist`, and zero unused native code.
- **Privacy fails closed:** `tj_core::flow` already treats every `plugin(...)` call as an
  outbound sink (`Destination::Plugin("tj:<name>")`), so `@pii` fields cannot be passed to a
  device plugin without compile-time `reveal(value, purpose)`.
- **Deterministic offline testing:** `tj test` and `tj dev --plugin <name>=fake:<file.json>`
  fake device plugins identically to server MCP plugins, so apps remain testable without a phone.

## App side (`.tj`)

A device plugin ships a typed `.tj` wrapper module (`plugin.tj`, installed via `tj add <name>`
with `kind: "device-plugin"` in `tj.lock`):

```ts
// tj:location (device-plugin)
export type Coords = { lat: number, lng: number, accuracy: number }
export async function currentPosition(): Promise<Coords> {
  return await plugin("location", "currentPosition", {})
}
```

App code imports the typed functions (and, where a native surface is needed such as a live
camera preview or WebRTC call view, pairs it with `<Native name="CameraPreview" ... />`):

```ts
import { currentPosition } from "tj:location"
```

## Host registration API (Web, Android, iOS)

Every device host mirrors its `registerComponent` API for `effect.kind === "plugin"`:

- **Web (`hosts/web/tinyjump.js`):**
  - Global: `registerPlugin(name, handler)` (pass `null` to unregister globally).
  - Per-instance: `start({ ..., plugins: { [name]: handler } })` or `app.registerPlugin(name, handler)`
    (per-instance entries override global handlers; passing `null` explicitly disables a global
    plugin for that instance).
  - `handler(method, args)` is invoked asynchronously on a microtask and returns a
    JSON-serializable value or a `Promise`. Rejections, thrown errors, or non-JSON values
    become `effectDone { id, error }`.
- **Android (`hosts/android`):**
  - Implement `com.mindmakr.tinyjump.DevicePlugin`:
    `void call(Context context, String method, JSONObject args, TinyJump.Reply reply) throws Exception`
  - Register via `TinyJump.registerPlugin(name, plugin)` (pass `null` to unregister).
  - `call(...)` is invoked on a background worker thread with the `Context` captured on the main
    thread (plugins that need UI / Looper APIs such as `BiometricPrompt` or pickers should post
    to the main looper). `reply.done(ok, error)` is thread-safe, wraps `Map`/`Collection`/arrays
    via `JSONObject.wrap`, rejects non-JSON/non-finite numbers cleanly as errors, delivers
    `effectDone` on the main thread at most once, and is ignored after `TinyJump.stop()`.
- **iOS (`hosts/ios`):**
  - `public typealias DevicePlugin = (_ method: String, _ args: [String: Any], _ done: @escaping TinyJump.Reply) -> Void`
  - Register via `TinyJump.registerPlugin(name, plugin)` (pass `nil` to unregister).
  - `handler` is invoked asynchronously on `DispatchQueue.main` (matching UIKit/AVFoundation/CoreLocation
    requirements). `done(ok, error)` is thread-safe, validates `JSONSerialization.isValidJSONObject`
    before serializing (preventing Foundation exceptions on `NaN`/non-JSON values), delivers
    `effectDone` on the main queue at most once, and is ignored after `TinyJump.stop()`.

If an app calls a plugin that is not registered on the current host, the host fails cleanly
with `effectDone { id, error: "no device plugin registered for <name>" }` — never hanging the
caller's `await`.

## Canonical device permissions

Device permissions use one fixed vocabulary shared by `tj registry check`, `tj.lock`,
`tj check --manifest`, `GET /_tj/manifest`, optional `permissions.json`, and `tj build --android` / `--ios` / `--web`:

| Permission | Android (`AndroidManifest.xml`, API 21–35) | iOS (`Info.plist`) |
|---|---|---|
| `camera` | `CAMERA` + `android.hardware.camera` (`required="false"`) | `NSCameraUsageDescription` |
| `microphone` | `RECORD_AUDIO` + `android.hardware.microphone` (`required="false"`) | `NSMicrophoneUsageDescription` |
| `location` | `ACCESS_FINE_LOCATION` + `ACCESS_COARSE_LOCATION` | `NSLocationWhenInUseUsageDescription` |
| `location-background` | `ACCESS_FINE_LOCATION` + `ACCESS_COARSE_LOCATION` + `ACCESS_BACKGROUND_LOCATION` | `NSLocationWhenInUseUsageDescription` + `NSLocationAlwaysAndWhenInUseUsageDescription` |
| `notifications` | `POST_NOTIFICATIONS` | *(runtime `UNUserNotificationCenter` prompt; no plist key)* |
| `files` | *(scoped Storage Access Framework; no broad storage permission)* | *(scoped `UIDocumentPickerViewController`; no plist key)* |
| `photos` | `READ_MEDIA_IMAGES` + `READ_MEDIA_VISUAL_USER_SELECTED` + `READ_EXTERNAL_STORAGE` (`maxSdkVersion="32"`) | `NSPhotoLibraryUsageDescription` |
| `contacts` | `READ_CONTACTS` | `NSContactsUsageDescription` |
| `biometrics` | `USE_BIOMETRIC` + `USE_FINGERPRINT` (`maxSdkVersion="27"`) | `NSFaceIDUsageDescription` |
| `bluetooth` | `BLUETOOTH` + `BLUETOOTH_ADMIN` (`maxSdkVersion="30"`) + `BLUETOOTH_SCAN` (`neverForLocation`) + `BLUETOOTH_CONNECT` | `NSBluetoothAlwaysUsageDescription` |
| `sensors` | `ACTIVITY_RECOGNITION` + `BODY_SENSORS` | `NSMotionUsageDescription` |

### Declaring & customizing permissions (`tj.lock` + `permissions.json`)

1. Any locked package in `tj.lock` with `permissions.device: ["camera", ...]` automatically
   adds those capabilities to the app's manifest and mobile build outputs.
2. An app may also place an optional `permissions.json` next to `app.tj` to declare local
   device capabilities or supply human-readable iOS usage descriptions (which App Store review
   requires to explain why the app asks):

```json
{
  "device": {
    "camera": "Scan QR codes and take profile photos.",
    "location": "Show nearby clinics while booking an appointment."
  }
}
```

- Unknown permission names fail closed in `tj registry check`, `tj check`, and `tj build`.
- `tj check --manifest` lists all active `devicePermissions` (with sources) so agents and
  critics can audit what hardware access an app requests.
- `tj build --android` injects the exact `<uses-permission>` / `<uses-feature>` entries into
  `AndroidManifest.xml`, and `tj build --ios` injects the exact `NS*UsageDescription` keys
  into `Info.plist`.

## Built-in opt-in host device plugins (`files`, `notifications`, `location`, `camera`)

TinyJump ships four opt-in host implementations outside `tj-core`:

- Web: `hosts/web/plugins/device.js` (plain ES2017, zero dependencies)
- Android: `hosts/android/plugins/DevicePlugins.java` (plain Java 11, `minSdk 21`, zero AndroidX/Kotlin)
- iOS: `hosts/ios/Plugins/DevicePlugins.swift` (pure Apple system frameworks, iOS 13+)

**Zero-cost opt-in:** `tj build` (`--web`, `--android`, `--ios`) copies and wires these files
**only** when the app's resolved `devicePermissions` (from `tj.lock` or `permissions.json`)
includes one of them. Apps that do not declare device permissions pay **0 bytes**. On
`tj dev` and `tj serve --web`, `/device-plugins.js` serves a 48-byte empty stub when no
device plugins are declared, and reloads live if `permissions.json` or `tj.lock` changes.

| Plugin | Permission | Methods | Return shape |
|---|---|---|---|
| `files` | `files` | `pick({ accept?, multiple?, readAs?: "text" \| "base64", maxBytes? })` | `{ cancelled, files: [{ name, mime, size, text?, base64? }] }` (10 MiB aggregate cap) |
| `files` | `files` | `save({ name, mime?, text?, base64? })` | `{ saved, cancelled, name }` |
| `notifications` | `notifications` | `requestPermission({})` | `{ granted, status }` |
| `notifications` | `notifications` | `notify({ id?, title, body? })` | `{ delivered, id }` |
| `location` | `location` / `location-background` | `currentPosition({ highAccuracy?, timeoutMs? })` | `{ lat, lng, accuracy, altitude, timestamp }` |
| `camera` | `camera` | `capture({ facing?: "back" \| "front", maxDimension?, quality? })` | `{ cancelled, mime, width, height, base64 }` *(Note: Android uses zero-dependency `ACTION_IMAGE_CAPTURE` without `FileProvider`, returning a compact camera thumbnail; apps needing full-sensor capture can register a custom `camera` plugin.)* |

Install the typed `.tj` wrapper modules from the official registry via:

```sh
tj add files notifications location camera
```

## Calls and media streaming (WebRTC)

See [docs/webrtc.md](webrtc.md) and [examples/video-call/app.tj](../examples/video-call/app.tj)
for the opt-in `<Native name="VideoCall" ... />` component and `@server` signaling pattern:

- **Signaling:** an existing `@server` object holds room membership, SDP offers/answers, and
  ICE candidates over the existing realtime change stream.
- **Native view:** `<Native name="VideoCall" active={...} peerId={...} remotePeerId={...} signals={...} onSignal={...} />`
  bundles `hosts/web/plugins/webrtc.js` automatically on Web (0 bytes when unused) and
  exposes the identical `registerComponent("VideoCall", ...)` contract on Android & iOS.
- **Plugin permissions:** `permissions.device: ["camera", "microphone"]`.

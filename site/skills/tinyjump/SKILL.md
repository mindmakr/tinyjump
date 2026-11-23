---
name: tinyjump
description: Build, verify, test, and package multi-platform apps with the tinyjump runtime (.tj TypeScript-shaped language, durable @server objects, native UI across Web, Android, iOS, macOS, Linux desktop, and self-hosted Linux servers). Activate whenever the user wants to create, edit, debug, check, test, or build a tinyjump app.
---

# TinyJump Multi-Platform App Builder

You are building an application on **tinyjump** — one pure-Rust runtime where a single TypeScript-shaped codebase (`.tj`) runs natively across **Web (WASM + DOM)**, **Android (plain Java Views, minSdk 21)**, **iOS (UIKit, iOS 13+)**, **macOS (AppKit, macOS 12+)**, **Linux Desktop (GTK 3)**, and a **durable real-time server (`tj serve`)**.

## Step 0 — Load the Exact Language Reference (Mandatory)

Before writing or modifying any `.tj` code, load the exact single-page language spec from the local CLI so you never guess syntax or props:

```sh
cargo tj docs
# or if `tj` is installed on PATH:
tj docs
```

If you are connected to a running app via MCP (`tj mcp`), call the `language` MCP tool instead.

## Step 1 — Follow TinyJump Language & Architecture Rules

Keep every app minimal, idiomatic, and compile-clean on the first try:

1. **Strict TypeScript subset — one way to do each thing:**
   - Always use `===` and `!==` (never `==` or `!=`).
   - Top-level declarations: `object`, `function`, `type`, `interface`, `let`, `const`. `function App()` is the UI root in `app.tj`.
   - Split larger apps into modules with `export` and `import { Foo } from "./foo"` (no default exports, no `import * as`, no `as` renames).
   - Built-in registry/plugins: `tj:ui` (`Field`, `Notice`, `Form`, `FormInput`, `Actions`), `tj:ai`, `tj:agent`, `tj:privacy`, `tj:consent`, or external Official packages via `tj search` / `tj add <name>` (e.g. `money`, `slug`, `csv`, `validate`, `files`, `notifications`, `location`, `camera`, `webrtc`, `gdpr-eu`, `pdpl-sa`).
2. **Durable objects (`object`) and real-time backend (`@server`):**
   - Local screen/device state lives on plain `object`s (`Counter.get("main")`).
   - Shared backend state lives on `@server` objects (`@server @read("anyone") object Room { ... }`). Every device automatically gets an offline-cached replica and live SSE updates.
   - **Closed by default:** whenever you add `@allow(...)` on a method, the class must also declare `@read(...)`. Use `@exclusive` on `async` methods that check-and-act (like booking a slot) to prevent races.
   - Only an object's own methods may mutate its fields (`this.items.push(...)`). External code calls methods or copies arrays.
   - Validate inputs once with `check(condition, "field", "Message")` and use `@invariant check(): boolean { ... }` for state invariants that roll back atomically on violation.
   - Mark personal data with `@pii("category", "purpose")` and require explicit `reveal(value, "purpose")` before any external `fetch` or log.
3. **Native UI catalog (16 components + `<Native>`):**
   - Containers: `Column`, `Row`, `Scroll`, `List` (`itemHeight`, `horizontal`, `onEndReached`), `Spacer`, `Theme` (`mode`, `accent`, `surface`, `muted`).
   - Controls & display: `Text` (`lines`), `Button` (`disabled`, `onPress`), `Input` (`value`, `placeholder`, `secure`, `multiline`, `type`, `zone`, `onChange`, `onSubmit` — note: `<Input>` has no `disabled` prop), `Toggle` (`value`, `onChange`), `Image` (`src`, `fit`), `Time` (`value`, `show`, `style`, `zone`), `Format` (`value`, `kind`, `currency`, `decimals`, `locale`), `Tabs` (`items`, `value`, `onChange`), `Icon` (`name`), and `<Native name="VideoCall" ... />` for opt-in native views.
   - **Always put `id="..."` on interactive controls and key outputs** so `tj test`, `tj review`, and `tj mcp` can address them reliably.
   - **Accessibility & layout hygiene:** keep interactive buttons/inputs at least 44px touch height where practical, ensure >= 4.5:1 text contrast on backgrounds, and give list items a `key`.
4. **Internationalization & RTL (zero ICU bloat):**
   - Put translations in `locales/en.json`, `locales/ar.json`, etc., and call `t("key", { count })`.
   - Use `<Time>` and `<Format>` so dates, numbers, and currencies render via each platform's native formatter.
   - Never hardcode left/right assumptions — Taffy mirrors rows and alignment automatically for RTL locales (`ar`, `ur`, `he`, `fa`).
5. **Opt-in Device Capabilities (`permissions.json`):**
   - Only declare what the app uses in `permissions.json` (e.g. `{"device": ["camera", "location", "notifications", "files"]}`), then install with `tj add camera` etc. `tj build` automatically injects the right `AndroidManifest.xml` permissions and iOS `Info.plist` usage descriptions.

## Step 2 — Run the Closed Verification Loop Before Shipping

Never stop after writing `.tj` code — prove it works using the built-in CLI tools:

```sh
# 1. Static compile, type, access-control & privacy check
cargo tj check path/to/app.tj --access --privacy

# 2. Optional: check against a compliance pack (e.g. GDPR / Saudi PDPL)
cargo tj check path/to/app.tj --privacy --compliance path/to/pack.json

# 3. Run deterministic in-memory trace tests (create tests/*.trace.jsonl!)
cargo tj test path/to/app.tj

# 4. Offline multi-locale, RTL & pseudo-locale (+35% text expansion) layout/contrast sweep
cargo tj review path/to/app.tj --pseudo

# 5. Performance & step benchmark
cargo tj bench path/to/app.tj
```

### Writing Deterministic Trace Tests (`tests/<name>.trace.jsonl`)

Put JSONL trace tests in `tests/` next to `app.tj` so `tj test` replays them deterministically in milliseconds (no browser or emulator needed):

```json
{"do":"boot","at":1700000000000}
{"do":"ui","target":{"id":"draft"},"event":"onChange","value":"Ship app"}
{"do":"ui","target":{"id":"add"},"event":"onPress"}
{"do":"assertScreen","contains":["Ship app"]}
{"do":"get","object":"Todos/main","assert":{"items":[{"text":"Ship app"}]}}
{"do":"review","width":390,"height":844}
```

Tip: `assertScreen` matches the compact outline (where `<Text>` is truncated at 30 chars), so keep `assertScreen` substrings <= 30 chars and assert full data via `{"do":"get", ...}`.

### Live Inspection via `tj mcp`

When the app is running (`tj dev`, Android debug `:4701`, iOS Simulator `:4701`, macOS `:4702`, or Linux GTK 3 `:4703`), connect `tj mcp --target <url>` to inspect the semantic tree, drive UI inputs, advance clocks, capture screenshots, and record `.trace.jsonl` regressions.

## Step 3 — Run & Package for Every Supported Platform

Use the commands below for each target platform TinyJump supports today:

### 1. Web (WASM + DOM — 215 KB gz AOT)
```sh
# Dev server with live reload on http://127.0.0.1:4700
cargo web
cargo tj dev path/to/app.tj

# Production static web bundle (auto-prefers compiler-free AOT WASM when built via `cargo web-aot`)
cargo web-aot
cargo tj build path/to/app.tj --web --out dist/web
```

### 2. Android (Native Java Views, minSdk 21 — 531 KB arm64 AOT APK)
Requires Android SDK + NDK + `cargo-ndk` + JDK 17.
```sh
# Generate self-contained Gradle project and assemble an installable arm64 AOT APK
cargo tj build path/to/app.tj --android --aot \
  --app-id com.example.myapp \
  --title "My App" \
  --version 1.0.0 \
  --abis arm64-v8a \
  --server https://api.example.com \
  --assemble --out dist/android
# Output APK: dist/android/app/build/outputs/apk/debug/app-debug.apk
# Add `--release` (with TJ_ANDROID_KEYSTORE* env vars) for signed release builds.
```

### 3. iOS (Native UIKit, iOS 13+ — Zero-Xcode `.app`)
Requires macOS with Xcode Command Line Tools (`xcrun`, `swiftc`, `aarch64-apple-ios-sim` / `aarch64-apple-ios`).
```sh
# Generate iOS bundle and assemble a runnable Simulator .app without opening Xcode
cargo tj build path/to/app.tj --ios --aot \
  --app-id com.example.myapp \
  --title "My App" \
  --version 1.0.0 \
  --server https://api.example.com \
  --assemble --out dist/ios
```

### 4. macOS Desktop (Native AppKit, macOS 12+)
Requires macOS with Swift toolchain.
```sh
# Build target/macos/TinyJump.app
bash scripts/macos.sh

# Launch your app natively on macOS (automation on 127.0.0.1:4702)
target/macos/TinyJump.app/Contents/MacOS/TinyJump \
  --app path/to/app.tj \
  --server http://127.0.0.1:4700
```

### 5. Linux Desktop (Native GTK 3 — 3.1 MB binary, zero dev headers)
Requires Linux with runtime `libgtk-3.so.0` (standard on Ubuntu/Fedora/Arch desktops; no `libgtk-3-dev` needed).
```sh
# Build hosts/linux/target/release/tinyjump-linux
bash scripts/linux.sh

# Launch your .tj or precompiled AOT .tjb app natively on Linux (automation on 127.0.0.1:4703)
hosts/linux/target/release/tinyjump-linux \
  --app path/to/app.tj \
  --server http://127.0.0.1:4700
```

### 6. Self-Hosted Linux / Production Backend
```sh
# Generate scoped API keys & start durable server with encryption at rest
cargo tj key alice --keys keys.json --role staff
cargo tj serve path/to/app.tj --port 4700 --host 0.0.0.0 --data ./data

# Or package a single self-contained Linux binary with embedded AOT bytecode
cargo tj build path/to/app.tj --linux --out dist/my-app
```

## Step 4 — Pre-Handoff Checklist

Before finishing, always verify and report:
- `tj check <app.tj> --access --privacy` exits `0` with no unexpected warnings.
- `tj test <app.tj>` passes all `.trace.jsonl` scenarios.
- `tj review <app.tj> --pseudo` passes with zero layout/overflow/clipping errors.
- Exact commands to run the app on the user's requested target platforms.

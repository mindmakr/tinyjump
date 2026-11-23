# macOS native AppKit host (`hosts/macos`)

Runs any tinyjump app as a native macOS desktop application (AppKit + `crates/tj-ffi` C ABI,
macOS 12+ on Apple Silicon and Intel) with **zero Electron**, **zero web views**, and **no Xcode
project required**.

## Quick start (macOS)

```sh
# Build target/macos/TinyJump.app
bash scripts/macos.sh

# Run an example app
target/macos/TinyJump.app/Contents/MacOS/TinyJump --example todo

# Run your own .tj or AOT .tjb app against a live `tj serve` backend
target/macos/TinyJump.app/Contents/MacOS/TinyJump \
  --app path/to/app.tj \
  --server http://127.0.0.1:4700

# Run the automated AppKit UI, RTL, screenshot, and @server sync test suite
bash scripts/macos.sh test
```

## Capabilities

- **Full native AppKit catalog:** mirrors every component in `crates/tj-core/src/ui/catalog.rs`
  (`Column`, `Row`, `Scroll`, `List`, `Text`, `Button`, `Input` including `secure`, `Toggle`
  via `NSSwitch`, `Image`, `Time`, `Format`, `Tabs` via `NSSegmentedControl`, `Icon` via
  SF Symbols, `Theme` via `NSAppearance`, `Spacer`, and `<Native name="..." />` via
  `TinyJump.registerComponent`).
- **Device plugins:** supports `TinyJump.registerPlugin(_:_:)` identically to iOS (`docs/device-plugins.md`).
- **Automation & visual review:** debug builds serve `/_tj/inspect`, `/_tj/input`, and
  `/_tj/screenshot` (PNG via `cacheDisplay`, no screen-recording permission needed) on
  `127.0.0.1:4702`, compatible with `tj review --target http://127.0.0.1:4702`.

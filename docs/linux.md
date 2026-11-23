# Linux native GTK 3 host (`hosts/linux`)

Runs any tinyjump `.tj` or precompiled binary AOT `.tjb` app natively on Linux with **GTK 3** —
zero Electron, zero Chromium/WebKit, zero `libgtk-3-dev` compile headers (dynamically loads the
system's `libgtk-3.so.0` at runtime), and **0 bytes added to `tj-core`**.

## Quick start (Linux)

```sh
# Build hosts/linux/target/release/tinyjump-linux (3.1 MB stripped)
bash scripts/linux.sh

# Run an example app
hosts/linux/target/release/tinyjump-linux --example todo

# Run your own .tj or binary AOT .tjb app against a live `tj serve` backend
hosts/linux/target/release/tinyjump-linux \
  --app path/to/app.tj \
  --server http://127.0.0.1:4700

# Run the automated headless (xvfb-run) UI, RTL, screenshot & @server sync test suite
bash scripts/linux.sh test
```

## What is built

- **Native GTK 3 widgets:** Taffy-positioned containers (`Column`, `Row`, `Scroll`, `List` with
  `onEndReached`, `Spacer`, `Theme`), `Text`, `Button`, `Input` (including `secure` and typed
  `number`/`date`/`time`/`datetime` epoch-ms conversion), `Toggle`, `Tabs`, `Time`, `Format`,
  Freedesktop-themed `Icon`, `GtkCssProvider` styling, and `GTK_TEXT_DIR_RTL` layout mirroring.
- **Crash-safe local storage & `@server` SSE sync:** owner-only (`0700`/`0600`) fsynced atomic
  `FileStore` (defaulting to `$XDG_DATA_HOME/tinyjump`), OS CSPRNG entropy/seeding from
  `/dev/urandom`, batched `GET /api/events` realtime stream, and `Idempotency-Key` server calls.
- **Automation & visual review:** serves `GET /_tj/inspect`, `POST /_tj/input`, and
  `GET /_tj/screenshot` (opaque Cairo PNG) on `127.0.0.1:4703`, compatible with
  `tj review --target http://127.0.0.1:4703`.

## Not yet built on Linux

- Remote HTTP image loading for `<Image src="..." />` (renders a placeholder icon for now).
- Multi-line `<Input multiline />` (uses single-line `GtkEntry` for now).
- Locale-specific `<Time>`/`<Format>` formatting beyond ISO-8601 / decimal formatting.
- Custom `<Native name="..." />` view and `DevicePlugin` host registrations (returns an explicit
  unsupported-plugin error to the app).

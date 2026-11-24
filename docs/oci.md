# Sealed Container / OCI Deployment Units (`tj build --oci`)

`tj build <app.tj> --oci` packages a TinyJump app into a deterministic, rootless `FROM scratch` **OCI Image Layout 1.0.0** bundle (`layout/`, `manifest.json`, `Containerfile`, and optionally a `.tar` archive) with **zero Docker daemon** at build time and **0 KB** added to `tj-core` or device hosts.

See [feedback/2026-11-24-builder-072-oci-retro.md](../feedback/2026-11-24-builder-072-oci-retro.md) for the design retro explaining why TinyJump implements sealed OCI packaging rather than embedding a Linux container runtime.

## Quick start

```sh
# Emit layout/, manifest.json, and Containerfile under ./examples/todo/dist/oci
tj build examples/todo/app.tj --oci

# Also emit a loadable single-file tar archive (todo-oci.tar)
tj build examples/todo/app.tj --oci --assemble

# Emit only a tar archive directly
tj build examples/todo/app.tj --oci --out ./todo.tar

# Cross-package on macOS/Windows using a static Linux tj binary
tj build examples/todo/app.tj --oci --runner ./tj-linux-amd64 --aot --assemble
```

## CLI flags

| Flag | Default | Description |
|---|---|---|
| `--out <path>` | `<app-dir>/dist/oci` | Output folder (`layout/`, `Containerfile`, `manifest.json`), or direct `.tar` archive when ending in `.tar` |
| `--assemble` | `false` | Also write `<bin_name>-oci.tar` alongside `layout/` (OCI layout archive for `podman load`, `skopeo copy oci-archive:...`, or Docker with containerd image store) |
| `--port <n>` | `4700` | Exposed TCP port (`1..=65535`), sets `ExposedPorts` and `TJ_DEFAULT_PORT` (`PORT` / `TJ_PORT` override at runtime) |
| `--arch <arch>` | auto-detected from `--runner` ELF header | `amd64`, `arm64`, or `arm` |
| `--no-web` | `false` | Backend/headless API-only image; omits `/web/` static UI and WASM bundle |
| `--runner <file>` | current `tj` executable | Standalone host runner binary (pass a Linux static `tj` ELF when cross-packaging) |
| `--wasm <file>` | auto-located `tj_wasm.wasm` | Explicit WASM binary to pack into `/web/tinyjump.wasm` |
| `--aot` | `false` | Strip compiler and pack compiler-free AOT WASM (`cargo web-aot`) when available |
| `--title <name>` | `<app>` | OCI `org.opencontainers.image.title` label/annotation |
| `--version <x.y.z>` | `0.1.0` | Strict `major.minor.patch` tag used in `org.opencontainers.image.ref.name` (`<bin>:<version>`) |
| `--min-trust <level>` | none | Minimum trust level enforced for `tj.lock` packages (`community`, `verified`, `official`) |
| `--json` | `false` | Machine-readable build report (digests, layer byte sizes, web assets) |

## Image structure & runtime contract

Every `--oci` image contains a single deterministic gzipped POSIX `ustar` layer (`mtime = 0`, owned by `65532:65532` `nonroot`):

```text
/app/<bin>         # Standalone executable (runner + .tjb bytecode + 48-byte TJAPP001 SHA-256 trailer)
/data/             # Empty writable mount point (VOLUME ["/data"], TJ_DATA=/data)
/web/              # Optional static UI bundle (unless --no-web or headless app)
  index.html
  tinyjump.js
  tinyjump.wasm
  plugins/*.js     # Opt-in plugins (device, webrtc, whiteboard, gfx) when used
  permissions.json # Normalized device permission declarations (never served over HTTP)
```

At startup, embedded binaries produced by `--linux` or `--oci`:
- Verify the `TJAPP001` trailer's SHA-256 against the embedded bytecode before booting.
- Bind `--host 0.0.0.0` (from container `CMD`) and listen on `--port`, or `$TJ_PORT` -> `$PORT` -> build-time `$TJ_DEFAULT_PORT` -> `4700` (so PaaS `PORT=8080` overrides work out of the box), storing state in `--data` or `$TJ_DATA` (`/data`).
- Serve the companion browser UI from `--web-dir /web` while keeping `permissions.json` local to engine initialization.
- Support `--info` and `--info --json` directly with `FROM scratch` (no `/bin/sh` required) to inspect embedded bytecode hash, object classes, and port.

### What is never packed (sealing guarantee)

- `.tj-data/`, `*.key`, `audit.key`, TLS certificates, `.env`, and runtime API keys (`TJ_KEYS`, `TJ_KEY_FILE`, `TJ_AUDIT_KEY_FILE`) are never copied into the image layer or config.
- `oci::validate_web_rel_path` rejects `.tj-data` or `*.key` paths defensively.
- Mount persistent state and keys at runtime into `/data` and container env vars.

## Deploying with Docker, Podman, or Skopeo

```sh
# Load an --assemble OCI archive into Podman or Docker (containerd image store),
# or copy the unpacked layout into classic Docker / any OCI registry via Skopeo
podman load -i examples/todo/dist/oci/todo-oci.tar
skopeo copy oci:examples/todo/dist/oci/layout docker-daemon:todo:0.1.0
skopeo copy oci:examples/todo/dist/oci/layout docker://ghcr.io/you/todo:0.1.0

# Create keys file (if enforcing API keys) and run rootless with persistent storage;
# set TJ_TLS_TERMINATED=1 when running behind a TLS-terminating reverse proxy
docker run --rm -p 4700:4700 \
  -v todo-data:/data \
  -v ./keys.json:/data/keys.json:ro \
  -e TJ_KEY_FILE=/data/app.key \
  -e TJ_KEYS=/data/keys.json \
  -e TJ_TLS_TERMINATED=1 \
  todo:0.1.0
```

## Measured sizes

Measured on Windows `tj.exe` (`10,465,280` bytes release CLI) with full `860 KB` `tj_wasm.wasm` (cross-packaging a stripped `musl` Linux runner + `--aot` WASM shrinks images further):

| App | Bytecode | Standalone exe | Uncompressed layer | Gzip layer | Assembled `.tar` |
|---|---|---|---|---|---|
| `examples/todo/app.tj` | 9,447 B | 10,474,775 B | 11,414,528 B | 4,326,468 B (4.13 MiB) | 4,335,104 B (4.13 MiB) |
| `examples/booking/app.tj` | 27,792 B | 10,493,120 B | 11,432,960 B | 4,333,277 B (4.13 MiB) | 4,341,760 B (4.14 MiB) |

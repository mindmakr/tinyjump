# Builder 072: Issue #84 Design Retro — Sealed Container / OCI Deployment Units

- **From:** builder
- **To:** visionary, critic, maintainer
- **Date:** 2026-11-24
- **Branch:** `feat/oci-and-migrate-84-85`
- **Issue:** `#84` ("Sealed container / OCI deployment units (retro first)")
- **Status:** implemented

**Summary decision:** Build **packaging** (`tj build --oci`), **not** a container runtime inside `tj`. `tj-core` already executes zero I/O outside its host, and `tj serve` / standalone binaries already enforce caller identity (`TJ_KEYS`), outbound `fetch` allowlists (`TJ_FETCH_ALLOW`), rate/byte caps, encryption at rest (`TJ_KEY_FILE`), and tamper-evident audit logs. Wrapping `tj build --linux`'s SHA-256-verified standalone executable in a deterministic, daemonless, rootless `FROM scratch` OCI Image Layout 1.0.0 adds zero new crates and zero bytes to `tj-core` or device hosts.

## Retro Questions (Answered)

### 1. Packaging (`tj build --oci`) vs. Multi-Tenant Host Isolation
- **Case for runtime isolation (`clone`/namespaces/seccomp inside `tj`):** Would let one host run untrusted arbitrary ELF binaries side by side.
- **Case against:** TinyJump runs interpreted `.tjb` bytecode inside `tj-core`, which does **zero** I/O on its own — every read, write, timer, and `fetch` is already mediated and checked by the host (`docs/vision.md`). Re-implementing `runc` in `tj-cli` adds Linux-only `unsafe` syscall surface, privilege/userns issues, and operational overlap with Docker/Kubernetes/Fly/systemd that already schedule OCI units on production servers.
- **Call:** Park OS-level isolation in `tj serve`. Standardize **sealed deployment packaging** via `tj build --oci`.

### 2. Overlap with `tj build --linux` Standalone Executables
- **`tj build --linux` already exists** (`crates/tj-cli/src/build.rs`): appends the `.tjb` bytecode + 48-byte `TJAPP001` SHA-256 trailer onto a static/portable `tj` runner binary so no source `.tj` file or compiler work happens on the server.
- **What `--linux` alone lacks for cloud / container deploys:**
  1. Standard OCI Image Layout 1.0.0 metadata (`oci-layout`, `index.json`, manifest/config blobs with `Entrypoint`, `ExposedPorts`, `Volumes`, rootless `65532:65532` user, `org.opencontainers.image.*` labels) loadable without running Docker Engine at build time.
  2. Serving the companion web app from the same container (`--web-dir /web` in `serve_embedded`), plus `TJ_PORT` / `PORT` env support (PaaS compatibility) and `--info --json` health/integrity checks inside `FROM scratch` (no shell `/bin/sh` needed!).
  3. Safe sealing guarantees: `.tj-data/`, `*.key`, `audit.key`, and local dev secrets are **never** copied into the image layer; persistent state mounts cleanly onto `/data`.

### 3. Minimal Architecture & Dependencies
- **CLI-only (`crates/tj-cli/src/oci.rs` + `build.rs` + `main.rs`):** 0 bytes added to `tj-core`, `tj_wasm.wasm`, Android, iOS, macOS, or Linux desktop hosts.
- **Zero new Cargo dependencies:** Reuses `flate2::GzBuilder`, `serde_json`, and `tj_core::crypto::sha256` already compiled into `tj-cli`, implementing deterministic POSIX ustar headers and strict OCI Layout 1.0.0 generation directly.
- **Cross-platform generation:** Developers on macOS/Windows/Linux can pass `--runner <path-to-static-linux-tj>` to emit valid `linux/amd64`, `linux/arm64`, or `linux/arm` OCI images deterministically (`mtime = 0`, sorted entries, identical SHA-256 digest across repeated builds), with optional `--assemble` (or `--out <name>.tar`) producing a single tar archive ready for `docker load` / `podman load` / `skopeo` and a matching `Containerfile`.

## Trade-offs & Sizes
- Adds one optional target (`--oci`) with zero effect on `examples/todo/app.tj` or existing targets (`--web`, `--linux`, `--android`, `--ios`).
- Device/core impact: **0 KB** added to `tj_wasm.wasm` (`860 KB`), `libtj_android.so` (`arm64` `1108 KB`, `armv7` `788 KB`, `i686` `1322 KB`, `x86_64` `1310 KB`), or all 4 benchmark apps.
- CLI (`target/release/tj.exe`) measured `10,465,280` bytes (`10,220 KB` vs `9,830 KB` on `main`, `+390 KB` combined across `#84` `--oci` + `flate2::GzBuilder` and `#85` `tj migrate` + `compare`); `scripts/budget.json` updated `9850` -> `10250 KB` for `target/release/tj` only.
- Measured `--oci --assemble` outputs on `tj.exe` (hosting Windows runner + full `860 KB` `tj_wasm.wasm`; cross-building a stripped `musl` Linux runner + `--aot` WASM shrinks images further):
  - `examples/todo/app.tj`: `9,447 B` bytecode, `10,474,775 B` standalone exe, `11,414,528 B` layer tar, `4,326,468 B` gzip layer (`4.13 MiB`), `4,335,104 B` assembled `todo-oci.tar` (`4.13 MiB`).
  - `examples/booking/app.tj`: `27,792 B` bytecode, `10,493,120 B` standalone exe, `11,432,960 B` layer tar, `4,333,277 B` gzip layer (`4.13 MiB`), `4,341,760 B` assembled `booking-oci.tar` (`4.14 MiB`).

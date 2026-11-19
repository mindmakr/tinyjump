#!/usr/bin/env bash
# Builds fully static `tj` binaries for Linux on x86_64, 64-bit ARM and 32-bit ARM
# (Raspberry Pi and older boards) into target/dist/.
# Needs zig on PATH and `cargo install cargo-zigbuild`. Run on Linux or WSL.
set -euo pipefail
cd "$(dirname "$0")/.."

targets=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf)
rustup target add "${targets[@]}" >/dev/null
mkdir -p target/dist
target_dir=${CARGO_TARGET_DIR:-target}
for t in "${targets[@]}"; do
  cargo zigbuild --release -p tj-cli --target "$t"
  cp "$target_dir/$t/release/tj" "target/dist/tj-$t"
done
ls -l target/dist

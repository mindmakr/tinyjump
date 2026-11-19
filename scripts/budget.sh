#!/usr/bin/env bash
# Builds what ships and checks it against scripts/budget.json: the size of the web runtime,
# the Android libraries and the CLI, and how fast and how much work each example takes to
# boot and run for a minute (`tj bench`). Run it before merging; it costs no CI minutes.
#
# It fails when anything is slower or bigger than its budget. If a change is worth the
# cost, raise the budget in the same commit and say why in the message.
# Needs cargo-ndk and the Android NDK for the Android libraries.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo web
cargo build --release -p tj-cli
cargo ndk --platform 21 -t armeabi-v7a -t arm64-v8a -t x86 -t x86_64 build --profile small -p tj-android
target/release/tj bench --budget scripts/budget.json

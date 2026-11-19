#!/usr/bin/env bash
# Builds the iOS demo app for the Simulator straight from the sources (no Xcode project).
# With `test`, it boots a simulator, installs the app and drives it through its automation
# API: local objects (todo) and server objects against a `tj serve` backend (chat).
# Runs on macOS with Xcode; CI runs it on a GitHub macOS runner.
set -euo pipefail
cd "$(dirname "$0")/.."

case "$(uname -m)" in
  arm64) rust=aarch64-apple-ios-sim; swift=arm64-apple-ios13.0-simulator ;;
  *) rust=x86_64-apple-ios; swift=x86_64-apple-ios13.0-simulator ;;
esac
rustup target add "$rust" >/dev/null
cargo build -p tj-ffi --profile small --target "$rust"

out=target/ios-sim
app="$out/TinyJump.app"
rm -rf "$app"
mkdir -p "$app"
xcrun --sdk iphonesimulator swiftc -O -D DEBUG -parse-as-library -target "$swift" \
  -import-objc-header crates/tj-ffi/include/tinyjump.h \
  -L "target/$rust/small" -ltj_ffi \
  hosts/ios/Sources/TinyJump/*.swift hosts/ios/Demo/*.swift \
  -o "$app/TinyJump"
mkdir -p "$app/examples"
for dir in examples/*/; do
  name=$(basename "$dir")
  mkdir -p "$app/examples/$name"
  cp "$dir/app.tj" "$app/examples/$name/"
done
cp hosts/ios/Demo/Info.plist "$app/Info.plist"
codesign --force --sign - "$app"
echo "built $app"

[[ "${1:-}" == "test" ]] || exit 0

bundle=com.mindmakr.tinyjump.demo
api=http://127.0.0.1:4701
udid=$(xcrun simctl list devices available -j | python3 -c '
import json, sys
devices = json.load(sys.stdin)["devices"]
print(next(d["udid"] for runtime in sorted(devices, reverse=True) if "iOS" in runtime for d in devices[runtime] if "iPhone" in d["name"]))')
echo "simulator $udid"
xcrun simctl boot "$udid" 2>/dev/null || true
xcrun simctl bootstatus "$udid" -b
xcrun simctl install "$udid" "$app"

wait_up() {
  for _ in $(seq 1 60); do
    curl -sf "$api/_tj/inspect" >/dev/null && return 0
    sleep 1
  done
  echo "the app did not start" >&2
  return 1
}
input() { curl -sf -X POST "$api/_tj/input" -H 'content-type: application/json' -d "$1" >/dev/null; }
expect() { # expect <url> <text>
  for _ in $(seq 1 20); do
    curl -sf "$1" | grep -q "$2" && return 0
    sleep 0.5
  done
  echo "expected '$2' from $1" >&2
  curl -s "$1" >&2 || true
  return 1
}

echo "== todo: native widgets, local objects, automation"
xcrun simctl launch "$udid" "$bundle" -example todo
wait_up
input '{"type":"ui","target":{"id":"draft"},"event":"onChange","value":"Hello from iOS"}'
input '{"type":"ui","target":{"id":"add"},"event":"onPress"}'
expect "$api/_tj/inspect" 'Hello from iOS'
curl -sf "$api/_tj/inspect" >"$out/todo-inspect.json"
curl -sf "$api/_tj/screenshot" -o "$out/todo.png"
xcrun simctl terminate "$udid" "$bundle"

echo "== chat: @server objects against tj serve"
cargo build --release -p tj-cli
rm -rf "$out/chat-data"
target/release/tj serve examples/chat/app.tj --data "$out/chat-data" &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT
xcrun simctl launch "$udid" "$bundle" -example chat -server http://127.0.0.1:4700
wait_up
input '{"type":"ui","target":{"id":"name"},"event":"onChange","value":"iPhone"}'
input '{"type":"ui","target":{"id":"draft"},"event":"onChange","value":"Hello from iOS"}'
input '{"type":"ui","target":{"id":"send"},"event":"onPress"}'
expect http://127.0.0.1:4700/api/Room/lobby 'Hello from iOS'
curl -sf -X POST http://127.0.0.1:4700/api/Room/lobby/post -d '["server","hi phone"]' >/dev/null
expect "$api/_tj/inspect" 'hi phone'
curl -sf "$api/_tj/screenshot" -o "$out/chat.png"
echo "iOS tests passed"

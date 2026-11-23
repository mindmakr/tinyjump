#!/usr/bin/env bash
# Builds the native macOS AppKit host straight from sources (no Xcode project).
# With `test`, launches the native .app and drives it through /_tj/inspect, /_tj/input,
# /_tj/screenshot, and `tj review`: local objects (todo LTR & RTL) and @server objects (chat).
set -euo pipefail
cd "$(dirname "$0")/.."

case "$(uname -m)" in
  arm64) swift_target=arm64-apple-macos12.0 ;;
  *) swift_target=x86_64-apple-macos12.0 ;;
esac

cargo build -p tj-ffi --profile small

out=target/macos
app="$out/TinyJump.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/examples"

xcrun --sdk macosx swiftc -O -D DEBUG -parse-as-library -target "$swift_target" \
  -import-objc-header crates/tj-ffi/include/tinyjump.h \
  -L "target/small" -ltj_ffi \
  hosts/macos/Sources/TinyJump/*.swift hosts/macos/Demo/*.swift \
  -o "$app/Contents/MacOS/TinyJump"

for dir in examples/*/; do
  name=$(basename "$dir")
  (cd "$dir" && find . -name '*.tj') | while read -r f; do
    mkdir -p "$app/Contents/Resources/examples/$name/$(dirname "$f")"
    cp "$dir/$f" "$app/Contents/Resources/examples/$name/$f"
  done
done
cp hosts/macos/Demo/Info.plist "$app/Contents/Info.plist"
codesign --force --sign - "$app"
echo "built $app"

[[ "${1:-}" == "test" ]] || exit 0

api=http://127.0.0.1:4702
bin="$app/Contents/MacOS/TinyJump"
server=""
gui=""
cleanup() {
  if [[ -n "$gui" ]]; then kill "$gui" 2>/dev/null || true; wait "$gui" 2>/dev/null || true; fi
  if [[ -n "$server" ]]; then kill "$server" 2>/dev/null || true; wait "$server" 2>/dev/null || true; fi
}
trap cleanup EXIT

wait_up() {
  for _ in $(seq 1 60); do
    curl -sf "$api/_tj/inspect" >/dev/null && return 0
    sleep 0.5
  done
  echo "macOS app did not start" >&2
  return 1
}
stop_gui() {
  if [[ -n "$gui" ]]; then
    kill "$gui" 2>/dev/null || true
    wait "$gui" 2>/dev/null || true
    gui=""
  fi
}
input() { curl -sf -X POST "$api/_tj/input" -H 'content-type: application/json' -d "$1" >/dev/null; }
expect() {
  for _ in $(seq 1 20); do
    curl -sf "$1" | grep -q "$2" && return 0
    sleep 0.5
  done
  echo "expected '$2' from $1" >&2
  curl -s "$1" >&2 || true
  return 1
}
starts_left() {
  curl -sf "$api/_tj/inspect" | python3 -c '
import json, sys
first, second = sys.argv[1:]
x = {}
def walk(nodes):
    for n in nodes:
        if "id" in n.get("props", {}) and "bounds" in n:
            x[n["props"]["id"]] = n["bounds"]["x"]
        walk(n.get("children", []))
walk(json.load(sys.stdin)["ok"]["tree"])
if not x[first] < x[second]:
    sys.exit(f"expected #{first} (x={x[first]}) left of #{second} (x={x[second]})")
' "$1" "$2"
}

cargo build --release -p tj-cli
tj=target/release/tj

echo "== macOS todo: native AppKit widgets, local objects, automation & screenshot"
rm -rf "$out/todo-data"
"$bin" --example todo --data "$out/todo-data" --port 4702 &
gui=$!
wait_up
input '{"type":"ui","target":{"id":"draft"},"event":"onChange","value":"Hello from macOS"}'
input '{"type":"ui","target":{"id":"add"},"event":"onPress"}'
expect "$api/_tj/inspect" 'Hello from macOS'
curl -sf "$api/_tj/inspect" >"$out/todo-inspect.json"
curl -sf "$api/_tj/screenshot" -o "$out/todo.png"
test -s "$out/todo.png"
"$tj" review --target "$api"
starts_left draft add
stop_gui

echo "== macOS todo RTL: mirrored layout"
"$bin" --example todo --data "$out/todo-data" --dir rtl --port 4702 &
gui=$!
wait_up
expect "$api/_tj/inspect" 'Hello from macOS'
curl -sf "$api/_tj/screenshot" -o "$out/todo-rtl.png"
"$tj" review --target "$api"
starts_left add draft
stop_gui

echo "== macOS chat: @server objects against tj serve"
rm -rf "$out/chat-data" "$out/chat-keys" "$out/chat-client"
key=$("$tj" key sam --keys "$out/chat-keys" --role staff | tail -n 1)
TJ_KEYS="$out/chat-keys" "$tj" serve examples/chat/app.tj --data "$out/chat-data" --port 4710 &
server=$!
server_ready=0
for _ in $(seq 1 40); do
  if curl -sf http://127.0.0.1:4710/api/whoami >/dev/null; then server_ready=1; break; fi
  sleep 0.25
done
if [[ "$server_ready" -ne 1 ]]; then
  echo "tj serve did not start on :4710" >&2
  exit 1
fi
"$bin" --example chat --server http://127.0.0.1:4710 --key "$key" --data "$out/chat-client" --port 4702 &
gui=$!
wait_up
expect "$api/_tj/inspect" 'Signed in as key:sam (staff)'
input '{"type":"ui","target":{"id":"name"},"event":"onChange","value":"Mac"}'
input '{"type":"ui","target":{"id":"draft"},"event":"onChange","value":"Hello from macOS"}'
input '{"type":"ui","target":{"id":"send"},"event":"onPress"}'
expect http://127.0.0.1:4710/api/Room/lobby 'Hello from macOS'
curl -sf -X POST http://127.0.0.1:4710/api/Room/lobby/post -H 'content-type: application/json' -d '["server","hi mac"]' >/dev/null
expect "$api/_tj/inspect" 'hi mac'
curl -sf "$api/_tj/screenshot" -o "$out/chat.png"
"$tj" review --target "$api"
starts_left draft send
stop_gui
echo "macOS tests passed"

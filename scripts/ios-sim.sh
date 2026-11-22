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
  # Every .tj file of the example: apps can be several files.
  (cd "$dir" && find . -name '*.tj') | while read -r f; do
    mkdir -p "$app/examples/$name/$(dirname "$f")"
    cp "$dir/$f" "$app/examples/$name/$f"
  done
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

starts_left() { # starts_left <first id> <second id>: the first is left of the second on screen
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

echo "== todo: native widgets, local objects, automation"
xcrun simctl launch "$udid" "$bundle" -example todo
wait_up
input '{"type":"ui","target":{"id":"draft"},"event":"onChange","value":"Hello from iOS"}'
input '{"type":"ui","target":{"id":"add"},"event":"onPress"}'
expect "$api/_tj/inspect" 'Hello from iOS'
curl -sf "$api/_tj/inspect" >"$out/todo-inspect.json"
curl -sf "$api/_tj/screenshot" -o "$out/todo.png"
"$tj" review --target "$api" # fails on layout errors; prints warnings
starts_left draft add
xcrun simctl terminate "$udid" "$bundle"

echo "== todo, right to left: the core mirrors the layout"
xcrun simctl launch "$udid" "$bundle" -example todo -dir rtl
wait_up
expect "$api/_tj/inspect" 'Hello from iOS'
curl -sf "$api/_tj/screenshot" -o "$out/todo-rtl.png"
"$tj" review --target "$api"
starts_left add draft
xcrun simctl terminate "$udid" "$bundle"

echo "== chat: @server objects against tj serve"
rm -rf "$out/chat-data" "$out/chat-keys"
key=$("$tj" key sam --keys "$out/chat-keys" --role staff | tail -n 1)
TJ_KEYS="$out/chat-keys" "$tj" serve examples/chat/app.tj --data "$out/chat-data" &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT
xcrun simctl launch "$udid" "$bundle" -example chat -server http://127.0.0.1:4700
wait_up
input '{"type":"ui","target":{"id":"name"},"event":"onChange","value":"iPhone"}'
input '{"type":"ui","target":{"id":"draft"},"event":"onChange","value":"Hello from iOS"}'
input '{"type":"ui","target":{"id":"send"},"event":"onPress"}'
expect http://127.0.0.1:4700/api/Room/lobby 'Hello from iOS'
curl -sf -X POST http://127.0.0.1:4700/api/Room/lobby/post -H 'content-type: application/json' -d '["server","hi phone"]' >/dev/null
expect "$api/_tj/inspect" 'hi phone'
curl -sf "$api/_tj/screenshot" -o "$out/chat.png"
"$tj" review --target "$api"
starts_left draft send
xcrun simctl terminate "$udid" "$bundle"

echo "== chat, right to left, signed in: the host passes the server's roles to the UI"
xcrun simctl launch "$udid" "$bundle" -example chat -server http://127.0.0.1:4700 -dir rtl -key "$key"
wait_up
expect "$api/_tj/inspect" 'hi phone'
expect "$api/_tj/inspect" 'Signed in as key:sam (staff)'
curl -sf "$api/_tj/screenshot" -o "$out/chat-rtl.png"
"$tj" review --target "$api"
starts_left send draft
kill $server 2>/dev/null || true
xcrun simctl terminate "$udid" "$bundle"

echo "== durable-cart: an app in several files, @server objects and server calls"
rm -rf "$out/cart-data"
"$tj" serve examples/durable-cart/app.tj --data "$out/cart-data" &
server=$!
xcrun simctl launch "$udid" "$bundle" -example durable-cart -server http://127.0.0.1:4700
wait_up
input '{"type":"ui","target":{"id":"name"},"event":"onChange","value":"iPhone"}'
input '{"type":"ui","target":{"id":"add-tea"},"event":"onPress"}'
expect http://127.0.0.1:4700/api/Cart/demo '"changedBy":"iPhone"'
expect "$api/_tj/inspect" 'Last change by iPhone'
curl -sf "$api/_tj/screenshot" -o "$out/cart.png"
"$tj" review --target "$api"
kill $server 2>/dev/null || true
xcrun simctl terminate "$udid" "$bundle"

echo "== booking: staff publishes a slot, customer saves profile and books (B19)"
rm -rf "$out/booking-data" "$out/booking-keys"
staff_key=$("$tj" key sam --keys "$out/booking-keys" --role staff | tail -n 1)
cust_key=$("$tj" key ana --keys "$out/booking-keys" | tail -n 1)
TJ_KEYS="$out/booking-keys" "$tj" serve examples/booking/app.tj --data "$out/booking-data" &
server=$!
xcrun simctl launch "$udid" "$bundle" -example booking -server http://127.0.0.1:4700 -key "$staff_key"
wait_up
expect "$api/_tj/inspect" 'Publish a slot'
input '{"type":"ui","target":{"id":"new-title"},"event":"onChange","value":"Haircut"}'
input '{"type":"ui","target":{"id":"new-start"},"event":"onChange","value":"2030-06-01 14:30"}'
input '{"type":"ui","target":{"id":"new-minutes"},"event":"onChange","value":"30"}'
input '{"type":"ui","target":{"id":"publish"},"event":"onPress"}'
expect "$api/_tj/inspect" 'Haircut'
curl -sf "$api/_tj/screenshot" -o "$out/booking-staff.png"
"$tj" review --target "$api"
xcrun simctl terminate "$udid" "$bundle"

xcrun simctl launch "$udid" "$bundle" -example booking -server http://127.0.0.1:4700 -key "$cust_key"
wait_up
expect "$api/_tj/inspect" 'Haircut'
input '{"type":"ui","target":{"id":"profile-name"},"event":"onChange","value":"Ana"}'
input '{"type":"ui","target":{"id":"profile-phone"},"event":"onChange","value":"555-0100"}'
input '{"type":"ui","target":{"id":"save-profile"},"event":"onPress"}'
expect "$api/_tj/inspect" 'Ana'
slot_id=$(curl -sf "$api/_tj/inspect" | python3 -c '
import json, sys
def walk(nodes):
    for n in nodes:
        i = n.get("props", {}).get("id", "")
        if i.startswith("book-"):
            print(i[5:])
            sys.exit(0)
        walk(n.get("children", []))
walk(json.load(sys.stdin)["ok"]["tree"])
sys.exit(1)
')
input "{\"type\":\"ui\",\"target\":{\"id\":\"book-$slot_id\"},\"event\":\"onPress\"}"
expect "$api/_tj/inspect" "cancel-$slot_id"
curl -sf "$api/_tj/screenshot" -o "$out/booking-customer.png"
"$tj" review --target "$api"
echo "iOS tests passed"

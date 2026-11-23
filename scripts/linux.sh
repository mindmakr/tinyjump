#!/usr/bin/env bash
# Builds the native Linux GTK 3 desktop host (dlopen libgtk-3.so.0, zero libgtk-3-dev headers).
# With `test`, launches under xvfb-run and drives todo (LTR & RTL), chat (@server sync),
# PNG screenshots, and `tj review` on 127.0.0.1:4703.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --manifest-path hosts/linux/Cargo.toml --release
bin=hosts/linux/target/release/tinyjump-linux
echo "built $bin ($(du -h "$bin" | cut -f1))"

[[ "${1:-}" == "test" ]] || exit 0

out=target/linux-host
mkdir -p "$out"
api=http://127.0.0.1:4703
server=""
gui=""

stop_gui() {
  if [[ -n "$gui" ]]; then
    # Kill the whole session/process group (xvfb-run + Xvfb + tinyjump-linux) cleanly.
    kill -- -"$gui" 2>/dev/null || kill "$gui" 2>/dev/null || true
    pkill -P "$gui" 2>/dev/null || true
    for _ in $(seq 1 20); do
      if ! curl -sf "$api/_tj/inspect" >/dev/null 2>&1; then break; fi
      sleep 0.1
    done
    kill -9 -- -"$gui" 2>/dev/null || kill -9 "$gui" 2>/dev/null || true
    wait "$gui" 2>/dev/null || true
    gui=""
  fi
}

cleanup() {
  stop_gui
  if [[ -n "$server" ]]; then
    kill "$server" 2>/dev/null || true
    wait "$server" 2>/dev/null || true
    server=""
  fi
}
trap cleanup EXIT INT TERM

launch_gui() {
  if command -v xvfb-run >/dev/null 2>&1; then
    setsid xvfb-run -a -s "-screen 0 1280x800x24" "$bin" "$@" &
  else
    setsid "$bin" "$@" &
  fi
  gui=$!
}

wait_up() {
  for _ in $(seq 1 60); do
    if ! kill -0 "$gui" 2>/dev/null; then
      echo "Linux GTK host exited unexpectedly before starting on $api" >&2
      return 1
    fi
    curl -sf "$api/_tj/inspect" >/dev/null && return 0
    sleep 0.25
  done
  echo "Linux GTK host did not start on $api" >&2
  return 1
}

assert_png() {
  test -s "$1"
  python3 -c '
import sys
p = sys.argv[1]
with open(p, "rb") as f:
    sig = f.read(8)
if sig != b"\x89PNG\r\n\x1a\n":
    sys.exit(f"{p} is not a valid PNG (magic={sig!r})")
' "$1"
}

input() { curl -sf -X POST "$api/_tj/input" -H 'content-type: application/json' -d "$1" >/dev/null; }
expect() {
  for _ in $(seq 1 30); do
    curl -sf "$1" | grep -q "$2" && return 0
    sleep 0.25
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

echo "== Linux GTK todo: native widgets, local objects, automation & screenshot"
rm -rf "$out/todo-data"
launch_gui --example todo --data "$out/todo-data" --port 4703
wait_up
input '{"type":"ui","target":{"id":"draft"},"event":"onChange","value":"Hello from Linux"}'
input '{"type":"ui","target":{"id":"add"},"event":"onPress"}'
expect "$api/_tj/inspect" 'Hello from Linux'
curl -sf "$api/_tj/inspect" >"$out/todo-inspect.json"
curl -sf "$api/_tj/screenshot" -o "$out/todo.png"
assert_png "$out/todo.png"
"$tj" review --target "$api"
starts_left draft add
stop_gui

echo "== Linux GTK todo RTL: mirrored layout"
launch_gui --example todo --data "$out/todo-data" --dir rtl --port 4703
wait_up
expect "$api/_tj/inspect" 'Hello from Linux'
curl -sf "$api/_tj/screenshot" -o "$out/todo-rtl.png"
assert_png "$out/todo-rtl.png"
"$tj" review --target "$api"
starts_left add draft
stop_gui

echo "== Linux GTK chat: @server objects against tj serve"
rm -rf "$out/chat-data" "$out/chat-keys" "$out/chat-client"
key=$("$tj" key sam --keys "$out/chat-keys" --role staff | tail -n 1)
TJ_KEYS="$out/chat-keys" "$tj" serve examples/chat/app.tj --data "$out/chat-data" --port 4711 &
server=$!
server_ready=0
for _ in $(seq 1 40); do
  if curl -sf http://127.0.0.1:4711/api/whoami >/dev/null; then server_ready=1; break; fi
  sleep 0.25
done
if [[ "$server_ready" -ne 1 ]]; then
  echo "tj serve did not start on :4711" >&2
  exit 1
fi
launch_gui --example chat --server http://127.0.0.1:4711 --key "$key" --data "$out/chat-client" --port 4703
wait_up
expect "$api/_tj/inspect" 'Signed in as key:sam (staff)'
input '{"type":"ui","target":{"id":"name"},"event":"onChange","value":"Linux"}'
input '{"type":"ui","target":{"id":"draft"},"event":"onChange","value":"Hello from Linux"}'
input '{"type":"ui","target":{"id":"send"},"event":"onPress"}'
expect http://127.0.0.1:4711/api/Room/lobby 'Hello from Linux'
curl -sf -X POST http://127.0.0.1:4711/api/Room/lobby/post -H 'content-type: application/json' -d '["server","hi linux"]' >/dev/null
expect "$api/_tj/inspect" 'hi linux'
curl -sf "$api/_tj/screenshot" -o "$out/chat.png"
assert_png "$out/chat.png"
"$tj" review --target "$api"
starts_left draft send
stop_gui
echo "Linux GTK tests passed"

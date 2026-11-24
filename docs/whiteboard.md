# Collaborative whiteboard (`<Native name="Whiteboard" />`)

TinyJump supports an opt-in collaborative drawing board with **0 bytes added to `tj-core`**:

1. **Topic storage:** Persist strokes as `@server` objects (`BoardStroke`) and query them per room
   via `BoardStroke.all(`${room}:`)`, streaming only `BoardStroke/<room>:*` changes to clients.
2. **Rendering & pointer capture:** Mount `<Native name="Whiteboard" ... />`, which renders
   normalized `0..1000` `[x0, y0, x1, y1, ...]` coordinate strokes on a DPR-scaled canvas and
   emits finalized strokes via `onStroke`.
3. **Typed helpers:** `tj add whiteboard` installs pure-`.tj` palette, validation, and
   `strokeKey(room, seq)` helpers from `mindmakr/tj-plugins`.

See [examples/collab-studio/app.tj](../examples/collab-studio/app.tj) (`cargo tj dev examples/collab-studio/app.tj`).

## `<Native name="Whiteboard" ... />` contract

### Props

| Prop | Type | Default | Meaning |
|---|---|---|---|
| `strokes` | `Stroke[]` | `[]` | Array of `{ id?, author?, color?, width?, points: number[] }` (`points` is flat normalized `0..1000` coordinate pairs; capped at 500 strokes / 200 points each). |
| `color` | `string` | `"#d9480f"` | Active pen color (`#rgb` or `#rrggbb` hex). |
| `width` | `number` | `3` | Active pen width (`1..24`). |
| `readOnly` | `boolean` | `false` | Disables local pointer drawing when `true`. |
| `background` | `string` | `"#ffffff"` | Canvas background color (`#rgb` or `#rrggbb` hex). |

### Events

| Event | Value | Meaning |
|---|---|---|
| `onStroke` | `{ color, width, points }` | Emitted on pointer release with validated color, clamped width, and downsampled `0..1000` integer coordinates (`<= 120` pairs). |

## Host setup

- **Web (`hosts/web/plugins/whiteboard.js`):** Plain ES2017 `<canvas>` component (zero dependencies).
  `tj build --web` and `tj dev` bundle and wire it **automatically** when compiled bytecode
  mounts `<Native name="Whiteboard" ... />`. Apps without `Whiteboard` pay **0 bytes**.
- **Native hosts:** Register `"Whiteboard"` via `TinyJump.registerComponent("Whiteboard", ...)`
  implementing the same props and `onStroke` contract (`docs/host-protocol.md`).

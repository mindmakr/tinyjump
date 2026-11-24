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
| `strokeColor` | `string` | `"#111114"` | Active pen color (`#rgb` or `#rrggbb` hex). Note: `width` and `height` remain standard layout box props. |
| `strokeWidth` | `number` | `4` | Active pen thickness (`1..24`). |
| `readOnly` | `boolean` | `false` | Disables local pointer drawing and toolbar buttons when `true`. |
| `readOnlyHint` | `string` | `"Read-only board"` | Hint text shown over the canvas when `readOnly` is `true`. |
| `background` | `string` | `"#ffffff"` | Canvas background color (`#rgb` or `#rrggbb` hex). Also used by the built-in Eraser tool. |
| `controls` | `boolean` | `true` | Renders the built-in toolbar (Pen, Eraser, color swatches, Fine/Med/Bold sizes, plus Undo/Clear when `onUndo`/`onClear` or `showUndo`/`showClear` are provided). |
| `palette` | `string[]` | 6-color ink/rust/teal/blue/violet/red set | Optional hex swatch list shown in the toolbar (up to 8 colors). |
| `showUndo` | `boolean` | auto when `onUndo` bound | Forces showing the toolbar Undo button. |
| `showClear` | `boolean` | auto when `onClear` bound | Forces showing the toolbar Clear button. |

### Events

| Event | Value | Meaning |
|---|---|---|
| `onStroke` | `{ color, width, points }` | Emitted on pointer release with active tool color (canvas background in Eraser mode), clamped width, and downsampled `0..1000` integer coordinates (`<= 120` pairs). |
| `onColorChange` | `string` | Optional notification emitted when the user picks a swatch in the built-in toolbar. |
| `onWidthChange` | `number` | Optional notification emitted when the user selects Fine/Med/Bold in the built-in toolbar. |
| `onUndo` | `null` | Emitted when the user clicks the toolbar **Undo** button. |
| `onClear` | `null` | Emitted when the user clicks the toolbar **Clear** button. |

## Host setup

- **Web (`hosts/web/plugins/whiteboard.js`):** Plain ES2017 `<canvas>` component (zero dependencies).
  `tj build --web` and `tj dev` bundle and wire it **automatically** when compiled bytecode
  mounts `<Native name="Whiteboard" ... />`. Apps without `Whiteboard` pay **0 bytes**.
- **Native hosts:** Register `"Whiteboard"` via `TinyJump.registerComponent("Whiteboard", ...)`
  implementing the same props and `onStroke` contract (`docs/host-protocol.md`).

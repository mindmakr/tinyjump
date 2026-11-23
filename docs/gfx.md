# Opt-in GPU Graphics & 3D (`tj-gfx`)

TinyJump keeps 2D apps tiny by making GPU 3D rendering strictly **opt-in** (`docs/vision.md`, Goal 8). Apps that do not declare `<Scene>` or `@frame` ship **0 bytes** of 3D renderer code.

## Declaring a 3D scene

3D scenes live in the shared UI catalog (`crates/tj-core/src/ui/catalog.rs`) and diff through the exact same `Output::Patch` stream as 2D widgets:

```tsx
object Stage {
  angle: number = 0
  @frame step(dt: number) {
    this.angle += dt * 1.2
  }
}

function App() {
  const s = Stage.get("main")
  return <Scene id="viewport" clear="#0f172a" ambient={0.28} height={360}>
    <Camera position={[0, 2, 5]} target={[0, 0, 0]} fov={45} />
    <Light kind="directional" direction={[-0.4, -1.0, -0.5]} color="#ffffff" intensity={1.2} />
    <Mesh
      id="cube"
      geometry="box"
      rotation={[0.3, s.angle, 0]}
      color="#ef4444"
      onPress={() => console.log("clicked cube")}
    />
  </Scene>
}
```

### Components

| Component | Props | Events | Children |
|---|---|---|---|
| `<Scene>` | `clear`, `ambient`, `antialias`, plus common layout/style props (`width`, `height`, `grow`, `radius`, `border`, `id`) | `onPointerDown`, `onPointerMove`, `onPointerUp` | `<Camera>`, `<Light>`, `<Mesh>` |
| `<Mesh>` | `geometry` (`"box"`, `"sphere"`, `"plane"`, `"pyramid"`, `"cylinder"`), `position` (`[x,y,z]`), `rotation` (`[rx,ry,rz]` radians), `scale` (`number` or `[sx,sy,sz]`), `color` (`#rrggbb`), `metallic` (`0..1`), `roughness` (`0..1`), `wireframe`, `visible` | `onPress` | Nested `<Mesh>` (inherits parent transform) |
| `<Camera>` | `position`, `target`, `up`, `fov` (degrees), `near`, `far`, `projection` (`"perspective"` \| `"orthographic"`) | — | none |
| `<Light>` | `kind` (`"directional"` \| `"point"` \| `"ambient"`), `position`, `direction`, `color`, `intensity` | — | none |

### Frame loop (`@frame`)

- Mark a synchronous method on a plain (device) object with `@frame`:
  ```tsx
  @frame step(dt: number) { this.angle += dt }
  ```
- Hosts send `{ "type": "frame", "dt": 0.016 }` (`dt` in seconds, finite and `>= 0`).
- `@frame` is forbidden on `@server` objects, cannot use `await`, and cannot combine with `@every`, `@allow`, `@exclusive`, `@invariant`, or `@needs`.
- Trig builtins `Math.sin`, `Math.cos`, `Math.tan`, and `Math.atan2` are available in `tj-core`.

## Renderers

### Native Rust (`crates/tj-gfx` on `wgpu`)

`crates/tj-gfx` is a standalone opt-in Rust crate built on `wgpu` (Vulkan, Metal, D3D12, GLES 3, WebGPU). Because neither `tj-core`, `tj-wasm`, `tj-android`, `tj-ffi`, nor `tj-cli` depends on it, non-3D builds pay **0 bytes**.

- `SceneGraph::apply(&ops)` ingests `tj_core::runtime::protocol::PatchOp` batches directly.
- `SceneGraph::pick(scene_id, x, y, w, h)` ray-casts pointer coordinates against visible meshes (with depth occlusion and flat `plane` intersection) and returns the hit `<Mesh>` node id when it handles `onPress`.
- `GfxRenderer::from_device(device, queue, format)` renders onto any host `wgpu::TextureView` via `render_to_view(&scene, &view, w, h)` (default Android/iOS/macOS/Linux host apps stay 0 KB until a host embeds `tj-gfx`). Note: `antialias` currently applies to the WebGL web host; `wgpu` renders at `sample_count: 1`.
- `GfxRenderer::new_headless()` + `render_rgba` / `render_png` renders depth-buffered 3D scenes offscreen (preferring hardware GPU with software fallback, swizzling BGRA surfaces to RGBA automatically) for automated screenshots and golden-image diffs.

Run its tests (including live GPU / software-fallback rendering and PNG verification):

```sh
cargo test --manifest-path crates/tj-gfx/Cargo.toml
```

### Web (`hosts/web/plugins/gfx.js`)

When an app uses `<Scene>` or `@frame`, `tj dev`, `tj serve --web`, and `tj build --web` automatically load `hosts/web/plugins/gfx.js` (plain ES2017 WebGL2 with automatic WebGL1 / Canvas 2D fallback) and attach it via `start({ ..., gfx: createGfxPlugin({ animate: hasFrame }) })`. The `requestAnimationFrame` loop runs only when the app declares `@frame` (static scenes re-render only on patches and viewport resize), WebGL resources are freed on `<Scene>` unmount, and apps without `<Scene>` or `@frame` never download or bundle `gfx-plugin.js`.

> **Persistence note:** `@frame` mutations follow the same durable-object turn semantics as regular methods (so `@invariant` rollback and crash durability hold consistently). Native hosts using `FileStore` with 60 FPS `@frame` loops should batch or debounce disk flushes at the host `Store` layer if targeting low-end flash storage.

## Testing 3D apps deterministically

`tj test` supports deterministic `"frame"` steps in `.trace.jsonl` files:

```json
{"do": "frame", "dt": 0.016, "count": 60}
```

See [examples/scene3d/app.tj](../examples/scene3d/app.tj) and [examples/scene3d/tests/scene3d.trace.jsonl](../examples/scene3d/tests/scene3d.trace.jsonl).

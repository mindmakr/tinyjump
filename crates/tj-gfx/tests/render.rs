use tj_core::{compile, Input, MemStore, Options, Output, Runtime};
use tj_gfx::{GfxRenderer, SceneGraph};

const SCENE_APP: &str = r##"
object Stage {
  angle: number = 0
  hits: number = 0
  paused: boolean = false

  @frame step(dt: number) {
    if (!this.paused) {
      this.angle += dt * 1.5
    }
  }

  hit() { this.hits += 1 }
  togglePause() { this.paused = !this.paused }
}

function App() {
  const s = Stage.get("main")
  return <Column padding={16} gap={8}>
    <Text id="status">Hits: {s.hits}</Text>
    <Scene id="viewport" clear="#0b1020" ambient={0.25} width={320} height={240}>
      <Camera position={[0, 1.5, 4.5]} target={[0, 0, 0]} fov={45} />
      <Light kind="directional" direction={[-0.4, -1.0, -0.5]} color="#ffffff" intensity={1.2} />
      <Light kind="point" position={[2, 2, 2]} color="#60a5fa" intensity={1.5} />
      <Mesh
        id="cube"
        geometry="box"
        position={[0, 0, 0]}
        rotation={[0.2, s.angle, 0]}
        scale={1.4}
        color="#ef4444"
        metallic={0.2}
        roughness={0.4}
        onPress={() => s.hit()}
      >
        <Mesh
          id="satellite"
          geometry="sphere"
          position={[1.5, 0.3, 0]}
          scale={0.45}
          color="#22c55e"
        />
      </Mesh>
      <Mesh geometry="plane" position={[0, -1.1, 0]} scale={[6, 1, 6]} color="#1e293b" />
    </Scene>
  </Column>
}
"##;

fn apply_patches(graph: &mut SceneGraph, outputs: &[Output]) {
    for out in outputs {
        if let Output::Patch { ops } = out {
            graph.apply(ops);
        }
    }
}

#[test]
fn scene_graph_patches_picking_and_frame_hook_work_end_to_end() {
    let prog = compile(SCENE_APP).expect("scene app compiles");
    assert!(prog.uses_scene());
    assert!(prog.has_frame());

    let (mut rt, boot_out) = Runtime::boot(prog, Box::new(MemStore::default()), Options::new(1000.0, 1)).expect("boot");
    let mut graph = SceneGraph::new();
    apply_patches(&mut graph, &boot_out);

    let scenes = graph.scene_ids();
    assert_eq!(scenes.len(), 1, "expected 1 <Scene> node");
    let sid = scenes[0];

    let resolved0 = graph.resolve(sid, 320, 240).expect("resolved scene");
    assert_eq!(resolved0.meshes.len(), 3, "cube + nested satellite + floor plane");
    assert_eq!(resolved0.lights.len(), 2);

    // Center pixel hits the red cube (`onPress`); NaN coordinates or non-clickable floor miss.
    assert!(graph.pick(sid, f32::NAN, 120.0, 320, 240).is_none());
    assert!(graph.pick(sid, 160.0, 230.0, 320, 240).is_none(), "non-clickable floor plane returns None");
    let hit_id = graph.pick(sid, 160.0, 120.0, 320, 240).expect("center ray hits cube");
    assert_eq!(hit_id, resolved0.meshes[0].id);
    assert!(resolved0.meshes[0].clickable);

    // Clicking the picked mesh fires `onPress` in tj-core!
    let ui_out = rt.handle(
        1016.0,
        Input::Ui {
            target: tj_core::Target::Node { node: hit_id },
            event: "onPress".into(),
            value: serde_json::Value::Null,
            reply: Some(1),
        },
    );
    apply_patches(&mut graph, &ui_out);
    let snap = rt.inspect();
    let stage = snap["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["object"] == "Stage/main")
        .expect("Stage/main in inspect");
    assert_eq!(stage["state"]["hits"], serde_json::json!(1));

    // Stepping `@frame(dt)` updates angle and emits patched mesh rotation.
    let frame_out = rt.handle(1500.0, Input::Frame { dt: 0.5 });
    apply_patches(&mut graph, &frame_out);
    let resolved1 = graph.resolve(sid, 320, 240).expect("resolved after frame");
    assert_ne!(resolved0.meshes[0].world, resolved1.meshes[0].world, "cube rotated on @frame");
    assert_ne!(resolved0.meshes[1].world, resolved1.meshes[1].world, "child satellite inherited parent rotation");
}

#[test]
fn wgpu_renders_3d_scene_pixels_and_valid_png() {
    let mut renderer = match GfxRenderer::new_headless() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("skipping GPU pixel test (no wgpu adapter in environment): {e}");
            return;
        }
    };
    eprintln!("tj-gfx adapter: {} ({})", renderer.adapter_name(), renderer.adapter_backend());

    let prog = compile(SCENE_APP).expect("scene app compiles");
    let (mut rt, boot_out) = Runtime::boot(prog, Box::new(MemStore::default()), Options::new(1000.0, 1)).expect("boot");
    let mut graph = SceneGraph::new();
    apply_patches(&mut graph, &boot_out);
    let sid = graph.scene_ids()[0];

    let rgba0 = renderer.render_rgba(&graph, sid, 160, 120).expect("render frame 0");
    assert_eq!(rgba0.len(), 160 * 120 * 4);

    // Center pixel must show the lit red cube (r significantly brighter than g/b and clear color).
    let center = (60 * 160 + 80) * 4;
    let (r, g, b, a) = (rgba0[center], rgba0[center + 1], rgba0[center + 2], rgba0[center + 3]);
    assert_eq!(a, 255);
    assert!(r > g.saturating_add(40) && r > b.saturating_add(40), "expected lit red cube at center, got rgba=({r},{g},{b},{a})");

    // Corner pixel should be the dark clear color, distinct from the center cube.
    let corner = (2 * 160 + 2) * 4;
    assert_ne!(&rgba0[corner..corner + 3], &rgba0[center..center + 3]);

    // Step animation and re-render: frame must visibly change.
    let frame_out = rt.handle(1500.0, Input::Frame { dt: 0.6 });
    apply_patches(&mut graph, &frame_out);
    let rgba1 = renderer.render_rgba(&graph, sid, 160, 120).expect("render frame 1");
    let diff_pixels = rgba0.chunks_exact(4).zip(rgba1.chunks_exact(4)).filter(|(p, q)| p != q).count();
    assert!(diff_pixels > 50, "expected animation to change pixels, only {diff_pixels} changed");

    // PNG capture produces a valid PNG stream.
    let png = renderer.render_png(&graph, sid, 160, 120).expect("render png");
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(png.len() > 100);
}

//! Patch-driven 3D scene graph consuming `tj_core::runtime::protocol::PatchOp`.

use crate::math::{parse_hex_color, Mat4, Ray, Vec3};
use crate::mesh::Geometry;
use serde_json::{Map, Value as Json};
use std::collections::HashMap;
use tj_core::runtime::protocol::PatchOp;

pub const MAX_LIGHTS: usize = 8;

#[derive(Debug, Clone)]
struct SceneNode {
    kind: String,
    props: Map<String, Json>,
    events: Vec<String>,
    children: Vec<u32>,
}

/// Retains the subset of UI patches needed to resolve `<Scene>` subtrees and pick `<Mesh>` nodes.
#[derive(Debug, Default)]
pub struct SceneGraph {
    nodes: HashMap<u32, SceneNode>,
    root_children: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct ResolvedLight {
    /// Directional vector (pointing from light, `is_point = false`) or world position (`is_point = true`).
    pub pos_or_dir: [f32; 3],
    pub is_point: bool,
    pub color: [f32; 3],
    pub intensity: f32,
}

#[derive(Debug, Clone)]
pub struct ResolvedMesh {
    pub id: u32,
    pub geometry: Geometry,
    pub world: Mat4,
    pub color: [f32; 3],
    pub metallic: f32,
    pub roughness: f32,
    pub wireframe: bool,
    pub clickable: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedScene {
    pub id: u32,
    pub clear_color: [f32; 3],
    pub ambient_color: [f32; 3],
    pub camera_pos: Vec3,
    pub view_proj: Mat4,
    pub lights: Vec<ResolvedLight>,
    pub meshes: Vec<ResolvedMesh>,
}

impl SceneGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a batch of `Output::Patch` operations from `tj-core`.
    pub fn apply(&mut self, ops: &[PatchOp]) {
        for op in ops {
            match op {
                PatchOp::Create { id, kind, props, events } => {
                    self.nodes.insert(
                        *id,
                        SceneNode {
                            kind: kind.clone(),
                            props: props.clone(),
                            events: events.clone(),
                            children: Vec::new(),
                        },
                    );
                }
                PatchOp::Update { id, props, events } => {
                    if let Some(node) = self.nodes.get_mut(id) {
                        for (k, v) in props {
                            if v.is_null() {
                                node.props.remove(k);
                            } else {
                                node.props.insert(k.clone(), v.clone());
                            }
                        }
                        if let Some(ev) = events {
                            node.events = ev.clone();
                        }
                    }
                }
                PatchOp::Children { id, ids } => {
                    if *id == 0 {
                        self.root_children = ids.clone();
                    } else if let Some(node) = self.nodes.get_mut(id) {
                        node.children = ids.clone();
                    }
                }
                PatchOp::Remove { id } => {
                    self.nodes.remove(id);
                }
            }
        }
    }

    /// Lists all active `<Scene>` node ids reachable from the root container in tree order.
    pub fn scene_ids(&self) -> Vec<u32> {
        let mut out = Vec::new();
        self.collect_scenes(&self.root_children, &mut out, 0);
        out
    }

    fn collect_scenes(&self, ids: &[u32], out: &mut Vec<u32>, depth: usize) {
        if depth > 64 {
            return;
        }
        for &id in ids {
            if let Some(n) = self.nodes.get(&id) {
                if n.kind == "Scene" {
                    out.push(id);
                } else {
                    self.collect_scenes(&n.children, out, depth + 1);
                }
            }
        }
    }

    /// Resolves a `<Scene>` subtree into camera matrices, lights, and world-transformed meshes.
    pub fn resolve(&self, scene_id: u32, width: u32, height: u32) -> Option<ResolvedScene> {
        let scene = self.nodes.get(&scene_id)?;
        if scene.kind != "Scene" {
            return None;
        }
        let aspect = (width.max(1) as f32) / (height.max(1) as f32);
        let clear_color = scene
            .props
            .get("clear")
            .or_else(|| scene.props.get("background"))
            .and_then(Json::as_str)
            .map_or([0.009, 0.015, 0.032], |s| parse_hex_color(s, [0.009, 0.015, 0.032]));
        let base_ambient = prop_f32(&scene.props, "ambient", 0.25).clamp(0.0, 2.0);
        let mut ambient_rgb = [base_ambient, base_ambient, base_ambient];

        let mut camera_pos = Vec3::new(0.0, 2.0, 5.0);
        let mut camera_target = Vec3::ZERO;
        let mut camera_up = Vec3::UP;
        let mut camera_fov = 45.0f32;
        let mut camera_near = 0.1f32;
        let mut camera_far = 100.0f32;
        let mut ortho = false;
        let mut saw_camera = false;

        let mut lights = Vec::new();
        let mut saw_light = false;
        let mut meshes = Vec::new();

        for &cid in &scene.children {
            let Some(child) = self.nodes.get(&cid) else { continue };
            match child.kind.as_str() {
                "Camera" if !saw_camera => {
                    saw_camera = true;
                    camera_pos = prop_vec3(&child.props, "position", camera_pos);
                    camera_target = prop_vec3(&child.props, "target", camera_target);
                    camera_up = prop_vec3(&child.props, "up", camera_up);
                    camera_fov = prop_f32(&child.props, "fov", camera_fov);
                    camera_near = prop_f32(&child.props, "near", camera_near);
                    camera_far = prop_f32(&child.props, "far", camera_far);
                    ortho = child.props.get("projection").and_then(Json::as_str) == Some("orthographic");
                }
                "Light" => {
                    saw_light = true;
                    let kind = child.props.get("kind").and_then(Json::as_str).unwrap_or("directional");
                    let color = child
                        .props
                        .get("color")
                        .and_then(Json::as_str)
                        .map_or([1.0, 1.0, 1.0], |s| parse_hex_color(s, [1.0, 1.0, 1.0]));
                    let intensity = prop_f32(&child.props, "intensity", 1.0).clamp(0.0, 20.0);
                    if kind == "ambient" {
                        ambient_rgb[0] += color[0] * intensity;
                        ambient_rgb[1] += color[1] * intensity;
                        ambient_rgb[2] += color[2] * intensity;
                    } else if lights.len() < MAX_LIGHTS {
                        let is_point = kind == "point";
                        let pos_or_dir = if is_point {
                            prop_vec3(&child.props, "position", Vec3::new(2.0, 4.0, 3.0)).to_array()
                        } else {
                            prop_vec3(&child.props, "direction", Vec3::new(-0.5, -1.0, -0.6)).normalize().to_array()
                        };
                        lights.push(ResolvedLight { pos_or_dir, is_point, color, intensity });
                    }
                }
                "Mesh" => {
                    self.collect_meshes(cid, Mat4::IDENTITY, &mut meshes, 0);
                }
                _ => {}
            }
        }

        if !saw_light {
            // Sensible default key light when a scene declares no `<Light>` elements at all.
            lights.push(ResolvedLight {
                pos_or_dir: Vec3::new(-0.5, -1.0, -0.6).normalize().to_array(),
                is_point: false,
                color: [1.0, 1.0, 1.0],
                intensity: 1.1,
            });
        }

        let view = Mat4::look_at_rh(camera_pos, camera_target, camera_up);
        let proj = if ortho {
            let dist = (camera_target - camera_pos).length().max(1.0);
            Mat4::orthographic_rh(dist * 0.5, aspect, camera_near, camera_far)
        } else {
            Mat4::perspective_rh(camera_fov.to_radians(), aspect, camera_near, camera_far)
        };

        Some(ResolvedScene {
            id: scene_id,
            clear_color,
            ambient_color: ambient_rgb,
            camera_pos,
            view_proj: proj * view,
            lights,
            meshes,
        })
    }

    fn collect_meshes(&self, id: u32, parent_world: Mat4, out: &mut Vec<ResolvedMesh>, depth: usize) {
        if depth > 64 {
            return;
        }
        let Some(node) = self.nodes.get(&id) else { return };
        if node.kind != "Mesh" {
            return;
        }
        if node.props.get("visible").and_then(Json::as_bool) == Some(false) {
            return;
        }
        let pos = prop_vec3(&node.props, "position", Vec3::ZERO);
        let rot = prop_vec3(&node.props, "rotation", Vec3::ZERO);
        let scale = prop_scale(&node.props, "scale");
        let local = Mat4::trs(pos, rot, scale);
        let world = parent_world * local;

        let geometry = node.props.get("geometry").and_then(Json::as_str).map_or(Geometry::Box, Geometry::from_name);
        let color = node
            .props
            .get("color")
            .and_then(Json::as_str)
            .map_or([0.6, 0.7, 0.85], |s| parse_hex_color(s, [0.6, 0.7, 0.85]));
        let metallic = prop_f32(&node.props, "metallic", 0.1).clamp(0.0, 1.0);
        let roughness = prop_f32(&node.props, "roughness", 0.55).clamp(0.05, 1.0);
        let wireframe = node.props.get("wireframe").and_then(Json::as_bool).unwrap_or(false);
        let clickable = node.events.iter().any(|e| e == "onPress");

        out.push(ResolvedMesh { id, geometry, world, color, metallic, roughness, wireframe, clickable });
        for &cid in &node.children {
            self.collect_meshes(cid, world, out, depth + 1);
        }
    }

    /// Ray-casts screen pixel `(x, y)` in a `width x height` `<Scene>` viewport and returns
    /// the closest visible `<Mesh>` node id if it handles `onPress` (occluded by closer opaque meshes).
    pub fn pick(&self, scene_id: u32, x: f32, y: f32, width: u32, height: u32) -> Option<u32> {
        if !x.is_finite() || !y.is_finite() || width == 0 || height == 0 {
            return None;
        }
        let scene = self.resolve(scene_id, width, height)?;
        let inv_vp = scene.view_proj.inverse()?;
        let ndc_x = (2.0 * x / width as f32) - 1.0;
        let ndc_y = 1.0 - (2.0 * y / height as f32);
        let near_world = inv_vp.transform_point(Vec3::new(ndc_x, ndc_y, 0.0));
        let far_world = inv_vp.transform_point(Vec3::new(ndc_x, ndc_y, 1.0));
        let world_dir = (far_world - near_world).normalize();

        let mut best: Option<(f32, u32, bool)> = None;
        for m in &scene.meshes {
            let Some(inv_m) = m.world.inverse() else { continue };
            let local_origin = inv_m.transform_point(near_world);
            let local_far = inv_m.transform_point(near_world + world_dir);
            let local_dir = local_far - local_origin;
            let ray = Ray { origin: local_origin, dir: local_dir };
            let hit = match m.geometry {
                Geometry::Plane => ray.intersect_unit_plane(),
                _ => ray.intersect_unit_box(),
            };
            if let Some(t) = hit {
                if best.is_none_or(|(bt, _, _)| t < bt) {
                    best = Some((t, m.id, m.clickable));
                }
            }
        }
        best.and_then(|(_, id, clickable)| clickable.then_some(id))
    }
}

fn finite_f32(v: Option<&Json>, default: f32) -> f32 {
    v.and_then(Json::as_f64)
        .map(|n| n as f32)
        .filter(|n| n.is_finite())
        .unwrap_or(default)
}

fn prop_f32(props: &Map<String, Json>, key: &str, default: f32) -> f32 {
    finite_f32(props.get(key), default)
}

fn prop_vec3(props: &Map<String, Json>, key: &str, default: Vec3) -> Vec3 {
    let Some(arr) = props.get(key).and_then(Json::as_array) else {
        return default;
    };
    Vec3::new(
        finite_f32(arr.first(), default.x),
        finite_f32(arr.get(1), default.y),
        finite_f32(arr.get(2), default.z),
    )
}

fn prop_scale(props: &Map<String, Json>, key: &str) -> Vec3 {
    if let Some(s) = props.get(key).and_then(Json::as_f64).map(|n| n as f32).filter(|n| n.is_finite()) {
        return Vec3::new(s, s, s);
    }
    prop_vec3(props, key, Vec3::ONE)
}

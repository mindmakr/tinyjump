//! Optional GPU 3D graphics module for tinyjump hosts (`tj-gfx` on `wgpu`).
//!
//! Consumes the exact [`tj_core::runtime::protocol::PatchOp`] stream emitted by `tj-core` for
//! `<Scene>`, `<Mesh>`, `<Camera>`, and `<Light>` nodes and renders them on `wgpu`
//! (Vulkan / Metal / D3D12 / GLES 3 / WebGPU) — both to live surfaces and offscreen RGBA/PNG.
//! Hosts opt in by depending on `tj-gfx`, so apps without 3D pay 0 bytes.

pub mod math;
pub mod mesh;
pub mod png;
pub mod renderer;
pub mod scene;

pub use math::{Mat4, Ray, Vec3};
pub use mesh::Geometry;
pub use png::encode_png_rgba;
pub use renderer::{GfxError, GfxRenderer};
pub use scene::{ResolvedLight, ResolvedMesh, ResolvedScene, SceneGraph};

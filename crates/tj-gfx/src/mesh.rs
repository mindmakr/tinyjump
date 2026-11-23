//! Procedural indexed 3D meshes (`box`, `sphere`, `plane`, `pyramid`, `cylinder`).

use crate::math::Vec3;
use std::f32::consts::TAU;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Geometry {
    Box,
    Sphere,
    Plane,
    Pyramid,
    Cylinder,
}

impl Geometry {
    pub fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "sphere" => Self::Sphere,
            "plane" => Self::Plane,
            "pyramid" => Self::Pyramid,
            "cylinder" => Self::Cylinder,
            _ => Self::Box,
        }
    }

    pub const ALL: [Geometry; 5] = [
        Geometry::Box,
        Geometry::Sphere,
        Geometry::Plane,
        Geometry::Pyramid,
        Geometry::Cylinder,
    ];
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
}

impl Vertex {
    pub const STRIDE: usize = 24;

    pub fn push_bytes(&self, out: &mut Vec<u8>) {
        for v in self.pos.iter().chain(self.normal.iter()) {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
}

#[derive(Debug, Clone)]
pub struct MeshData {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn vertex_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.vertices.len() * Vertex::STRIDE);
        for v in &self.vertices {
            v.push_bytes(&mut out);
        }
        out
    }

    pub fn index_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.indices.len() * 4);
        for &i in &self.indices {
            out.extend_from_slice(&i.to_le_bytes());
        }
        out
    }
}

pub fn build_mesh(kind: Geometry) -> MeshData {
    match kind {
        Geometry::Box => unit_box(),
        Geometry::Sphere => unit_sphere(24, 16),
        Geometry::Plane => unit_plane(),
        Geometry::Pyramid => unit_pyramid(),
        Geometry::Cylinder => unit_cylinder(24),
    }
}

fn push_quad(verts: &mut Vec<Vertex>, idx: &mut Vec<u32>, pts: [[f32; 3]; 4], normal: [f32; 3]) {
    let base = verts.len() as u32;
    for pos in pts {
        verts.push(Vertex { pos, normal });
    }
    idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn unit_box() -> MeshData {
    let mut v = Vec::with_capacity(24);
    let mut i = Vec::with_capacity(36);
    let h = 0.5;
    // +Z (front)
    push_quad(&mut v, &mut i, [[-h, -h, h], [h, -h, h], [h, h, h], [-h, h, h]], [0.0, 0.0, 1.0]);
    // -Z (back)
    push_quad(&mut v, &mut i, [[h, -h, -h], [-h, -h, -h], [-h, h, -h], [h, h, -h]], [0.0, 0.0, -1.0]);
    // +X (right)
    push_quad(&mut v, &mut i, [[h, -h, h], [h, -h, -h], [h, h, -h], [h, h, h]], [1.0, 0.0, 0.0]);
    // -X (left)
    push_quad(&mut v, &mut i, [[-h, -h, -h], [-h, -h, h], [-h, h, h], [-h, h, -h]], [-1.0, 0.0, 0.0]);
    // +Y (top)
    push_quad(&mut v, &mut i, [[-h, h, h], [h, h, h], [h, h, -h], [-h, h, -h]], [0.0, 1.0, 0.0]);
    // -Y (bottom)
    push_quad(&mut v, &mut i, [[-h, -h, -h], [h, -h, -h], [h, -h, h], [-h, -h, h]], [0.0, -1.0, 0.0]);
    MeshData { vertices: v, indices: i }
}

fn unit_plane() -> MeshData {
    let mut v = Vec::with_capacity(8);
    let mut i = Vec::with_capacity(12);
    let h = 0.5;
    // Double-sided horizontal plane at y = 0.
    push_quad(&mut v, &mut i, [[-h, 0.0, h], [h, 0.0, h], [h, 0.0, -h], [-h, 0.0, -h]], [0.0, 1.0, 0.0]);
    push_quad(&mut v, &mut i, [[-h, 0.0, -h], [h, 0.0, -h], [h, 0.0, h], [-h, 0.0, h]], [0.0, -1.0, 0.0]);
    MeshData { vertices: v, indices: i }
}

fn unit_sphere(sectors: u32, stacks: u32) -> MeshData {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let r = 0.5;
    for st in 0..=stacks {
        let v = st as f32 / stacks as f32;
        let phi = v * std::f32::consts::PI;
        let (sin_p, cos_p) = phi.sin_cos();
        for se in 0..=sectors {
            let u = se as f32 / sectors as f32;
            let theta = u * TAU;
            let (sin_t, cos_t) = theta.sin_cos();
            let nx = sin_p * cos_t;
            let ny = cos_p;
            let nz = sin_p * sin_t;
            vertices.push(Vertex { pos: [nx * r, ny * r, nz * r], normal: [nx, ny, nz] });
        }
    }
    let stride = sectors + 1;
    for st in 0..stacks {
        for se in 0..sectors {
            let a = st * stride + se;
            let b = a + stride;
            indices.extend_from_slice(&[a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    MeshData { vertices, indices }
}

fn unit_pyramid() -> MeshData {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let h = 0.5;
    let apex = Vec3::new(0.0, h, 0.0);
    let corners = [
        Vec3::new(-h, -h, h),
        Vec3::new(h, -h, h),
        Vec3::new(h, -h, -h),
        Vec3::new(-h, -h, -h),
    ];
    for s in 0..4 {
        let p0 = corners[s];
        let p1 = corners[(s + 1) % 4];
        let n = (p1 - p0).cross(apex - p0).normalize().to_array();
        let base = vertices.len() as u32;
        vertices.push(Vertex { pos: p0.to_array(), normal: n });
        vertices.push(Vertex { pos: p1.to_array(), normal: n });
        vertices.push(Vertex { pos: apex.to_array(), normal: n });
        indices.extend_from_slice(&[base, base + 1, base + 2]);
    }
    // Base quad (-Y)
    push_quad(&mut vertices, &mut indices, [[-h, -h, -h], [h, -h, -h], [h, -h, h], [-h, -h, h]], [0.0, -1.0, 0.0]);
    MeshData { vertices, indices }
}

fn unit_cylinder(segments: u32) -> MeshData {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let r = 0.5;
    let h = 0.5;
    // Side walls
    for seg in 0..segments {
        let a0 = (seg as f32 / segments as f32) * TAU;
        let a1 = ((seg + 1) as f32 / segments as f32) * TAU;
        let (s0, c0) = a0.sin_cos();
        let (s1, c1) = a1.sin_cos();
        let base = vertices.len() as u32;
        vertices.push(Vertex { pos: [c0 * r, -h, s0 * r], normal: [c0, 0.0, s0] });
        vertices.push(Vertex { pos: [c1 * r, -h, s1 * r], normal: [c1, 0.0, s1] });
        vertices.push(Vertex { pos: [c1 * r, h, s1 * r], normal: [c1, 0.0, s1] });
        vertices.push(Vertex { pos: [c0 * r, h, s0 * r], normal: [c0, 0.0, s0] });
        indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
    }
    // Top cap (+Y)
    let top_center = vertices.len() as u32;
    vertices.push(Vertex { pos: [0.0, h, 0.0], normal: [0.0, 1.0, 0.0] });
    for seg in 0..segments {
        let a = (seg as f32 / segments as f32) * TAU;
        let (s, c) = a.sin_cos();
        vertices.push(Vertex { pos: [c * r, h, s * r], normal: [0.0, 1.0, 0.0] });
    }
    for seg in 0..segments {
        let curr = top_center + 1 + seg;
        let next = top_center + 1 + (seg + 1) % segments;
        indices.extend_from_slice(&[top_center, next, curr]);
    }
    // Bottom cap (-Y)
    let bot_center = vertices.len() as u32;
    vertices.push(Vertex { pos: [0.0, -h, 0.0], normal: [0.0, -1.0, 0.0] });
    for seg in 0..segments {
        let a = (seg as f32 / segments as f32) * TAU;
        let (s, c) = a.sin_cos();
        vertices.push(Vertex { pos: [c * r, -h, s * r], normal: [0.0, -1.0, 0.0] });
    }
    for seg in 0..segments {
        let curr = bot_center + 1 + seg;
        let next = bot_center + 1 + (seg + 1) % segments;
        indices.extend_from_slice(&[bot_center, curr, next]);
    }
    MeshData { vertices, indices }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_geometry_produces_valid_indexed_triangles() {
        for g in Geometry::ALL {
            let m = build_mesh(g);
            assert!(!m.vertices.is_empty());
            assert!(!m.indices.is_empty() && m.indices.len() % 3 == 0);
            for &idx in &m.indices {
                assert!((idx as usize) < m.vertices.len(), "{g:?}: index out of bounds");
            }
            for v in &m.vertices {
                let len = Vec3::new(v.normal[0], v.normal[1], v.normal[2]).length();
                assert!((len - 1.0).abs() < 1e-3, "{g:?}: non-unit normal {len}");
            }
        }
    }
}

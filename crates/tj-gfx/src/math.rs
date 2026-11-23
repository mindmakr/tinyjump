//! Pure-Rust 3D math for `tj-gfx` (column-major right-handed coordinates, `wgpu` NDC `z in [0, 1]`).

use std::ops::{Add, Mul, Sub};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Add for Vec3 {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl Sub for Vec3 {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 0.0 };
    pub const ONE: Vec3 = Vec3 { x: 1.0, y: 1.0, z: 1.0 };
    pub const UP: Vec3 = Vec3 { x: 0.0, y: 1.0, z: 0.0 };

    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    pub fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }

    pub fn scale(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s)
    }

    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: Self) -> Self {
        Self::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    pub fn normalize(self) -> Self {
        let len = self.length();
        if len > 1e-6 {
            self.scale(1.0 / len)
        } else {
            Self::new(0.0, 1.0, 0.0)
        }
    }
}

/// Column-major 4x4 matrix (`cols[col][row]`), matching WGSL `mat4x4<f32>`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4 {
    pub cols: [[f32; 4]; 4],
}

impl Mul for Mat4 {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        let mut cols = [[0.0f32; 4]; 4];
        for (c, col) in cols.iter_mut().enumerate() {
            for (r, slot) in col.iter_mut().enumerate() {
                *slot = self.cols[0][r] * rhs.cols[c][0]
                    + self.cols[1][r] * rhs.cols[c][1]
                    + self.cols[2][r] * rhs.cols[c][2]
                    + self.cols[3][r] * rhs.cols[c][3];
            }
        }
        Self { cols }
    }
}

impl Mat4 {
    pub const IDENTITY: Mat4 = Mat4 {
        cols: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };

    pub fn to_bytes(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        let mut i = 0;
        for col in &self.cols {
            for &v in col {
                out[i..i + 4].copy_from_slice(&v.to_le_bytes());
                i += 4;
            }
        }
        out
    }

    pub fn mul_vec4(self, v: [f32; 4]) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        for (r, slot) in out.iter_mut().enumerate() {
            *slot = self.cols[0][r] * v[0]
                + self.cols[1][r] * v[1]
                + self.cols[2][r] * v[2]
                + self.cols[3][r] * v[3];
        }
        out
    }

    pub fn transform_point(self, p: Vec3) -> Vec3 {
        let v = self.mul_vec4([p.x, p.y, p.z, 1.0]);
        let inv_w = if v[3].abs() > 1e-6 { 1.0 / v[3] } else { 1.0 };
        Vec3::new(v[0] * inv_w, v[1] * inv_w, v[2] * inv_w)
    }

    pub fn translation(t: Vec3) -> Self {
        Self {
            cols: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [t.x, t.y, t.z, 1.0],
            ],
        }
    }

    pub fn scale(s: Vec3) -> Self {
        Self {
            cols: [
                [s.x, 0.0, 0.0, 0.0],
                [0.0, s.y, 0.0, 0.0],
                [0.0, 0.0, s.z, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    pub fn rotation_x(rad: f32) -> Self {
        let (s, c) = rad.sin_cos();
        Self {
            cols: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, c, s, 0.0],
                [0.0, -s, c, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    pub fn rotation_y(rad: f32) -> Self {
        let (s, c) = rad.sin_cos();
        Self {
            cols: [
                [c, 0.0, -s, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [s, 0.0, c, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    pub fn rotation_z(rad: f32) -> Self {
        let (s, c) = rad.sin_cos();
        Self {
            cols: [
                [c, s, 0.0, 0.0],
                [-s, c, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    /// Composes `T * Rz * Ry * Rx * S`.
    pub fn trs(pos: Vec3, rot: Vec3, scale: Vec3) -> Self {
        let r = Self::rotation_z(rot.z) * Self::rotation_y(rot.y) * Self::rotation_x(rot.x);
        Self::translation(pos) * r * Self::scale(scale)
    }

    /// Right-handed look-at view matrix.
    pub fn look_at_rh(eye: Vec3, target: Vec3, up: Vec3) -> Self {
        let forward = target - eye;
        let f = if forward.length() > 1e-6 { forward.normalize() } else { Vec3::new(0.0, 0.0, -1.0) };
        let up_n = if up.length() > 1e-6 { up.normalize() } else { Vec3::UP };
        let mut s = f.cross(up_n);
        if s.length() <= 1e-6 {
            // Eye looking straight along `up`: choose an orthogonal fallback up vector.
            s = f.cross(Vec3::new(1.0, 0.0, 0.0));
        }
        let s = s.normalize();
        let u = s.cross(f);
        Self {
            cols: [
                [s.x, u.x, -f.x, 0.0],
                [s.y, u.y, -f.y, 0.0],
                [s.z, u.z, -f.z, 0.0],
                [-s.dot(eye), -u.dot(eye), f.dot(eye), 1.0],
            ],
        }
    }

    /// Right-handed perspective projection into `wgpu` NDC (`x,y in [-1,1]`, `z in [0,1]`).
    pub fn perspective_rh(fov_y_rad: f32, aspect: f32, near: f32, far: f32) -> Self {
        let near = near.max(1e-4);
        let far = far.max(near + 1e-3);
        let aspect = aspect.max(1e-4);
        let f = 1.0 / (fov_y_rad.clamp(0.05, std::f32::consts::PI - 0.05) * 0.5).tan();
        let nf = 1.0 / (near - far);
        Self {
            cols: [
                [f / aspect, 0.0, 0.0, 0.0],
                [0.0, f, 0.0, 0.0],
                [0.0, 0.0, far * nf, -1.0],
                [0.0, 0.0, far * near * nf, 0.0],
            ],
        }
    }

    /// Right-handed orthographic projection into `wgpu` NDC (`z in [0,1]`).
    pub fn orthographic_rh(half_height: f32, aspect: f32, near: f32, far: f32) -> Self {
        let hh = half_height.max(1e-3);
        let hw = hh * aspect.max(1e-4);
        let far = far.max(near + 1e-3);
        let nf = 1.0 / (near - far);
        Self {
            cols: [
                [1.0 / hw, 0.0, 0.0, 0.0],
                [0.0, 1.0 / hh, 0.0, 0.0],
                [0.0, 0.0, nf, 0.0],
                [0.0, 0.0, near * nf, 1.0],
            ],
        }
    }

    /// Inverse-transpose of the upper-left 3x3, embedded in a `Mat4` for WGSL normal transforms.
    pub fn normal_matrix(&self) -> Self {
        let a = self.cols[0][0];
        let b = self.cols[1][0];
        let c = self.cols[2][0];
        let d = self.cols[0][1];
        let e = self.cols[1][1];
        let f = self.cols[2][1];
        let g = self.cols[0][2];
        let h = self.cols[1][2];
        let i = self.cols[2][2];

        let c00 = e * i - f * h;
        let c01 = -(d * i - f * g);
        let c02 = d * h - e * g;
        let c10 = -(b * i - c * h);
        let c11 = a * i - c * g;
        let c12 = -(a * h - b * g);
        let c20 = b * f - c * e;
        let c21 = -(a * f - c * d);
        let c22 = a * e - b * d;

        let det = a * c00 + b * c01 + c * c02;
        if det.abs() <= 1e-8 {
            return Self::IDENTITY;
        }
        let inv = 1.0 / det;
        // Cofactor matrix divided by det IS inverse-transpose!
        Self {
            cols: [
                [c00 * inv, c10 * inv, c20 * inv, 0.0],
                [c01 * inv, c11 * inv, c21 * inv, 0.0],
                [c02 * inv, c12 * inv, c22 * inv, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    /// Full 4x4 matrix inverse via Gauss-Jordan elimination (for unprojecting picking rays).
    pub fn inverse(&self) -> Option<Self> {
        let mut a = [[0.0f32; 8]; 4];
        for (r, row) in a.iter_mut().enumerate() {
            for (c, slot) in row.iter_mut().take(4).enumerate() {
                *slot = self.cols[c][r];
            }
            row[r + 4] = 1.0;
        }
        for col in 0..4 {
            let mut pivot = col;
            let mut max_val = a[col][col].abs();
            for (r, row) in a.iter().enumerate().skip(col + 1) {
                if row[col].abs() > max_val {
                    max_val = row[col].abs();
                    pivot = r;
                }
            }
            if max_val <= 1e-8 {
                return None;
            }
            if pivot != col {
                a.swap(col, pivot);
            }
            let inv = 1.0 / a[col][col];
            for v in &mut a[col] {
                *v *= inv;
            }
            for r in 0..4 {
                if r != col {
                    let factor = a[r][col];
                    let pivot_row = a[col];
                    for (dst, src) in a[r].iter_mut().zip(pivot_row.iter()) {
                        *dst -= factor * src;
                    }
                }
            }
        }
        let mut cols = [[0.0f32; 4]; 4];
        for r in 0..4 {
            for (c, col) in cols.iter_mut().enumerate() {
                col[r] = a[r][c + 4];
            }
        }
        Some(Self { cols })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Ray {
    pub origin: Vec3,
    pub dir: Vec3,
}

impl Ray {
    fn is_finite(self) -> bool {
        self.origin.x.is_finite()
            && self.origin.y.is_finite()
            && self.origin.z.is_finite()
            && self.dir.x.is_finite()
            && self.dir.y.is_finite()
            && self.dir.z.is_finite()
    }

    /// Intersects ray with unit axis-aligned box `[-0.5, 0.5]^3`, returning hit distance `t >= 0`.
    pub fn intersect_unit_box(self) -> Option<f32> {
        if !self.is_finite() {
            return None;
        }
        let mut tmin = 0.0f32;
        let mut tmax = f32::INFINITY;
        for (o, d) in [(self.origin.x, self.dir.x), (self.origin.y, self.dir.y), (self.origin.z, self.dir.z)] {
            if d.abs() <= 1e-7 {
                if !(-0.5..=0.5).contains(&o) {
                    return None;
                }
            } else {
                let mut t1 = (-0.5 - o) / d;
                let mut t2 = (0.5 - o) / d;
                if !t1.is_finite() || !t2.is_finite() {
                    return None;
                }
                if t1 > t2 {
                    std::mem::swap(&mut t1, &mut t2);
                }
                tmin = tmin.max(t1);
                tmax = tmax.min(t2);
                if tmin > tmax {
                    return None;
                }
            }
        }
        if tmax >= 0.0 {
            Some(tmin)
        } else {
            None
        }
    }

    /// Intersects ray with unit horizontal plane `y = 0, x in [-0.5, 0.5], z in [-0.5, 0.5]`.
    pub fn intersect_unit_plane(self) -> Option<f32> {
        if !self.is_finite() || self.dir.y.abs() <= 1e-7 {
            return None;
        }
        let t = -self.origin.y / self.dir.y;
        if !t.is_finite() || t < 0.0 {
            return None;
        }
        let x = self.origin.x + self.dir.x * t;
        let z = self.origin.z + self.dir.z * t;
        ((-0.5..=0.5).contains(&x) && (-0.5..=0.5).contains(&z)).then_some(t)
    }
}

/// Parses `#rgb` or `#rrggbb` into linear RGB `[r, g, b]` in `0..=1`.
pub fn parse_hex_color(raw: &str, fallback: [f32; 3]) -> [f32; 3] {
    let s = raw.trim();
    let Some(hex) = s.strip_prefix('#') else {
        return fallback;
    };
    if !hex.is_ascii() {
        return fallback;
    }
    let srgb_to_linear = |u: u8| {
        let c = f32::from(u) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    match hex.len() {
        3 => {
            let mut b = [0u8; 3];
            for (i, ch) in hex.bytes().enumerate() {
                let Some(d) = (ch as char).to_digit(16) else { return fallback };
                b[i] = (d * 17) as u8;
            }
            [srgb_to_linear(b[0]), srgb_to_linear(b[1]), srgb_to_linear(b[2])]
        }
        6 | 8 => {
            let Ok(r) = u8::from_str_radix(&hex[0..2], 16) else { return fallback };
            let Ok(g) = u8::from_str_radix(&hex[2..4], 16) else { return fallback };
            let Ok(b) = u8::from_str_radix(&hex[4..6], 16) else { return fallback };
            [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b)]
        }
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trs_inverse_round_trips() {
        let m = Mat4::trs(Vec3::new(1.5, -2.0, 3.0), Vec3::new(0.4, 0.7, -0.2), Vec3::new(2.0, 1.5, 0.5));
        let inv = m.inverse().expect("invertible");
        let prod = m * inv;
        for c in 0..4 {
            for r in 0..4 {
                let want = if c == r { 1.0 } else { 0.0 };
                assert!((prod.cols[c][r] - want).abs() < 1e-4, "mismatch at ({c},{r}): {}", prod.cols[c][r]);
            }
        }
    }

    #[test]
    fn perspective_maps_origin_into_ndc() {
        let v = Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::UP);
        let p = Mat4::perspective_rh(45.0f32.to_radians(), 1.0, 0.1, 100.0);
        let ndc = (p * v).transform_point(Vec3::ZERO);
        assert!(ndc.x.abs() < 1e-5 && ndc.y.abs() < 1e-5);
        assert!(ndc.z > 0.0 && ndc.z < 1.0, "wgpu NDC z in [0,1], got {}", ndc.z);
    }

    #[test]
    fn parse_hex_converts_srgb() {
        let white = parse_hex_color("#fff", [0.0; 3]);
        assert!((white[0] - 1.0).abs() < 1e-5 && (white[1] - 1.0).abs() < 1e-5 && (white[2] - 1.0).abs() < 1e-5);
        let black = parse_hex_color("#000000", [1.0; 3]);
        assert!(black[0] == 0.0 && black[1] == 0.0 && black[2] == 0.0);
        assert_eq!(parse_hex_color("not-a-color", [0.2, 0.3, 0.4]), [0.2, 0.3, 0.4]);
        // Non-ASCII 6-byte and 8-byte UTF-8 strings must not panic on slicing.
        assert_eq!(parse_hex_color("#€€", [0.2, 0.3, 0.4]), [0.2, 0.3, 0.4]);
        assert_eq!(parse_hex_color("#🔥🔥", [0.2, 0.3, 0.4]), [0.2, 0.3, 0.4]);
    }

    #[test]
    fn non_finite_rays_and_flat_planes_behave_safely() {
        let nan_ray = Ray { origin: Vec3::new(f32::NAN, 0.0, 5.0), dir: Vec3::new(0.0, 0.0, -1.0) };
        assert!(nan_ray.intersect_unit_box().is_none());
        assert!(nan_ray.intersect_unit_plane().is_none());
        let plane_hit = Ray { origin: Vec3::new(0.0, 1.0, 0.0), dir: Vec3::new(0.0, -1.0, 0.0) };
        assert!((plane_hit.intersect_unit_plane().unwrap() - 1.0).abs() < 1e-5);
        let plane_miss = Ray { origin: Vec3::new(0.0, 0.4, 2.0), dir: Vec3::new(0.0, 0.0, -1.0) };
        assert!(plane_miss.intersect_unit_plane().is_none());
    }
}

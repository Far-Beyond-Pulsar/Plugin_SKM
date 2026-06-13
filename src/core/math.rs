//! Minimal 3D math: vectors and column-major 4x4 matrices.
//!
//! Only the operations needed to evaluate a skeletal pose and build a
//! camera view-projection matrix for the viewport renderer are implemented.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 0.0 };
    pub const ONE: Vec3 = Vec3 { x: 1.0, y: 1.0, z: 1.0 };

    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    pub fn lerp(self, other: Vec3, t: f32) -> Vec3 {
        Vec3::new(
            self.x + (other.x - self.x) * t,
            self.y + (other.y - self.y) * t,
            self.z + (other.z - self.z) * t,
        )
    }

    pub fn sub(self, other: Vec3) -> Vec3 {
        Vec3::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }

    pub fn add(self, other: Vec3) -> Vec3 {
        Vec3::new(self.x + other.x, self.y + other.y, self.z + other.z)
    }

    pub fn scale(self, s: f32) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }

    pub fn cross(self, other: Vec3) -> Vec3 {
        Vec3::new(
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
    }

    pub fn dot(self, other: Vec3) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    pub fn length(self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    pub fn normalize(self) -> Vec3 {
        let len = self.length();
        if len < 1e-8 {
            self
        } else {
            self.scale(1.0 / len)
        }
    }

    pub fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
}

/// Column-major 4x4 matrix, stored as 16 floats (matches WGSL `mat4x4<f32>` layout).
#[derive(Clone, Copy, Debug)]
pub struct Mat4(pub [f32; 16]);

impl Mat4 {
    pub const IDENTITY: Mat4 = Mat4([
        1.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        0.0, 0.0, 0.0, 1.0,
    ]);

    pub fn from_translation(t: Vec3) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        m.0[12] = t.x;
        m.0[13] = t.y;
        m.0[14] = t.z;
        m
    }

    pub fn from_scale(s: Vec3) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        m.0[0] = s.x;
        m.0[5] = s.y;
        m.0[10] = s.z;
        m
    }

    /// Rotation matrix from XYZ Euler angles given in degrees, applied Z then Y then X.
    pub fn from_euler_xyz_deg(rot: Vec3) -> Mat4 {
        let (x, y, z) = (rot.x.to_radians(), rot.y.to_radians(), rot.z.to_radians());
        let (sx, cx) = (x.sin(), x.cos());
        let (sy, cy) = (y.sin(), y.cos());
        let (sz, cz) = (z.sin(), z.cos());

        // Rx * Ry * Rz
        Mat4([
            cy * cz, cy * sz, -sy, 0.0,
            sx * sy * cz - cx * sz, sx * sy * sz + cx * cz, sx * cy, 0.0,
            cx * sy * cz + sx * sz, cx * sy * sz - sx * cz, cx * cy, 0.0,
            0.0, 0.0, 0.0, 1.0,
        ])
    }

    /// Multiply two column-major matrices: `self * rhs`.
    pub fn mul(&self, rhs: &Mat4) -> Mat4 {
        let a = &self.0;
        let b = &rhs.0;
        let mut out = [0.0f32; 16];
        for col in 0..4 {
            for row in 0..4 {
                let mut sum = 0.0;
                for k in 0..4 {
                    sum += a[k * 4 + row] * b[col * 4 + k];
                }
                out[col * 4 + row] = sum;
            }
        }
        Mat4(out)
    }

    pub fn transform_point(&self, p: Vec3) -> Vec3 {
        let m = &self.0;
        Vec3::new(
            m[0] * p.x + m[4] * p.y + m[8] * p.z + m[12],
            m[1] * p.x + m[5] * p.y + m[9] * p.z + m[13],
            m[2] * p.x + m[6] * p.y + m[10] * p.z + m[14],
        )
    }

    /// Transform a point to clip space, returning `(x, y, z, w)` before the
    /// perspective divide.
    pub fn transform_clip(&self, p: Vec3) -> (f32, f32, f32, f32) {
        let m = &self.0;
        (
            m[0] * p.x + m[4] * p.y + m[8] * p.z + m[12],
            m[1] * p.x + m[5] * p.y + m[9] * p.z + m[13],
            m[2] * p.x + m[6] * p.y + m[10] * p.z + m[14],
            m[3] * p.x + m[7] * p.y + m[11] * p.z + m[15],
        )
    }

    /// Right-handed perspective projection (WGPU clip space: depth 0..1).
    pub fn perspective(fov_y_deg: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let f = 1.0 / (fov_y_deg.to_radians() * 0.5).tan();
        let nf_range = far - near;
        Mat4([
            f / aspect, 0.0, 0.0, 0.0,
            0.0, f, 0.0, 0.0,
            0.0, 0.0, far / nf_range, 1.0,
            0.0, 0.0, -(far * near) / nf_range, 0.0,
        ])
    }

    /// Inverse of this matrix, via Gauss-Jordan elimination with partial
    /// pivoting. Used to reconstruct world-space positions from depth for
    /// TAA history reprojection.
    pub fn inverse(&self) -> Mat4 {
        let src = &self.0;
        // `a[row]` holds the augmented `[A | I]` row: columns 0..4 are `A`,
        // columns 4..8 are the identity, which becomes `A^-1` once `A`'s
        // side has been reduced to the identity.
        let mut a = [[0f32; 8]; 4];
        for row in 0..4 {
            for col in 0..4 {
                a[row][col] = src[col * 4 + row];
            }
            a[row][4 + row] = 1.0;
        }

        for col in 0..4 {
            let mut pivot = col;
            for row in (col + 1)..4 {
                if a[row][col].abs() > a[pivot][col].abs() {
                    pivot = row;
                }
            }
            a.swap(col, pivot);

            let div = a[col][col];
            if div.abs() > 1e-12 {
                for c in 0..8 {
                    a[col][c] /= div;
                }
            }
            for row in 0..4 {
                if row != col {
                    let factor = a[row][col];
                    for c in 0..8 {
                        a[row][c] -= factor * a[col][c];
                    }
                }
            }
        }

        let mut out = [0f32; 16];
        for row in 0..4 {
            for col in 0..4 {
                out[col * 4 + row] = a[row][4 + col];
            }
        }
        Mat4(out)
    }

    /// Right-handed look-at view matrix.
    pub fn look_at(eye: Vec3, target: Vec3, up: Vec3) -> Mat4 {
        let f = target.sub(eye).normalize(); // forward
        let s = f.cross(up).normalize(); // right
        let u = s.cross(f); // up

        Mat4([
            s.x, u.x, -f.x, 0.0,
            s.y, u.y, -f.y, 0.0,
            s.z, u.z, -f.z, 0.0,
            -s.x * eye.x - s.y * eye.y - s.z * eye.z,
            -u.x * eye.x - u.y * eye.y - u.z * eye.z,
            f.x * eye.x + f.y * eye.y + f.z * eye.z,
            1.0,
        ])
    }
}

/// A local transform expressed as translation + Euler rotation (degrees) + scale.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub translation: Vec3,
    /// Euler angles in degrees (XYZ order).
    pub rotation: Vec3,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Transform = Transform {
        translation: Vec3::ZERO,
        rotation: Vec3::ZERO,
        scale: Vec3::ONE,
    };

    pub fn to_matrix(&self) -> Mat4 {
        Mat4::from_translation(self.translation)
            .mul(&Mat4::from_euler_xyz_deg(self.rotation))
            .mul(&Mat4::from_scale(self.scale))
    }

    /// Component-wise linear interpolation. Sufficient for small Euler deltas
    /// between adjacent keyframes; not a substitute for quaternion slerp on
    /// large rotations.
    pub fn lerp(&self, other: &Transform, t: f32) -> Transform {
        Transform {
            translation: self.translation.lerp(other.translation, t),
            rotation: self.rotation.lerp(other.rotation, t),
            scale: self.scale.lerp(other.scale, t),
        }
    }
}

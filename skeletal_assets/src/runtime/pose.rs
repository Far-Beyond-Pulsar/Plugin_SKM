//! Runtime pose data: index-based local-space bone transforms, curves and root
//! motion, with the blend, additive and space-conversion operations every
//! graph node is built from.
//!
//! Everything here is deterministic: no hash-map iteration order reaches a
//! result (curves are ordered maps) and all math is plain `f32`.

use std::collections::BTreeMap;

use glam::{Mat4, Quat, Vec3};

use crate::math::Transform;
use crate::skeleton::Skeleton;

/// Translation, rotation and scale of one bone relative to its parent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoneTransform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl BoneTransform {
    pub const IDENTITY: Self = Self { translation: Vec3::ZERO, rotation: Quat::IDENTITY, scale: Vec3::ONE };

    pub fn new(translation: Vec3, rotation: Quat, scale: Vec3) -> Self {
        Self { translation, rotation, scale }
    }

    /// From the asset format's Euler-angle transform, through its matrix so the
    /// rotation convention is exactly the one the editor preview uses.
    pub fn from_asset(transform: &Transform) -> Self {
        Self::from_mat4(Mat4::from_cols_array(&transform.to_matrix().0))
    }

    pub fn from_mat4(matrix: Mat4) -> Self {
        let (scale, rotation, translation) = matrix.to_scale_rotation_translation();
        Self { translation, rotation: rotation.normalize(), scale }
    }

    pub fn to_mat4(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    /// Interpolates toward `other`: lerp for translation and scale, shortest-arc
    /// slerp for rotation. `w` is not clamped.
    pub fn blend(&self, other: &Self, w: f32) -> Self {
        Self {
            translation: self.translation.lerp(other.translation, w),
            rotation: slerp_short(self.rotation, other.rotation, w),
            scale: self.scale.lerp(other.scale, w),
        }
    }

    pub fn is_finite(&self) -> bool {
        self.translation.is_finite() && self.rotation.is_finite() && self.scale.is_finite()
    }
}

impl Default for BoneTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Shortest-arc spherical interpolation that never produces NaN for
/// near-identical or opposite inputs.
pub fn slerp_short(a: Quat, b: Quat, w: f32) -> Quat {
    let b = if a.dot(b) < 0.0 { -b } else { b };
    let q = if a.dot(b) > 0.9995 { a.lerp(b, w) } else { a.slerp(b, w) };
    let q = q.normalize();
    if q.is_finite() { q } else { a }
}

/// Root motion over one update: a rigid delta in the actor's space, composed on
/// the right (`object_next = object * delta`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RootMotion {
    pub translation: Vec3,
    pub rotation: Quat,
}

impl RootMotion {
    pub const IDENTITY: Self = Self { translation: Vec3::ZERO, rotation: Quat::IDENTITY };

    pub fn is_identity(&self) -> bool {
        self.translation == Vec3::ZERO && self.rotation == Quat::IDENTITY
    }

    pub fn to_mat4(&self) -> Mat4 {
        Mat4::from_rotation_translation(self.rotation, self.translation)
    }

    pub fn from_mat4(m: Mat4) -> Self {
        let (_, rotation, translation) = m.to_scale_rotation_translation();
        Self { translation, rotation: rotation.normalize() }
    }

    /// `self` then `next`.
    pub fn then(&self, next: &Self) -> Self {
        Self { translation: self.translation + self.rotation * next.translation, rotation: (self.rotation * next.rotation).normalize() }
    }

    pub fn inverse(&self) -> Self {
        let r = self.rotation.inverse();
        Self { translation: r * -self.translation, rotation: r }
    }

    pub fn blend(&self, other: &Self, w: f32) -> Self {
        Self { translation: self.translation.lerp(other.translation, w), rotation: slerp_short(self.rotation, other.rotation, w) }
    }

    /// Scaled by `w` toward the identity.
    pub fn scaled(&self, w: f32) -> Self {
        Self::IDENTITY.blend(self, w)
    }
}

impl Default for RootMotion {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Named scalar channels carried with a pose.
pub type Curves = BTreeMap<String, f32>;

/// Blend two curve sets; an absent curve counts as 0.
pub fn blend_curves(a: &Curves, b: &Curves, w: f32) -> Curves {
    let mut out = Curves::new();
    for (name, &va) in a {
        out.insert(name.clone(), va + (b.get(name).copied().unwrap_or(0.0) - va) * w);
    }
    for (name, &vb) in b {
        out.entry(name.clone()).or_insert(vb * w);
    }
    out
}

/// What a pose's bone transforms mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoseKind {
    /// A complete local-space pose.
    Full,
    /// A difference applied in each bone's parent-local frame.
    AdditiveLocal,
    /// A difference applied in mesh space.
    AdditiveMesh,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pose {
    pub bones: Vec<BoneTransform>,
    pub curves: Curves,
    pub root_motion: RootMotion,
    pub kind: PoseKind,
}

impl Pose {
    pub fn from_bones(bones: Vec<BoneTransform>) -> Self {
        Self { bones, curves: Curves::new(), root_motion: RootMotion::IDENTITY, kind: PoseKind::Full }
    }

    pub fn blend(&self, other: &Self, w: f32) -> Self {
        debug_assert_eq!(self.bones.len(), other.bones.len());
        Self {
            bones: self.bones.iter().zip(&other.bones).map(|(a, b)| a.blend(b, w)).collect(),
            curves: blend_curves(&self.curves, &other.curves, w),
            root_motion: self.root_motion.blend(&other.root_motion, w),
            kind: self.kind,
        }
    }

    pub fn is_finite(&self) -> bool {
        self.bones.iter().all(BoneTransform::is_finite)
    }
}

/// Weighted blend of several poses: weights are normalised to sum to 1 (all
/// zero selects the first), then poses blend pairwise in order, so the result
/// does not depend on anything but the input order.
pub fn blend_weighted(poses: &[(&Pose, f32)]) -> Option<Pose> {
    let total: f32 = poses.iter().map(|(_, w)| w.max(0.0)).sum();
    let mut it = poses.iter().filter(|(_, w)| *w > 0.0 || total <= 0.0);
    let (first, w0) = it.next().or_else(|| poses.first())?;
    if total <= 0.0 {
        return Some((*first).clone());
    }
    let mut acc = (*first).clone();
    let mut acc_w = w0.max(0.0);
    for (pose, w) in it {
        let w = w.max(0.0);
        if w <= 0.0 {
            continue;
        }
        acc = acc.blend(pose, w / (acc_w + w));
        acc_w += w;
    }
    Some(acc)
}

/// Index-based skeleton: parents precede children in `order`.
#[derive(Clone, Debug)]
pub struct RuntimeSkeleton {
    pub id: String,
    pub ids: Vec<String>,
    pub names: Vec<String>,
    pub parents: Vec<Option<usize>>,
    /// Topological order, parents first, ties by asset order.
    pub order: Vec<usize>,
    pub bind: Vec<BoneTransform>,
    index: std::collections::HashMap<String, usize>,
    name_index: std::collections::HashMap<String, usize>,
}

impl RuntimeSkeleton {
    pub fn new(id: impl Into<String>, skeleton: &Skeleton) -> Result<Self, String> {
        skeleton.validate().map_err(|e| format!("skeleton hierarchy is invalid: {e}"))?;
        let ids: Vec<String> = skeleton.bones.iter().map(|b| b.id.clone()).collect();
        let index: std::collections::HashMap<String, usize> = ids.iter().cloned().enumerate().map(|(i, id)| (id, i)).collect();
        let parents: Vec<Option<usize>> =
            skeleton.bones.iter().map(|b| b.parent.as_ref().and_then(|p| index.get(p).copied())).collect();
        let mut order = Vec::with_capacity(ids.len());
        let mut placed = vec![false; ids.len()];
        while order.len() < ids.len() {
            let before = order.len();
            for i in 0..ids.len() {
                if !placed[i] && parents[i].is_none_or(|p| placed[p]) {
                    placed[i] = true;
                    order.push(i);
                }
            }
            if order.len() == before {
                return Err("skeleton hierarchy has a cycle".into());
            }
        }
        let names: Vec<String> = skeleton.bones.iter().map(|b| b.name.clone()).collect();
        let name_index = names.iter().cloned().enumerate().map(|(i, n)| (n, i)).collect();
        Ok(Self {
            id: id.into(),
            names,
            bind: skeleton.bones.iter().map(|b| BoneTransform::from_asset(&b.bind_transform)).collect(),
            ids,
            parents,
            order,
            index,
            name_index,
        })
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// A bone by stable id.
    pub fn bone(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }

    /// A bone by stable id, falling back to its display name.
    pub fn bone_by_id_or_name(&self, key: &str) -> Option<usize> {
        self.bone(key).or_else(|| self.name_index.get(key).copied())
    }

    pub fn bind_pose(&self) -> Pose {
        Pose::from_bones(self.bind.clone())
    }

    pub fn depth(&self, bone: usize) -> usize {
        let mut d = 0;
        let mut at = self.parents[bone];
        while let Some(p) = at {
            d += 1;
            at = self.parents[p];
        }
        d
    }

    /// Whether `ancestor` is `bone` or one of its ancestors.
    pub fn is_ancestor_or_self(&self, ancestor: usize, bone: usize) -> bool {
        let mut at = Some(bone);
        while let Some(b) = at {
            if b == ancestor {
                return true;
            }
            at = self.parents[b];
        }
        false
    }

    /// Bones from `start` down to `end`, or `None` if `end` is not below `start`.
    pub fn chain(&self, start: usize, end: usize) -> Option<Vec<usize>> {
        let mut out = vec![end];
        let mut at = end;
        while at != start {
            at = self.parents[at]?;
            out.push(at);
        }
        out.reverse();
        Some(out)
    }

    pub fn children(&self, bone: usize) -> impl Iterator<Item = usize> + '_ {
        (0..self.len()).filter(move |&i| self.parents[i] == Some(bone))
    }

    /// Mesh-space matrices of every bone for a local pose.
    pub fn to_mesh_space(&self, local: &[BoneTransform]) -> Vec<Mat4> {
        let mut out = vec![Mat4::IDENTITY; self.len()];
        for &i in &self.order {
            let m = local[i].to_mat4();
            out[i] = match self.parents[i] {
                Some(p) => out[p] * m,
                None => m,
            };
        }
        out
    }

    /// Local transforms that produce `mesh` (the inverse of [`Self::to_mesh_space`]).
    pub fn from_mesh_space(&self, mesh: &[Mat4]) -> Vec<BoneTransform> {
        let mut out = vec![BoneTransform::IDENTITY; self.len()];
        for &i in &self.order {
            let local = match self.parents[i] {
                Some(p) => mesh[p].inverse() * mesh[i],
                None => mesh[i],
            };
            out[i] = BoneTransform::from_mat4(local);
        }
        out
    }

    /// Mesh-space matrix of one bone, composed along its ancestors only.
    pub fn bone_mesh_space(&self, local: &[BoneTransform], bone: usize) -> Mat4 {
        let mut chain = vec![bone];
        let mut at = bone;
        while let Some(p) = self.parents[at] {
            chain.push(p);
            at = p;
        }
        chain.iter().rev().fold(Mat4::IDENTITY, |acc, &b| acc * local[b].to_mat4())
    }

    /// Skinning-ready bone transforms: `mesh_space * inverse_bind`, using the
    /// skeleton's bind pose as the reference.
    pub fn inverse_bind(&self) -> Vec<Mat4> {
        self.to_mesh_space(&self.bind).into_iter().map(|m| m.inverse()).collect()
    }
}

/// Make an additive pose: `source` relative to `reference`.
pub fn make_additive(skel: &RuntimeSkeleton, source: &Pose, reference: &Pose, mesh_space: bool) -> Pose {
    let mut curves = Curves::new();
    for (name, &v) in &source.curves {
        curves.insert(name.clone(), v - reference.curves.get(name).copied().unwrap_or(0.0));
    }
    for (name, &v) in &reference.curves {
        curves.entry(name.clone()).or_insert(-v);
    }
    let bones = if mesh_space {
        let s = skel.to_mesh_space(&source.bones);
        let r = skel.to_mesh_space(&reference.bones);
        (0..skel.len())
            .map(|i| {
                let (ss, sr, st) = s[i].to_scale_rotation_translation();
                let (rs, rr, rt) = r[i].to_scale_rotation_translation();
                BoneTransform::new(st - rt, (sr * rr.inverse()).normalize(), safe_div(ss, rs))
            })
            .collect()
    } else {
        source
            .bones
            .iter()
            .zip(&reference.bones)
            .map(|(s, r)| BoneTransform::new(s.translation - r.translation, (r.rotation.inverse() * s.rotation).normalize(), safe_div(s.scale, r.scale)))
            .collect()
    };
    Pose {
        bones,
        curves,
        root_motion: RootMotion::IDENTITY,
        kind: if mesh_space { PoseKind::AdditiveMesh } else { PoseKind::AdditiveLocal },
    }
}

fn safe_div(a: Vec3, b: Vec3) -> Vec3 {
    Vec3::new(
        if b.x.abs() > 1e-8 { a.x / b.x } else { 1.0 },
        if b.y.abs() > 1e-8 { a.y / b.y } else { 1.0 },
        if b.z.abs() > 1e-8 { a.z / b.z } else { 1.0 },
    )
}

/// Apply `additive` onto `base` with strength `alpha`. Curves add scaled by
/// `alpha`; root motion keeps the base's.
pub fn apply_additive(skel: &RuntimeSkeleton, base: &Pose, additive: &Pose, alpha: f32) -> Pose {
    let alpha = alpha.clamp(0.0, 1.0);
    let scaled = |a: &BoneTransform| {
        BoneTransform::new(a.translation * alpha, slerp_short(Quat::IDENTITY, a.rotation, alpha), Vec3::ONE.lerp(a.scale, alpha))
    };
    let mut curves = base.curves.clone();
    for (name, &v) in &additive.curves {
        *curves.entry(name.clone()).or_insert(0.0) += v * alpha;
    }
    let bones = match additive.kind {
        PoseKind::AdditiveMesh => {
            let mut mesh = skel.to_mesh_space(&base.bones);
            for (i, m) in mesh.iter_mut().enumerate() {
                let (s, r, t) = m.to_scale_rotation_translation();
                let a = scaled(&additive.bones[i]);
                *m = Mat4::from_scale_rotation_translation(s * a.scale, (a.rotation * r).normalize(), t + a.translation);
            }
            skel.from_mesh_space(&mesh)
        }
        _ => base
            .bones
            .iter()
            .zip(&additive.bones)
            .map(|(b, a)| {
                let a = scaled(a);
                BoneTransform::new(b.translation + a.translation, (b.rotation * a.rotation).normalize(), b.scale * a.scale)
            })
            .collect(),
    };
    Pose { bones, curves, root_motion: base.root_motion, kind: PoseKind::Full }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::math::Vec3 as AssetVec3;
    use crate::skeleton::Bone;

    pub(crate) fn chain_skeleton(n: usize) -> Skeleton {
        Skeleton {
            bones: (0..n)
                .map(|i| Bone {
                    id: format!("b{i}"),
                    name: format!("bone_{i}"),
                    parent: (i > 0).then(|| format!("b{}", i - 1)),
                    bind_transform: Transform { translation: AssetVec3::new(0.0, if i == 0 { 0.0 } else { 1.0 }, 0.0), ..Transform::IDENTITY },
                })
                .collect(),
        }
    }

    #[test]
    fn mesh_space_round_trips() {
        let skel = RuntimeSkeleton::new("s", &chain_skeleton(4)).unwrap();
        let mut local = skel.bind.clone();
        local[1].rotation = Quat::from_rotation_z(0.4);
        local[2].rotation = Quat::from_rotation_x(-0.7);
        let mesh = skel.to_mesh_space(&local);
        let back = skel.from_mesh_space(&mesh);
        for (a, b) in local.iter().zip(&back) {
            assert!((a.translation - b.translation).length() < 1e-4);
            assert!(a.rotation.dot(b.rotation).abs() > 0.9999);
        }
    }

    #[test]
    fn additive_make_then_apply_reproduces_the_source() {
        let skel = RuntimeSkeleton::new("s", &chain_skeleton(3)).unwrap();
        let reference = skel.bind_pose();
        let mut source = skel.bind_pose();
        source.bones[1].rotation = Quat::from_rotation_y(0.5);
        source.bones[2].translation += Vec3::new(0.1, 0.0, 0.0);
        source.curves.insert("a".into(), 2.0);
        for mesh in [false, true] {
            let add = make_additive(&skel, &source, &reference, mesh);
            let out = apply_additive(&skel, &reference, &add, 1.0);
            for (a, b) in source.bones.iter().zip(&out.bones) {
                assert!((a.translation - b.translation).length() < 1e-4, "mesh={mesh}");
                assert!(a.rotation.dot(b.rotation).abs() > 0.9999, "mesh={mesh}");
            }
            assert!((out.curves["a"] - 2.0).abs() < 1e-6);
            let half = apply_additive(&skel, &reference, &add, 0.5);
            assert!(half.bones[2].translation.x > 0.04 && half.bones[2].translation.x < 0.06 || mesh);
        }
    }

    #[test]
    fn weighted_blend_normalises_and_is_order_stable() {
        let skel = RuntimeSkeleton::new("s", &chain_skeleton(2)).unwrap();
        let mut a = skel.bind_pose();
        let mut b = skel.bind_pose();
        a.bones[1].translation.x = 0.0;
        b.bones[1].translation.x = 4.0;
        let p = blend_weighted(&[(&a, 3.0), (&b, 1.0)]).unwrap();
        assert!((p.bones[1].translation.x - 1.0).abs() < 1e-5);
        let z = blend_weighted(&[(&a, 0.0), (&b, 0.0)]).unwrap();
        assert_eq!(z.bones[1].translation.x, 0.0);
        let again = blend_weighted(&[(&a, 3.0), (&b, 1.0)]).unwrap();
        assert_eq!(p, again);
    }

    #[test]
    fn curves_blend_with_absent_as_zero() {
        let mut a = Curves::new();
        a.insert("x".into(), 1.0);
        let mut b = Curves::new();
        b.insert("y".into(), 1.0);
        let c = blend_curves(&a, &b, 0.25);
        assert!((c["x"] - 0.75).abs() < 1e-6 && (c["y"] - 0.25).abs() < 1e-6);
    }
}

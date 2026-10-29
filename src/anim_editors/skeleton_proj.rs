//! A 2D front-view projection of a skeleton's bind pose, for the overlays of
//! the IK Rig, Retarget and IK Retargeter editors: where each bone is on a
//! canvas, which bone a pointer is over, and the bones' parent links.
//!
//! The editors draw chains, goals and mappings over this; the full skinned 3D
//! preview stays in the Sequence/Skeleton viewport.

use skeletal_assets::runtime::pose::RuntimeSkeleton;
use skeletal_assets::skeleton::Skeleton;

#[derive(Clone, Debug, PartialEq)]
pub struct ProjBone {
    pub id: String,
    pub name: String,
    pub parent: Option<usize>,
    /// Mesh-space position of the bone's origin in the bind pose.
    pub position: [f32; 3],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkeletonProjection {
    pub bones: Vec<ProjBone>,
    min: [f32; 2],
    max: [f32; 2],
}

impl SkeletonProjection {
    pub fn new(skeleton: &Skeleton) -> Self {
        let Ok(runtime) = RuntimeSkeleton::new("projection", skeleton) else { return Self::default() };
        let mesh = runtime.to_mesh_space(&runtime.bind_pose().bones);
        let mut bones = Vec::with_capacity(runtime.len());
        for i in 0..runtime.len() {
            let p = mesh[i].w_axis;
            let source = &skeleton.bones.iter().find(|b| runtime.bone(&b.id) == Some(i));
            bones.push(ProjBone {
                id: source.map(|b| b.id.clone()).unwrap_or_default(),
                name: source.map(|b| b.name.clone()).unwrap_or_default(),
                parent: runtime.parents[i],
                position: [p.x, p.y, p.z],
            });
        }
        let (mut min, mut max) = ([f32::MAX; 2], [f32::MIN; 2]);
        for b in &bones {
            for k in 0..2 {
                min[k] = min[k].min(b.position[k]);
                max[k] = max[k].max(b.position[k]);
            }
        }
        if bones.is_empty() {
            return Self::default();
        }
        Self { bones, min, max }
    }

    pub fn is_empty(&self) -> bool {
        self.bones.is_empty()
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.bones.iter().position(|b| b.id == id)
    }

    /// Pixel positions in a `width` x `height` box with `pad` around it. The
    /// skeleton is fitted keeping its aspect ratio; mesh +Y is up.
    pub fn layout(&self, width: f32, height: f32, pad: f32) -> Vec<(f32, f32)> {
        let span = [(self.max[0] - self.min[0]).max(1e-4), (self.max[1] - self.min[1]).max(1e-4)];
        let scale = ((width - 2.0 * pad) / span[0]).min((height - 2.0 * pad) / span[1]);
        let used = [span[0] * scale, span[1] * scale];
        let origin = [(width - used[0]) * 0.5, (height - used[1]) * 0.5];
        self.bones
            .iter()
            .map(|b| (origin[0] + (b.position[0] - self.min[0]) * scale, height - origin[1] - (b.position[1] - self.min[1]) * scale))
            .collect()
    }

    /// The bone nearest `(x, y)` within `radius` pixels.
    pub fn pick(&self, width: f32, height: f32, pad: f32, x: f32, y: f32, radius: f32) -> Option<String> {
        self.layout(width, height, pad)
            .iter()
            .enumerate()
            .map(|(i, (bx, by))| (((bx - x).powi(2) + (by - y).powi(2)).sqrt(), i))
            .filter(|(d, _)| *d <= radius)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, i)| self.bones[i].id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::super::ik_rig_model::tests::skeleton;
    use super::*;
    use skeletal_assets::math::{Transform, Vec3};

    fn leg() -> Skeleton {
        let mut s = skeleton();
        // pelvis up 1, thigh down 0.5, shin down 0.5, foot down 0.4; spine up 0.5
        for (id, t) in [("pelvis", [0.0, 1.0, 0.0]), ("thigh", [0.1, -0.1, 0.0]), ("shin", [0.0, -0.5, 0.0]), ("foot", [0.0, -0.4, 0.0]), ("spine", [0.0, 0.8, 0.0])] {
            s.bone_mut(id).unwrap().bind_transform = Transform { translation: Vec3::new(t[0], t[1], t[2]), ..Transform::IDENTITY };
        }
        s
    }

    #[test]
    fn bones_project_upright_and_fit_the_box() {
        let p = SkeletonProjection::new(&leg());
        assert_eq!(p.bones.len(), 6);
        let at = p.layout(200.0, 300.0, 20.0);
        let foot = at[p.index_of("foot").unwrap()];
        let spine = at[p.index_of("spine").unwrap()];
        assert!(foot.1 > spine.1, "feet are lower on screen than the spine top");
        for (x, y) in &at {
            assert!((19.9..=180.1).contains(x) && (19.9..=280.1).contains(y), "inside the padded box: {x},{y}");
        }
        assert!(p.bones[p.index_of("foot").unwrap()].parent.is_some());
    }

    #[test]
    fn picking_returns_the_nearest_bone_within_the_radius() {
        let p = SkeletonProjection::new(&leg());
        let at = p.layout(200.0, 300.0, 20.0);
        let (x, y) = at[p.index_of("shin").unwrap()];
        assert_eq!(p.pick(200.0, 300.0, 20.0, x + 2.0, y - 2.0, 8.0).as_deref(), Some("shin"));
        assert_eq!(p.pick(200.0, 300.0, 20.0, x + 300.0, y, 8.0), None);
    }

    #[test]
    fn an_empty_skeleton_projects_nothing() {
        let p = SkeletonProjection::new(&Skeleton::default());
        assert!(p.is_empty() && p.layout(100.0, 100.0, 5.0).is_empty());
    }
}

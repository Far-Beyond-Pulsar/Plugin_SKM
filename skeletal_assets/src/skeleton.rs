//! bone hierarchy and validated editing operations.

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use super::math::Transform;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bone {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    /// local bind pose, relative to the parent bone.
    pub bind_transform: Transform,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Skeleton {
    pub bones: Vec<Bone>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HierarchyError {
    EmptyId,
    EmptyName,
    DuplicateId(String),
    MissingBone(String),
    MissingParent { bone: String, parent: String },
    Cycle(String),
    HasChildren(String),
    InvalidTransform,
    UnrepresentableBindPose,
}

impl fmt::Display for HierarchyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyId => f.write_str("The bone ID cannot be empty."),
            Self::EmptyName => f.write_str("The name cannot be empty."),
            Self::DuplicateId(id) => write!(f, "Bone ID '{id}' already exists."),
            Self::MissingBone(id) => write!(f, "Bone '{id}' does not exist."),
            Self::MissingParent { bone, parent } => write!(f, "Bone '{bone}' has missing parent '{parent}'."),
            Self::Cycle(id) => write!(f, "Parenting '{id}' here would create a cycle."),
            Self::HasChildren(id) => write!(f, "Bone '{id}' has children. Use Delete Subtree."),
            Self::InvalidTransform => f.write_str("Transform components must be finite."),
            Self::UnrepresentableBindPose => f.write_str("Cannot preserve the bind pose: the transform is singular, contains shear, or loses precision."),
        }
    }
}

impl std::error::Error for HierarchyError {}

pub(crate) fn validate_transform(t: &Transform) -> Result<(), HierarchyError> {
    if [t.translation, t.rotation, t.scale]
        .iter()
        .flat_map(|v| v.to_array())
        .all(f32::is_finite)
    {
        Ok(())
    } else {
        Err(HierarchyError::InvalidTransform)
    }
}

fn preserves_bind_pose(candidate: &super::math::Mat4, original: &super::math::Mat4) -> bool {
    if !candidate.preserves_affine(original) {
        return false;
    }
    if candidate.0 == original.0 {
        return true;
    }
    // a changed candidate needs a reliable inverse to certify its bind-space error.
    let inverse = original.inverse();
    let bind = original.mul(&inverse);
    bind.approx_eq(&super::math::Mat4::IDENTITY, 2e-5)
        && candidate.mul(&inverse).preserves_affine(&bind)
}

impl Skeleton {
    pub fn bone(&self, id: &str) -> Option<&Bone> {
        self.bones.iter().find(|b| b.id == id)
    }

    pub fn bone_mut(&mut self, id: &str) -> Option<&mut Bone> {
        self.bones.iter_mut().find(|b| b.id == id)
    }

    pub fn root_bones(&self) -> Vec<&Bone> {
        self.bones.iter().filter(|b| b.parent.is_none()).collect()
    }

    pub fn children_of<'a>(&'a self, id: &str) -> Vec<&'a Bone> {
        self.bones
            .iter()
            .filter(|b| b.parent.as_deref() == Some(id))
            .collect()
    }

    /// validate public fields and deserialized data before editing them.
    pub fn validate(&self) -> Result<(), HierarchyError> {
        let mut indices = HashMap::with_capacity(self.bones.len());
        for (i, bone) in self.bones.iter().enumerate() {
            if bone.id.trim().is_empty() {
                return Err(HierarchyError::EmptyId);
            }
            if indices.insert(bone.id.as_str(), i).is_some() {
                return Err(HierarchyError::DuplicateId(bone.id.clone()));
            }
            if bone.name.trim().is_empty() {
                return Err(HierarchyError::EmptyName);
            }
            validate_transform(&bone.bind_transform)?;
        }
        let mut parents = Vec::with_capacity(self.bones.len());
        for bone in &self.bones {
            parents.push(match bone.parent.as_deref() {
                Some(parent) => {
                    Some(
                        *indices
                            .get(parent)
                            .ok_or_else(|| HierarchyError::MissingParent {
                                bone: bone.id.clone(),
                                parent: parent.into(),
                            })?,
                    )
                }
                None => None,
            });
        }
        // each chain is visited once, including disconnected cycles.
        let mut states = vec![0u8; self.bones.len()];
        for start in 0..self.bones.len() {
            let mut current = Some(start);
            while let Some(i) = current {
                match states[i] {
                    1 => return Err(HierarchyError::Cycle(self.bones[i].id.clone())),
                    2 => break,
                    _ => states[i] = 1,
                }
                current = parents[i];
            }
            current = Some(start);
            while let Some(i) = current {
                if states[i] != 1 {
                    break;
                }
                states[i] = 2;
                current = parents[i];
            }
        }
        Ok(())
    }

    /// iterative, bounded traversal; malformed components are still inspectable.
    pub fn depth_first(&self) -> Vec<(&Bone, usize)> {
        let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, bone) in self.bones.iter().enumerate() {
            if let Some(parent) = bone.parent.as_deref() {
                children.entry(parent).or_default().push(i);
            }
        }
        let mut visited = vec![false; self.bones.len()];
        let mut out = Vec::with_capacity(self.bones.len());
        let roots = self
            .bones
            .iter()
            .enumerate()
            .filter(|(_, b)| b.parent.is_none())
            .map(|(i, _)| i);
        let mut stack = Vec::new();
        for start in roots.chain(0..self.bones.len()) {
            if visited[start] {
                continue;
            }
            stack.push((start, 0));
            while let Some((i, depth)) = stack.pop() {
                if visited[i] {
                    continue;
                }
                visited[i] = true;
                let bone = &self.bones[i];
                out.push((bone, depth));
                if let Some(children) = children.get(bone.id.as_str()) {
                    stack.extend(children.iter().rev().map(|&child| (child, depth + 1)));
                }
            }
        }
        out
    }

    pub fn add_bone(&mut self, bone: Bone) -> Result<(), HierarchyError> {
        self.validate()?;
        if bone.id.trim().is_empty() {
            return Err(HierarchyError::EmptyId);
        }
        if bone.name.trim().is_empty() {
            return Err(HierarchyError::EmptyName);
        }
        if self.bone(&bone.id).is_some() {
            return Err(HierarchyError::DuplicateId(bone.id));
        }
        validate_transform(&bone.bind_transform)?;
        if bone.parent.as_deref() == Some(&bone.id) {
            return Err(HierarchyError::Cycle(bone.id));
        }
        if let Some(parent) = &bone.parent {
            if self.bone(parent).is_none() {
                return Err(HierarchyError::MissingParent {
                    bone: bone.id,
                    parent: parent.clone(),
                });
            }
        }
        self.bones.push(bone);
        Ok(())
    }

    /// change only the display name; tracks and joints keep their stable ids.
    pub fn rename_bone(&mut self, id: &str, name: String) -> Result<(), HierarchyError> {
        self.validate()?;
        if name.trim().is_empty() {
            return Err(HierarchyError::EmptyName);
        }
        self.bone_mut(id)
            .ok_or_else(|| HierarchyError::MissingBone(id.into()))?
            .name = name;
        Ok(())
    }

    pub fn set_bind_transform(
        &mut self,
        id: &str,
        transform: Transform,
    ) -> Result<(), HierarchyError> {
        self.validate()?;
        validate_transform(&transform)?;
        self.bone_mut(id)
            .ok_or_else(|| HierarchyError::MissingBone(id.into()))?
            .bind_transform = transform;
        Ok(())
    }

    pub fn subtree_ids(&self, id: &str) -> Result<HashSet<String>, HierarchyError> {
        self.validate()?;
        if self.bone(id).is_none() {
            return Err(HierarchyError::MissingBone(id.into()));
        }
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        for bone in &self.bones {
            if let Some(parent) = bone.parent.as_deref() {
                children.entry(parent).or_default().push(&bone.id);
            }
        }
        let mut ids = HashSet::new();
        let mut pending = vec![id];
        while let Some(id) = pending.pop() {
            ids.insert(id.to_string());
            if let Some(children) = children.get(id) {
                pending.extend(children.iter().copied());
            }
        }
        Ok(ids)
    }

    pub fn remove_bone(&mut self, id: &str) -> Result<Vec<Bone>, HierarchyError> {
        self.validate()?;
        if self
            .bones
            .iter()
            .any(|bone| bone.parent.as_deref() == Some(id))
        {
            return Err(HierarchyError::HasChildren(id.into()));
        }
        self.remove_subtree(id)
    }

    /// callers must check external references and remove attached sockets in the same transaction.
    pub fn remove_subtree(&mut self, id: &str) -> Result<Vec<Bone>, HierarchyError> {
        let ids = self.subtree_ids(id)?;
        let mut removed = Vec::with_capacity(ids.len());
        let mut retained = Vec::with_capacity(self.bones.len() - ids.len());
        for bone in std::mem::take(&mut self.bones) {
            if ids.contains(&bone.id) {
                removed.push(bone);
            } else {
                retained.push(bone);
            }
        }
        self.bones = retained;
        Ok(removed)
    }

    /// preserve every world-space bind matrix, rejecting shear rather than approximating it.
    /// animated local tracks need a separate migration before their hierarchy can change.
    pub fn reparent_bone(&mut self, id: &str, parent: Option<&str>) -> Result<(), HierarchyError> {
        self.validate()?;
        let bone = self
            .bone(id)
            .ok_or_else(|| HierarchyError::MissingBone(id.into()))?;
        if bone.parent.as_deref() == parent {
            return Ok(());
        }
        let mut ancestor = parent;
        while let Some(p) = ancestor {
            if p == id {
                return Err(HierarchyError::Cycle(id.into()));
            }
            ancestor = self
                .bone(p)
                .ok_or_else(|| HierarchyError::MissingParent {
                    bone: id.into(),
                    parent: p.into(),
                })?
                .parent
                .as_deref();
        }
        let bind = super::animation::AnimationClip::bind_pose();
        let world = super::animation::evaluate_world_transforms(self, &bind, 0.0);
        let local = if let Some(parent) = parent {
            let parent_world = world
                .get(parent)
                .ok_or_else(|| HierarchyError::MissingBone(parent.into()))?;
            if !parent_world
                .mul(&parent_world.inverse())
                .approx_eq(&super::math::Mat4::IDENTITY, 1e-4)
            {
                return Err(HierarchyError::UnrepresentableBindPose);
            }
            parent_world.inverse().mul(&world[id])
        } else {
            world[id]
        };
        let transform =
            Transform::try_from_matrix(&local).ok_or(HierarchyError::UnrepresentableBindPose)?;
        let candidate_world = parent.map_or_else(
            || transform.to_matrix(),
            |parent| world[parent].mul(&transform.to_matrix()),
        );
        if !preserves_bind_pose(&candidate_world, &world[id]) {
            return Err(HierarchyError::UnrepresentableBindPose);
        }
        // descendants can amplify small errors; evaluate using the runtime's persisted f32 math.
        let mut candidates = HashMap::new();
        candidates.insert(id, candidate_world);
        for (child, _) in self.depth_first() {
            if child.id == id {
                continue;
            }
            if let Some(parent_world) = child.parent.as_deref().and_then(|p| candidates.get(p)) {
                let matrix = parent_world.mul(&child.bind_transform.to_matrix());
                if !preserves_bind_pose(&matrix, &world[&child.id]) {
                    return Err(HierarchyError::UnrepresentableBindPose);
                }
                candidates.insert(child.id.as_str(), matrix);
            }
        }
        let bone = self
            .bone_mut(id)
            .ok_or_else(|| HierarchyError::MissingBone(id.into()))?;
        bone.parent = parent.map(str::to_owned);
        bone.bind_transform = transform;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::{evaluate_world_transforms, AnimationClip, BoneTrack, Keyframe};
    use crate::assets::{load_skeleton_asset, save_skeleton_asset, SkeletonAsset};
    use crate::math::{Mat4, Vec3};
    use crate::sockets::{Socket, Sockets};

    fn bone(id: &str, parent: Option<&str>) -> Bone {
        Bone {
            id: id.into(),
            name: id.into(),
            parent: parent.map(str::to_owned),
            bind_transform: Transform {
                translation: Vec3::new(1.0, 2.0, 3.0),
                ..Transform::IDENTITY
            },
        }
    }
    fn tree() -> Skeleton {
        let mut skeleton = Skeleton::default();
        for b in [
            bone("root", None),
            bone("arm", Some("root")),
            bone("hand", Some("arm")),
            bone("leg", Some("root")),
        ] {
            skeleton.add_bone(b).unwrap();
        }
        skeleton
    }
    #[test]
    fn roots_children_and_levels_have_deterministic_order() {
        let mut skeleton = tree();
        skeleton.add_bone(bone("other", None)).unwrap();
        assert_eq!(
            skeleton
                .depth_first()
                .iter()
                .map(|(b, d)| (b.id.as_str(), *d))
                .collect::<Vec<_>>(),
            vec![
                ("root", 0),
                ("arm", 1),
                ("hand", 2),
                ("leg", 1),
                ("other", 0)
            ]
        );
        assert_eq!(skeleton.root_bones().len(), 2);
        assert_eq!(skeleton.subtree_ids("arm").unwrap().len(), 2);
    }
    #[test]
    fn invalid_mutations_are_atomic() {
        let mut skeleton = tree();
        let before = skeleton.clone();
        assert!(matches!(
            skeleton.add_bone(bone("root", None)),
            Err(HierarchyError::DuplicateId(_))
        ));
        assert!(matches!(
            skeleton.add_bone(bone("new", Some("missing"))),
            Err(HierarchyError::MissingParent { .. })
        ));
        assert!(matches!(
            skeleton.add_bone(bone("new", Some("new"))),
            Err(HierarchyError::Cycle(_))
        ));
        for (id, parent) in [
            ("root", Some("root")),
            ("root", Some("hand")),
            ("arm", Some("hand")),
            ("hand", Some("missing")),
            ("missing", None),
        ] {
            assert!(skeleton.reparent_bone(id, parent).is_err());
            assert_eq!(skeleton, before);
        }
        assert!(skeleton.remove_bone("arm").is_err());
        assert!(skeleton.remove_subtree("missing").is_err());
        assert!(skeleton.rename_bone("arm", "  ".into()).is_err());
        assert!(skeleton
            .set_bind_transform(
                "root",
                Transform {
                    scale: Vec3::new(f32::NAN, 1.0, 1.0),
                    ..Transform::IDENTITY
                }
            )
            .is_err());
        assert_eq!(skeleton, before);
    }
    #[test]
    fn reparent_and_detach_preserve_all_world_bind_matrices() {
        let mut skeleton = tree();
        skeleton.bone_mut("root").unwrap().bind_transform.rotation = Vec3::new(23.0, -12.0, 44.0);
        skeleton.bone_mut("leg").unwrap().bind_transform.rotation.z = 15.0;
        let bind = AnimationClip::bind_pose();
        let before = evaluate_world_transforms(&skeleton, &bind, 0.0);
        for parent in [Some("leg"), None, Some("root")] {
            skeleton.reparent_bone("arm", parent).unwrap();
            assert_eq!(skeleton.bone("arm").unwrap().parent.as_deref(), parent);
            for (id, matrix) in evaluate_world_transforms(&skeleton, &bind, 0.0) {
                assert!(matrix.approx_eq(&before[&id], 1e-4), "{id}");
            }
        }
    }
    #[test]
    fn reparent_rejects_singular_and_sheared_local_transforms() {
        let mut skeleton = tree();
        skeleton.bone_mut("leg").unwrap().bind_transform.scale.x = 0.0;
        let before = skeleton.clone();
        assert_eq!(
            skeleton.reparent_bone("arm", Some("leg")),
            Err(HierarchyError::UnrepresentableBindPose)
        );
        assert_eq!(skeleton, before);
        skeleton.bone_mut("leg").unwrap().bind_transform.scale = Vec3::new(2.0, 1.0, 1.0);
        skeleton.bone_mut("arm").unwrap().bind_transform.rotation.z = 45.0;
        let before = skeleton.clone();
        assert!(skeleton.reparent_bone("arm", Some("leg")).is_err());
        assert_eq!(skeleton, before);
    }
    #[test]
    fn shear_rejection_is_scale_independent_and_keeps_world_and_skin_matrices() {
        for scale in [1e-7, 1e-4, 1.0, 1e4, 1e7] {
            for sign in [-1.0, 1.0] {
                let mut moving = bone("moving", None);
                moving.bind_transform = Transform {
                    rotation: Vec3::new(0.0, 0.0, 45.0),
                    scale: Vec3::new(sign * scale, scale, scale),
                    ..Transform::IDENTITY
                };
                let mut parent = bone("parent", None);
                parent.bind_transform = Transform {
                    scale: Vec3::new(2.0, 1.0, 1.0),
                    ..Transform::IDENTITY
                };
                let mut skeleton = Skeleton {
                    bones: vec![moving, parent, bone("child", Some("moving"))],
                };
                let original = skeleton.clone();
                let before = evaluate_world_transforms(&skeleton, &AnimationClip::bind_pose(), 0.0);
                let inverse_binds: HashMap<_, _> = before
                    .iter()
                    .map(|(id, m)| (id.clone(), m.inverse()))
                    .collect();
                assert_eq!(
                    skeleton.reparent_bone("moving", Some("parent")),
                    Err(HierarchyError::UnrepresentableBindPose),
                    "scale {scale}, sign {sign}"
                );
                assert_eq!(skeleton, original);
                let after = evaluate_world_transforms(&skeleton, &AnimationClip::bind_pose(), 0.0);
                for (id, matrix) in &before {
                    assert_eq!(matrix.0, after[id].0);
                    assert_eq!(
                        matrix.mul(&inverse_binds[id]).0,
                        after[id].mul(&inverse_binds[id]).0
                    );
                }
            }
        }
    }

    #[test]
    fn representable_scaled_reflections_preserve_descendant_skinning() {
        for scale in [1e-7, 1e-4, 1.0, 1e4, 1e7] {
            let mut moving = bone("moving", None);
            moving.bind_transform = Transform {
                scale: Vec3::new(-scale, scale, scale),
                ..Transform::IDENTITY
            };
            let mut parent = bone("parent", None);
            parent.bind_transform = Transform {
                scale: Vec3::new(-2.0, 2.0, 2.0),
                ..Transform::IDENTITY
            };
            let mut skeleton = Skeleton {
                bones: vec![moving, parent, bone("child", Some("moving"))],
            };
            let before = evaluate_world_transforms(&skeleton, &AnimationClip::bind_pose(), 0.0);
            skeleton.reparent_bone("moving", Some("parent")).unwrap();
            let after = evaluate_world_transforms(&skeleton, &AnimationClip::bind_pose(), 0.0);
            for (id, original) in &before {
                assert!(after[id].preserves_affine(original));
                assert!(
                    after[id]
                        .mul(&original.inverse())
                        .approx_eq(&Mat4::IDENTITY, 2e-5),
                    "{id}, {scale}"
                );
            }
        }
    }

    #[test]
    fn reparent_rejects_persisted_f32_cancellation_and_near_singular_axes_atomically() {
        for scale in [1.0, 1e-9, 0.0] {
            let mut moving = bone("moving", None);
            moving.bind_transform = Transform {
                translation: Vec3::new(1.0, 0.0, 0.0),
                ..Transform::IDENTITY
            };
            let mut parent = bone("parent", None);
            parent.bind_transform = Transform {
                translation: Vec3::new(100_000_000.0, 0.0, 0.0),
                scale: Vec3::new(scale, 1.0, 1.0),
                ..Transform::IDENTITY
            };
            let mut skeleton = Skeleton {
                bones: vec![
                    moving,
                    parent,
                    bone("child", Some("moving")),
                    bone("grandchild", Some("child")),
                ],
            };
            let before = skeleton.clone();
            assert_eq!(
                skeleton.reparent_bone("moving", Some("parent")),
                Err(HierarchyError::UnrepresentableBindPose)
            );
            assert_eq!(skeleton, before);
        }
    }

    #[test]
    fn large_world_coordinates_do_not_hide_bind_space_drift() {
        let mut moving = bone("moving", None);
        moving.bind_transform = Transform {
            translation: Vec3::new(100_000_000.0, 0.0, 0.0),
            ..Transform::IDENTITY
        };
        let mut parent = bone("parent", None);
        parent.bind_transform = Transform {
            translation: Vec3::new(7.0, 0.0, 0.0),
            scale: Vec3::new(3.0, 3.0, 3.0),
            ..Transform::IDENTITY
        };
        let mut skeleton = Skeleton {
            bones: vec![moving, parent, bone("child", Some("moving"))],
        };
        let before = skeleton.clone();
        let world = evaluate_world_transforms(&skeleton, &AnimationClip::bind_pose(), 0.0);
        let local =
            Transform::try_from_matrix(&world["parent"].inverse().mul(&world["moving"])).unwrap();
        let candidate = world["parent"].mul(&local.to_matrix());
        assert!(candidate.preserves_affine(&world["moving"]));
        assert!(!preserves_bind_pose(&candidate, &world["moving"]));
        assert_eq!(
            skeleton.reparent_bone("moving", Some("parent")),
            Err(HierarchyError::UnrepresentableBindPose)
        );
        assert_eq!(skeleton, before);
    }

    #[test]
    fn leaf_and_subtree_deletion_do_not_reparent_survivors() {
        let mut skeleton = tree();
        let leg = skeleton.bone("leg").unwrap().clone();
        assert_eq!(skeleton.remove_bone("hand").unwrap().len(), 1);
        skeleton.add_bone(bone("finger", Some("arm"))).unwrap();
        let removed = skeleton.remove_subtree("arm").unwrap();
        assert_eq!(
            removed.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
            ["arm", "finger"]
        );
        assert_eq!(skeleton.bone("leg"), Some(&leg));
        skeleton.validate().unwrap();
        skeleton.remove_subtree("root").unwrap();
        assert!(skeleton.bones.is_empty());
    }
    #[test]
    fn rename_keeps_animation_tracks_and_joint_ids_resolvable() {
        let mut skeleton = tree();
        let clip = AnimationClip {
            tracks: vec![BoneTrack {
                bone_id: "arm".into(),
                keyframes: vec![Keyframe::linear(0.0, Transform::IDENTITY)],
            }],
            ..AnimationClip::bind_pose()
        };
        let joint = crate::skmesh::Joint {
            bone: "arm".into(),
            inverse_bind: Mat4::IDENTITY.0,
        };
        let before = evaluate_world_transforms(&skeleton, &clip, 0.0);
        skeleton.rename_bone("arm", "Upper arm".into()).unwrap();
        assert!(clip.orphan_tracks(&skeleton).is_empty());
        assert!(skeleton.bone(&joint.bone).is_some());
        assert!(
            evaluate_world_transforms(&skeleton, &clip, 0.0)["arm"].approx_eq(&before["arm"], 1e-6)
        );
    }
    #[test]
    fn empty_bone_names_are_rejected_when_loading_assets() {
        for name in ["", " ", "\t\n"] {
            let mut skeleton = tree();
            skeleton.bone_mut("arm").unwrap().name = name.to_string();

            assert_eq!(skeleton.validate(), Err(HierarchyError::EmptyName));

            let serialized = SkeletonAsset::new(skeleton).to_text();

            assert!(
                SkeletonAsset::parse(&serialized).is_err(),
                "accepted invalid bone name: {name:?}"
            );
        }
    }
    #[test]
    fn malformed_serialized_hierarchies_are_rejected_and_traversal_is_bounded() {
        for bones in [
            vec![bone("a", Some("a"))],
            vec![bone("a", Some("b")), bone("b", Some("a"))],
            vec![bone("a", None), bone("a", None)],
            vec![bone("a", Some("missing"))],
        ] {
            let skeleton = Skeleton { bones };
            assert!(skeleton.validate().is_err());
            assert_eq!(skeleton.depth_first().len(), skeleton.bones.len());
            assert!(SkeletonAsset::parse(&SkeletonAsset::new(skeleton).to_text()).is_err());
        }
    }
    #[test]
    fn deep_hierarchies_do_not_use_the_call_stack() {
        let skeleton = Skeleton {
            bones: (0..10_000)
                .map(|i| {
                    bone(
                        &i.to_string(),
                        (i > 0).then(|| (i - 1).to_string()).as_deref(),
                    )
                })
                .collect(),
        };
        skeleton.validate().unwrap();
        assert_eq!(skeleton.depth_first().last().unwrap().1, 9_999);
    }
    #[test]
    fn sockets_validate_persist_and_follow_the_animated_bone() {
        let mut asset = SkeletonAsset::new(tree());
        let skeleton = asset.skeleton();
        let socket = Socket {
            id: "socket-1".into(),
            name: "Weapon".into(),
            bone_id: "hand".into(),
            local_transform: Transform::IDENTITY,
        };
        asset.sockets.add(&skeleton, socket.clone()).unwrap();
        assert!(asset.sockets.add(&skeleton, socket.clone()).is_err());
        assert!(asset
            .sockets
            .add(
                &skeleton,
                Socket {
                    id: "other".into(),
                    ..socket.clone()
                }
            )
            .is_err());
        assert!(asset
            .sockets
            .set(
                &skeleton,
                Socket {
                    bone_id: "missing".into(),
                    ..socket.clone()
                }
            )
            .is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rig.skeleton");
        save_skeleton_asset(&path, &asset).unwrap();
        let back = load_skeleton_asset(&path).unwrap();
        assert_eq!(back.id, asset.id);
        assert_eq!(back.sockets, asset.sockets);
        let clip = AnimationClip {
            tracks: vec![BoneTrack {
                bone_id: "hand".into(),
                keyframes: vec![Keyframe::linear(0.0, Transform::IDENTITY)],
            }],
            ..AnimationClip::bind_pose()
        };
        assert!(socket
            .world_transform(&skeleton, &clip, 0.0)
            .unwrap()
            .approx_eq(
                &evaluate_world_transforms(&skeleton, &clip, 0.0)["hand"],
                1e-6
            ));
        assert!(SkeletonAsset::parse(r#"{"version":1,"bones":[]}"#)
            .unwrap()
            .sockets
            .is_empty());
        let invalid = serde_json::json!([Socket {
            bone_id: "absent".into(),
            ..socket
        }]);
        let sockets: Sockets = serde_json::from_value(invalid).unwrap();
        assert!(sockets.validate(&skeleton).is_err());
    }
}

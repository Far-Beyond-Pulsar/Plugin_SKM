//! Per-bone blend masks, authored against a skeleton by stable bone id.
//!
//! A mask is a list of branches: each names a bone and a blend depth, and gives
//! that bone and its descendants (to `depth` levels, or all when `None`) a
//! weight. Bones outside every branch take `default_weight`. Layered-blend
//! graph nodes, Montage slot tracks and retarget settings use the same type so
//! the runtime builds one cached weight table per `(skeleton, mask)`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::skeleton::Skeleton;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaskBranch {
    pub bone: String,
    /// Levels below `bone` that receive the weight; `None` is the whole subtree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    pub weight: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BoneMaskSpec {
    #[serde(default)]
    pub branches: Vec<MaskBranch>,
    #[serde(default)]
    pub default_weight: f32,
}

impl BoneMaskSpec {
    /// Local rules, independent of any skeleton.
    pub fn validate(&self) -> Result<(), String> {
        if !self.default_weight.is_finite() || !(0.0..=1.0).contains(&self.default_weight) {
            return Err("Mask default weight must be in [0, 1]".into());
        }
        for branch in &self.branches {
            if branch.bone.trim().is_empty() {
                return Err("Mask branch needs a bone".into());
            }
            if !branch.weight.is_finite() || !(0.0..=1.0).contains(&branch.weight) {
                return Err(format!("Mask weight of `{}` must be in [0, 1]", branch.bone));
            }
        }
        Ok(())
    }

    /// Bone ids that are not in `skeleton`.
    pub fn missing_bones(&self, skeleton: &Skeleton) -> Vec<String> {
        self.branches
            .iter()
            .filter(|b| skeleton.bone(&b.bone).is_none())
            .map(|b| b.bone.clone())
            .collect()
    }

    /// Per-bone weights in `skeleton.bones` order. Later branches override
    /// earlier ones where they overlap, so authoring order is meaningful.
    pub fn weights(&self, skeleton: &Skeleton) -> Vec<f32> {
        let index: HashMap<&str, usize> = skeleton
            .bones
            .iter()
            .enumerate()
            .map(|(i, b)| (b.id.as_str(), i))
            .collect();
        let mut out = vec![self.default_weight; skeleton.bones.len()];
        for branch in &self.branches {
            let Some(&start) = index.get(branch.bone.as_str()) else {
                continue;
            };
            out[start] = branch.weight;
            let mut frontier = vec![start];
            let mut level = 0u32;
            while !frontier.is_empty() && branch.depth.map_or(true, |d| level < d) {
                let mut next = Vec::new();
                for &parent in &frontier {
                    let parent_id = skeleton.bones[parent].id.as_str();
                    for (i, bone) in skeleton.bones.iter().enumerate() {
                        if bone.parent.as_deref() == Some(parent_id) {
                            out[i] = branch.weight;
                            next.push(i);
                        }
                    }
                }
                frontier = next;
                level += 1;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Transform;
    use crate::skeleton::Bone;

    fn chain() -> Skeleton {
        let bone = |id: &str, parent: Option<&str>| Bone {
            id: id.into(),
            name: id.into(),
            parent: parent.map(Into::into),
            bind_transform: Transform::IDENTITY,
        };
        Skeleton {
            bones: vec![
                bone("root", None),
                bone("spine", Some("root")),
                bone("chest", Some("spine")),
                bone("head", Some("chest")),
                bone("leg", Some("root")),
            ],
        }
    }

    #[test]
    fn depth_limits_and_later_branches_override() {
        let mask = BoneMaskSpec {
            branches: vec![
                MaskBranch { bone: "spine".into(), depth: None, weight: 1.0 },
                MaskBranch { bone: "chest".into(), depth: Some(0), weight: 0.5 },
            ],
            default_weight: 0.0,
        };
        assert_eq!(mask.weights(&chain()), vec![0.0, 1.0, 0.5, 1.0, 0.0]);
        let shallow = BoneMaskSpec {
            branches: vec![MaskBranch { bone: "spine".into(), depth: Some(1), weight: 1.0 }],
            default_weight: 0.0,
        };
        assert_eq!(shallow.weights(&chain()), vec![0.0, 1.0, 1.0, 0.0, 0.0]);
        assert!(BoneMaskSpec { branches: vec![MaskBranch { bone: "x".into(), depth: None, weight: 2.0 }], default_weight: 0.0 }.validate().is_err());
    }
}

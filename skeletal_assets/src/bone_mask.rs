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
    /// Name of a mask stored on the skeleton. When set, the named mask is the
    /// rule and `branches` / `default_weight` are ignored (and kept empty by
    /// the editors), so a layered-blend layer or Montage slot track follows
    /// later edits to the library mask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// A mask stored on a skeleton asset and shared by name (Unreal's mask /
/// blend profile). The spec is always inline: profiles do not nest.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NamedBoneMask {
    pub name: String,
    #[serde(default)]
    pub spec: BoneMaskSpec,
}

/// Rules for a whole library: unique non-empty names, inline valid specs that
/// only name bones of `skeleton`.
pub fn validate_library(library: &[NamedBoneMask], skeleton: &Skeleton) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for mask in library {
        if mask.name.trim().is_empty() {
            return Err("A skeleton mask needs a name".into());
        }
        if !seen.insert(mask.name.as_str()) {
            return Err(format!("Skeleton mask `{}` is defined twice", mask.name));
        }
        if mask.spec.profile.is_some() {
            return Err(format!("Skeleton mask `{}` cannot refer to another mask", mask.name));
        }
        mask.spec.validate().map_err(|e| format!("Skeleton mask `{}`: {e}", mask.name))?;
        if let Some(bone) = mask.spec.missing_bones(skeleton).first() {
            return Err(format!("Skeleton mask `{}` names bone `{bone}`, which the skeleton does not have", mask.name));
        }
    }
    Ok(())
}

impl BoneMaskSpec {
    /// A mask that follows the library mask `name`.
    pub fn named(name: impl Into<String>) -> Self {
        Self { profile: Some(name.into()), ..Self::default() }
    }

    /// The inline rule this mask stands for: itself, or the library mask it names.
    pub fn resolve<'a>(&'a self, library: &'a [NamedBoneMask]) -> Result<&'a BoneMaskSpec, String> {
        match &self.profile {
            None => Ok(self),
            Some(name) => library
                .iter()
                .find(|m| &m.name == name)
                .map(|m| &m.spec)
                .ok_or_else(|| format!("The skeleton has no mask named `{name}`")),
        }
    }

    /// [`Self::weights`] after resolving a named profile against `library`.
    pub fn weights_in(&self, skeleton: &Skeleton, library: &[NamedBoneMask]) -> Result<Vec<f32>, String> {
        Ok(self.resolve(library)?.weights(skeleton))
    }

    /// [`Self::missing_bones`] of the resolved rule; an unknown profile name is an error.
    pub fn missing_bones_in(&self, skeleton: &Skeleton, library: &[NamedBoneMask]) -> Result<Vec<String>, String> {
        Ok(self.resolve(library)?.missing_bones(skeleton))
    }

    /// Local rules, independent of any skeleton.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(name) = &self.profile {
            return if name.trim().is_empty() { Err("Mask profile name cannot be empty".into()) } else { Ok(()) };
        }
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
            default_weight: 0.0, ..Default::default()
        };
        assert_eq!(mask.weights(&chain()), vec![0.0, 1.0, 0.5, 1.0, 0.0]);
        let shallow = BoneMaskSpec {
            branches: vec![MaskBranch { bone: "spine".into(), depth: Some(1), weight: 1.0 }],
            default_weight: 0.0, ..Default::default()
        };
        assert_eq!(shallow.weights(&chain()), vec![0.0, 1.0, 1.0, 0.0, 0.0]);
        assert!(BoneMaskSpec { branches: vec![MaskBranch { bone: "x".into(), depth: None, weight: 2.0 }], default_weight: 0.0, ..Default::default() }.validate().is_err());
    }

    #[test]
    fn a_named_mask_resolves_against_the_library_and_follows_edits() {
        let upper = BoneMaskSpec { branches: vec![MaskBranch { bone: "spine".into(), depth: None, weight: 1.0 }], ..Default::default() };
        let mut library = vec![NamedBoneMask { name: "UpperBody".into(), spec: upper }];
        let by_name = BoneMaskSpec::named("UpperBody");
        assert_eq!(by_name.weights_in(&chain(), &library).unwrap(), vec![0.0, 1.0, 1.0, 1.0, 0.0]);
        library[0].spec.branches[0].bone = "chest".into();
        assert_eq!(by_name.weights_in(&chain(), &library).unwrap(), vec![0.0, 0.0, 1.0, 1.0, 0.0]);
        assert!(BoneMaskSpec::named("Nope").weights_in(&chain(), &library).is_err());
        assert!(by_name.validate().is_ok());
    }

    #[test]
    fn a_library_rejects_duplicates_nesting_and_unknown_bones() {
        let ok = NamedBoneMask { name: "A".into(), spec: BoneMaskSpec::default() };
        assert!(validate_library(&[ok.clone()], &chain()).is_ok());
        assert!(validate_library(&[ok.clone(), ok.clone()], &chain()).is_err());
        assert!(validate_library(&[NamedBoneMask { name: "B".into(), spec: BoneMaskSpec::named("A") }], &chain()).is_err());
        let ghost = BoneMaskSpec { branches: vec![MaskBranch { bone: "tail".into(), depth: None, weight: 1.0 }], ..Default::default() };
        assert!(validate_library(&[NamedBoneMask { name: "C".into(), spec: ghost }], &chain()).is_err());
        assert!(validate_library(&[NamedBoneMask { name: " ".into(), spec: BoneMaskSpec::default() }], &chain()).is_err());
    }
}

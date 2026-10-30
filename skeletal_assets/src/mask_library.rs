//! Moving inline masks into a skeleton's named mask library.
//!
//! Masks used to be authored per Layered Blend layer and per Montage slot
//! track. A skeleton now owns named masks (see [`crate::bone_mask`]); this
//! module is the migration and the "save as skeleton mask" action: an inline
//! mask with branches becomes a library entry, reusing an existing entry
//! whose rule is identical, and the user is left holding a by-name reference.

use serde_json::Value;

use crate::anim_graph::AnimationGraphAsset;
use crate::bone_mask::{BoneMaskSpec, NamedBoneMask};
use crate::montage::AnimationMontageAsset;

/// A name not yet used in `library`, derived from `base`.
pub fn unique_name(library: &[NamedBoneMask], base: &str) -> String {
    let base = if base.trim().is_empty() {
        "Mask"
    } else {
        base.trim()
    };
    if !library.iter().any(|m| m.name == base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|c| !library.iter().any(|m| &m.name == c))
        .unwrap_or_default()
}

/// Replace `spec` by a reference to a library mask with the same rule: an
/// identical existing entry, or a new one called `name_hint`. Returns the name
/// used, or `None` when `spec` is already by-name or has no branches (a plain
/// "everything" / "nothing" mask is not worth a library entry).
pub fn promote(
    spec: &mut BoneMaskSpec,
    library: &mut Vec<NamedBoneMask>,
    name_hint: &str,
) -> Option<String> {
    if spec.profile.is_some() || spec.branches.is_empty() {
        return None;
    }
    let name = match library.iter().find(|m| m.spec == *spec) {
        Some(existing) => existing.name.clone(),
        None => {
            let name = unique_name(library, name_hint);
            library.push(NamedBoneMask {
                name: name.clone(),
                spec: spec.clone(),
            });
            name
        }
    };
    *spec = BoneMaskSpec::named(name.clone());
    Some(name)
}

/// Promote every inline Layered Blend layer mask of `graph`. Returns the names now referenced.
pub fn promote_graph_masks(
    graph: &mut AnimationGraphAsset,
    library: &mut Vec<NamedBoneMask>,
) -> Vec<String> {
    let mut used = Vec::new();
    let mut graphs: Vec<&mut graphy_blueprint::GraphDescription> = vec![&mut graph.main];
    graphs.extend(graph.subgraphs.iter_mut().map(|s| &mut s.graph));
    for description in graphs {
        let mut ids: Vec<String> = description.nodes.keys().cloned().collect();
        ids.sort();
        for id in ids {
            let node = description
                .nodes
                .get_mut(&id)
                .expect("id came from the map");
            if node.node_type != "anim.layered_blend_per_bone" {
                continue;
            }
            let Some(Value::Array(masks)) = node.properties.get_mut("masks") else {
                continue;
            };
            for (layer, value) in masks.iter_mut().enumerate() {
                let Ok(mut spec) = serde_json::from_value::<BoneMaskSpec>(value.clone()) else {
                    continue;
                };
                if let Some(name) = promote(
                    &mut spec,
                    library,
                    &format!("{} Layer {}", node.id, layer + 1),
                ) {
                    *value = serde_json::to_value(&spec).unwrap_or(Value::Null);
                    used.push(name);
                }
            }
        }
    }
    used
}

/// Promote every inline slot-track mask of `montage`. Returns the names now referenced.
pub fn promote_montage_masks(
    montage: &mut AnimationMontageAsset,
    library: &mut Vec<NamedBoneMask>,
) -> Vec<String> {
    let mut used = Vec::new();
    for track in &mut montage.tracks {
        if let Some(spec) = track.mask.as_mut() {
            if let Some(name) = promote(spec, library, &format!("{} {}", track.group, track.slot)) {
                used.push(name);
            }
        }
    }
    used
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bone_mask::MaskBranch;

    fn upper() -> BoneMaskSpec {
        BoneMaskSpec {
            branches: vec![MaskBranch {
                bone: "spine".into(),
                depth: None,
                weight: 1.0,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn promoting_reuses_an_identical_entry_and_skips_trivial_masks() {
        let mut library = Vec::new();
        let mut a = upper();
        let mut b = upper();
        assert_eq!(
            promote(&mut a, &mut library, "Upper").as_deref(),
            Some("Upper")
        );
        assert_eq!(
            promote(&mut b, &mut library, "Other").as_deref(),
            Some("Upper")
        );
        assert_eq!(library.len(), 1);
        assert_eq!(a, BoneMaskSpec::named("Upper"));
        let mut everything = BoneMaskSpec {
            default_weight: 1.0,
            ..Default::default()
        };
        assert!(promote(&mut everything, &mut library, "All").is_none());
        assert!(
            promote(&mut a, &mut library, "Again").is_none(),
            "a named mask is left alone"
        );
    }

    #[test]
    fn names_stay_unique() {
        let library = vec![NamedBoneMask {
            name: "Upper".into(),
            spec: upper(),
        }];
        assert_eq!(unique_name(&library, "Upper"), "Upper 2");
        assert_eq!(unique_name(&library, ""), "Mask");
    }
}

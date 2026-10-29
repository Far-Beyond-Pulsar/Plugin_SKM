//! Editing the per-layer bone masks of a Layered Blend per Bone node.
//!
//! The node's `masks` property is an array of [`BoneMaskSpec`], one per layer.
//! A mask is a default weight plus ordered branches (a bone, how many levels
//! below it, a weight); later branches override earlier ones. Every command
//! here takes and returns the property's JSON, so the model applies it as one
//! undoable property edit, and every result is validated against the graph's
//! skeleton so a mask that names a bone the skeleton does not have is refused
//! at the edit rather than at compile time.

use serde_json::Value;
use skeletal_assets::bone_mask::{BoneMaskSpec, MaskBranch};
use skeletal_assets::skeleton::Skeleton;

/// The masks of a node with `layers` layers; missing layers are empty masks
/// (weight 0 everywhere), which is also what the runtime does for them.
pub fn read(value: &Value, layers: usize) -> Vec<BoneMaskSpec> {
    let mut specs: Vec<BoneMaskSpec> = value
        .as_array()
        .map(|a| a.iter().map(|m| serde_json::from_value(m.clone()).unwrap_or_default()).collect())
        .unwrap_or_default();
    let layers = layers.max(1);
    specs.resize_with(layers, BoneMaskSpec::default);
    specs.truncate(layers);
    specs
}

pub fn write(specs: &[BoneMaskSpec]) -> Value {
    serde_json::to_value(specs).unwrap_or(Value::Null)
}

/// Whether `value` is a well-formed `masks` property for `skeleton`.
pub fn validate(value: &Value, skeleton: Option<&Skeleton>) -> Result<(), String> {
    if value.is_null() || value == &Value::String(String::new()) {
        return Ok(());
    }
    let array = value.as_array().ok_or("masks must be a list with one mask per layer")?;
    for (layer, spec) in array.iter().enumerate() {
        let spec: BoneMaskSpec = serde_json::from_value(spec.clone()).map_err(|e| format!("Layer {layer}: {e}"))?;
        spec.validate().map_err(|e| format!("Layer {layer}: {e}"))?;
        if let Some(skeleton) = skeleton {
            if let Some(missing) = spec.missing_bones(skeleton).first() {
                return Err(format!("Layer {layer} names bone `{missing}`, which the skeleton does not have"));
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub enum Preset {
    /// Every bone takes the layer fully.
    Everything,
    /// No bone takes the layer.
    Nothing,
    /// `bone` and everything below it takes the layer (an upper body from the spine).
    Subtree(String),
    /// Everything except `bone` and what is below it (a lower body).
    Except(String),
}

pub fn apply_preset(spec: &mut BoneMaskSpec, preset: &Preset) {
    *spec = match preset {
        Preset::Everything => BoneMaskSpec { branches: Vec::new(), default_weight: 1.0 },
        Preset::Nothing => BoneMaskSpec { branches: Vec::new(), default_weight: 0.0 },
        Preset::Subtree(bone) => BoneMaskSpec { branches: vec![MaskBranch { bone: bone.clone(), depth: None, weight: 1.0 }], default_weight: 0.0 },
        Preset::Except(bone) => BoneMaskSpec { branches: vec![MaskBranch { bone: bone.clone(), depth: None, weight: 0.0 }], default_weight: 1.0 },
    };
}

pub fn add_branch(spec: &mut BoneMaskSpec, bone: &str) {
    spec.branches.push(MaskBranch { bone: bone.to_string(), depth: None, weight: 1.0 });
}

pub fn remove_branch(spec: &mut BoneMaskSpec, index: usize) -> bool {
    if index < spec.branches.len() {
        spec.branches.remove(index);
        true
    } else {
        false
    }
}

/// Move a branch one place earlier or later; order decides which branch wins an overlap.
pub fn move_branch(spec: &mut BoneMaskSpec, index: usize, later: bool) -> bool {
    let to = if later { index + 1 } else { index.wrapping_sub(1) };
    if index < spec.branches.len() && to < spec.branches.len() {
        spec.branches.swap(index, to);
        true
    } else {
        false
    }
}

/// What a branch field edits.
#[derive(Clone, Debug, PartialEq)]
pub enum BranchField {
    Bone(String),
    /// `None` is the whole subtree.
    Depth(Option<u32>),
    Weight(f32),
}

pub fn set_branch(spec: &mut BoneMaskSpec, index: usize, field: BranchField) -> Result<(), String> {
    let branch = spec.branches.get_mut(index).ok_or("no such branch")?;
    match field {
        BranchField::Bone(bone) => branch.bone = bone,
        BranchField::Depth(depth) => branch.depth = depth,
        BranchField::Weight(w) => {
            if !w.is_finite() || !(0.0..=1.0).contains(&w) {
                return Err("a weight is between 0 and 1".into());
            }
            branch.weight = w;
        }
    }
    Ok(())
}

pub fn set_default_weight(spec: &mut BoneMaskSpec, weight: f32) -> Result<(), String> {
    if !weight.is_finite() || !(0.0..=1.0).contains(&weight) {
        return Err("a weight is between 0 and 1".into());
    }
    spec.default_weight = weight;
    Ok(())
}

/// Swap what the mask takes and leaves: an upper-body mask becomes the lower body.
pub fn invert(spec: &mut BoneMaskSpec) {
    spec.default_weight = 1.0 - spec.default_weight;
    for branch in &mut spec.branches {
        branch.weight = 1.0 - branch.weight;
    }
}

/// A bone of `skeleton` by id, or else by name ignoring case.
pub fn resolve_bone(skeleton: &Skeleton, text: &str) -> Option<String> {
    let text = text.trim();
    skeleton
        .bones
        .iter()
        .find(|b| b.id == text)
        .or_else(|| skeleton.bones.iter().find(|b| b.name.eq_ignore_ascii_case(text)))
        .map(|b| b.id.clone())
}

/// The weight each bone ends up with, in skeleton order, as `(bone name, weight)`.
pub fn effective(spec: &BoneMaskSpec, skeleton: &Skeleton) -> Vec<(String, f32)> {
    skeleton.bones.iter().zip(spec.weights(skeleton)).map(|(b, w)| (b.name.clone(), w)).collect()
}

/// How many bones have any weight, and how many there are, for the summary line.
pub fn coverage(spec: &BoneMaskSpec, skeleton: &Skeleton) -> (usize, usize) {
    let weights = spec.weights(skeleton);
    (weights.iter().filter(|w| **w > 0.0).count(), weights.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sample;
    use serde_json::json;

    #[test]
    fn presets_branches_and_weights_edit_the_spec_and_validate_against_the_skeleton() {
        let skeleton = sample::humanoid_skeleton();
        let spine = skeleton.bones.iter().find(|b| b.name.to_lowercase().contains("spine")).expect("a spine").id.clone();
        let mut spec = BoneMaskSpec::default();
        apply_preset(&mut spec, &Preset::Subtree(spine.clone()));
        let (covered, total) = coverage(&spec, &skeleton);
        assert!(covered > 1 && covered < total, "an upper body covers some of the bones: {covered}/{total}");
        apply_preset(&mut spec, &Preset::Except(spine.clone()));
        let (lower, _) = coverage(&spec, &skeleton);
        assert_eq!(lower + covered, total, "the two presets partition the skeleton");

        add_branch(&mut spec, &spine);
        add_branch(&mut spec, &skeleton.bones[0].id);
        assert!(set_branch(&mut spec, 1, BranchField::Weight(2.0)).is_err());
        set_branch(&mut spec, 1, BranchField::Weight(0.25)).unwrap();
        set_branch(&mut spec, 1, BranchField::Depth(Some(1))).unwrap();
        assert!(move_branch(&mut spec, 1, false) && !move_branch(&mut spec, 0, false));
        assert!(set_default_weight(&mut spec, f32::NAN).is_err());
        assert!(remove_branch(&mut spec, 2) && !remove_branch(&mut spec, 9));

        let value = write(&[spec.clone()]);
        assert_eq!(read(&value, 1), vec![spec.clone()], "the property round-trips");
        assert!(validate(&value, Some(&skeleton)).is_ok());
        let bad = json!([{ "branches": [{ "bone": "ghost", "weight": 1.0 }], "default_weight": 0.0 }]);
        assert!(validate(&bad, Some(&skeleton)).unwrap_err().contains("ghost"));
        assert!(validate(&bad, None).is_ok(), "without a skeleton only the local rules apply");
        assert!(validate(&json!([{ "branches": [], "default_weight": 3.0 }]), None).is_err());
        assert!(validate(&json!("nope"), None).is_err());
        let mut flipped = spec.clone();
        invert(&mut flipped);
        invert(&mut flipped);
        assert_eq!(flipped, spec, "inverting twice is the identity");
        let name = skeleton.bones[1].name.to_uppercase();
        assert_eq!(resolve_bone(&skeleton, &name), Some(skeleton.bones[1].id.clone()));
        assert_eq!(resolve_bone(&skeleton, "nobody"), None);
    }

    #[test]
    fn layers_are_padded_and_truncated_to_the_node_count() {
        assert_eq!(read(&Value::Null, 3).len(), 3);
        let two = write(&[BoneMaskSpec::default(), BoneMaskSpec { branches: vec![], default_weight: 0.5 }]);
        assert_eq!(read(&two, 1).len(), 1);
        assert_eq!(read(&two, 3)[1].default_weight, 0.5);
    }
}

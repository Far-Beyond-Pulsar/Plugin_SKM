//! The named bone masks stored on a Skeleton asset (Unreal's mask / blend
//! profiles), edited from the Animation Graph and Montage editors.
//!
//! Layered Blend layers and Montage slot tracks refer to a mask by name
//! (`BoneMaskSpec::named`); the rule lives on the skeleton, so one edit
//! re-shapes every user. [`apply`] is the pure edit of a [`SkeletonAsset`];
//! [`update`] is that edit on the project's file (load, apply, validate,
//! save, re-index). Library edits are saved immediately: they are edits of the
//! skeleton file, not of the open document, so they are not on the document's
//! undo stack.

use std::path::{Path, PathBuf};

use pulsar_reflection::{AssetKind, SkeletonAssetKind};
use skeletal_assets::assets::{save_skeleton_asset, SkeletonAsset};
use skeletal_assets::bone_mask::{validate_library, BoneMaskSpec, NamedBoneMask};
use skeletal_assets::mask_library::unique_name;

use crate::anim_editors::refs::ProjectRefIndex;
use crate::core::asset_index;

#[derive(Clone, Debug, PartialEq)]
pub enum MaskOp {
    /// A new mask; `name` is made unique.
    Add {
        name: String,
        spec: BoneMaskSpec,
    },
    Rename {
        from: String,
        to: String,
    },
    Delete {
        name: String,
    },
    /// Edit the rule of one mask.
    Edit {
        name: String,
    },
}

/// Apply `op` to the skeleton's library. `Edit` is applied by [`edit`] with a closure;
/// this handles the structural operations and returns the name now in use.
pub fn apply(asset: &mut SkeletonAsset, op: MaskOp) -> Result<String, String> {
    let name = match op {
        MaskOp::Add { name, spec } => {
            let name = unique_name(&asset.masks, &name);
            asset.masks.push(NamedBoneMask {
                name: name.clone(),
                spec,
            });
            name
        }
        MaskOp::Rename { from, to } => {
            let to = to.trim().to_string();
            if to.is_empty() {
                return Err("A mask needs a name".into());
            }
            if from != to && asset.masks.iter().any(|m| m.name == to) {
                return Err(format!("The skeleton already has a mask named `{to}`"));
            }
            asset
                .masks
                .iter_mut()
                .find(|m| m.name == from)
                .ok_or_else(|| format!("no mask named `{from}`"))?
                .name = to.clone();
            to
        }
        MaskOp::Delete { name } => {
            let before = asset.masks.len();
            asset.masks.retain(|m| m.name != name);
            if asset.masks.len() == before {
                return Err(format!("no mask named `{name}`"));
            }
            name
        }
        MaskOp::Edit { name } => name,
    };
    validate_library(&asset.masks, &asset.skeleton())?;
    Ok(name)
}

/// Edit the rule of the mask `name` with `change`; the result must stay valid.
pub fn edit(
    asset: &mut SkeletonAsset,
    name: &str,
    change: impl FnOnce(&mut BoneMaskSpec) -> Result<(), String>,
) -> Result<(), String> {
    let index = asset
        .masks
        .iter()
        .position(|m| m.name == name)
        .ok_or_else(|| format!("no mask named `{name}`"))?;
    let mut spec = asset.masks[index].spec.clone();
    change(&mut spec)?;
    let mut library = asset.masks.clone();
    library[index].spec = spec;
    validate_library(&library, &asset.skeleton())?;
    asset.masks = library;
    Ok(())
}

pub fn skeleton_path(root: &Path, id: &str) -> Option<PathBuf> {
    ProjectRefIndex::new(root).locate_path(SkeletonAssetKind::KIND, id)
}

pub fn load(root: &Path, id: &str) -> Result<SkeletonAsset, String> {
    let path =
        skeleton_path(root, id).ok_or("the skeleton asset could not be found in the project")?;
    let bytes = engine_fs::virtual_fs::read_file(&path).map_err(|e| e.to_string())?;
    SkeletonAsset::parse(&String::from_utf8(bytes).map_err(|e| e.to_string())?)
}

/// The skeleton's named masks, or none when it cannot be read.
pub fn library(root: &Path, id: &str) -> Vec<NamedBoneMask> {
    load(root, id).map(|a| a.masks).unwrap_or_default()
}

/// Load the skeleton, run `change` on it and save it. Returns the saved asset.
pub fn update(
    root: &Path,
    id: &str,
    change: impl FnOnce(&mut SkeletonAsset) -> Result<(), String>,
) -> Result<SkeletonAsset, String> {
    let path =
        skeleton_path(root, id).ok_or("the skeleton asset could not be found in the project")?;
    let mut asset = load(root, id)?;
    change(&mut asset)?;
    save_skeleton_asset(&path, &asset).map_err(|e| e.to_string())?;
    asset_index::record_asset(root, &path);
    Ok(asset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sample;
    use skeletal_assets::bone_mask::MaskBranch;

    fn asset() -> SkeletonAsset {
        SkeletonAsset::new(sample::humanoid_skeleton())
    }

    fn spec_on(bone: &str) -> BoneMaskSpec {
        BoneMaskSpec {
            branches: vec![MaskBranch {
                bone: bone.into(),
                depth: None,
                weight: 1.0,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn masks_are_added_renamed_edited_and_deleted_with_validation() {
        let mut a = asset();
        let spine = a
            .bones
            .iter()
            .find(|b| b.name.to_lowercase().contains("spine"))
            .unwrap()
            .id
            .clone();
        let first = apply(
            &mut a,
            MaskOp::Add {
                name: "Upper".into(),
                spec: spec_on(&spine),
            },
        )
        .unwrap();
        let second = apply(
            &mut a,
            MaskOp::Add {
                name: "Upper".into(),
                spec: spec_on(&spine),
            },
        )
        .unwrap();
        assert_eq!((first.as_str(), second.as_str()), ("Upper", "Upper 2"));
        assert!(apply(
            &mut a,
            MaskOp::Rename {
                from: "Upper 2".into(),
                to: "Upper".into()
            }
        )
        .is_err());
        assert_eq!(
            apply(
                &mut a,
                MaskOp::Rename {
                    from: "Upper 2".into(),
                    to: "Arms".into()
                }
            )
            .unwrap(),
            "Arms"
        );
        assert!(
            apply(
                &mut a,
                MaskOp::Add {
                    name: "Bad".into(),
                    spec: spec_on("ghost")
                }
            )
            .is_err(),
            "an unknown bone is refused"
        );
        a.masks.retain(|m| m.name != "Bad");
        edit(&mut a, "Arms", |s| {
            s.default_weight = 0.5;
            Ok(())
        })
        .unwrap();
        assert!(edit(&mut a, "Arms", |s| {
            s.default_weight = 3.0;
            Ok(())
        })
        .is_err());
        assert_eq!(
            a.masks
                .iter()
                .find(|m| m.name == "Arms")
                .unwrap()
                .spec
                .default_weight,
            0.5,
            "a refused edit changes nothing"
        );
        apply(
            &mut a,
            MaskOp::Delete {
                name: "Upper".into(),
            },
        )
        .unwrap();
        assert!(apply(
            &mut a,
            MaskOp::Delete {
                name: "Upper".into()
            }
        )
        .is_err());
        let back = SkeletonAsset::parse(&a.to_text()).unwrap();
        assert_eq!(back.masks, a.masks, "masks survive save and reopen");
    }

    #[test]
    fn a_skeleton_without_masks_is_written_without_the_field() {
        let a = asset();
        assert!(!a.to_text().contains("\"masks\""));
        assert!(SkeletonAsset::parse(&a.to_text()).unwrap().masks.is_empty());
    }
}

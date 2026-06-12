//! Disk format for a skeletal animation asset.
//!
//! An asset is a folder containing `skeleton.json` (the bone hierarchy and
//! bind pose) and `animation.json` (the active clip). Both are plain JSON so
//! they're easy to hand-edit or generate from import tooling.

use std::path::Path;

use super::animation::AnimationClip;
use super::skeleton::Skeleton;

pub const SKELETON_FILE: &str = "skeleton.json";
pub const ANIMATION_FILE: &str = "animation.json";

pub fn load_skeleton(asset_dir: &Path) -> anyhow::Result<Skeleton> {
    let path = asset_dir.join(SKELETON_FILE);
    let text = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&text)?)
}

pub fn load_animation(asset_dir: &Path) -> anyhow::Result<AnimationClip> {
    let path = asset_dir.join(ANIMATION_FILE);
    let text = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&text)?)
}

pub fn save_skeleton(asset_dir: &Path, skeleton: &Skeleton) -> anyhow::Result<()> {
    std::fs::create_dir_all(asset_dir)?;
    let text = serde_json::to_string_pretty(skeleton)?;
    std::fs::write(asset_dir.join(SKELETON_FILE), text)?;
    Ok(())
}

pub fn save_animation(asset_dir: &Path, clip: &AnimationClip) -> anyhow::Result<()> {
    std::fs::create_dir_all(asset_dir)?;
    let text = serde_json::to_string_pretty(clip)?;
    std::fs::write(asset_dir.join(ANIMATION_FILE), text)?;
    Ok(())
}

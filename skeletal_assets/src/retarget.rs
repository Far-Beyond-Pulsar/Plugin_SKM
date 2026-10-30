//! The `.animretarget` asset (Retarget Profile) and the retarget data shared
//! with `.ikretarget`.
//!
//! A profile maps animation from a source skeleton to a target skeleton whose
//! hierarchy may differ. Mapping is by chain when both IK rigs are given and by
//! bone otherwise. Rotation and translation modes, the retarget poses and the
//! root policy are authored here; the profile never stores animation.

use pulsar_reflection::{AssetRef, IkRigAssetKind, RetargetProfileAssetKind, SkeletonAssetKind};
use serde::{Deserialize, Serialize};

use crate::codec::{self, ensure_finite, ensure_unique, from_value, read_envelope, CodecError, Extra, VersionPolicy};
use crate::diagnostics::Diagnostic;
use crate::ik_rig::{IkRigAsset, Vec3};
use crate::skeleton::Skeleton;

pub const RETARGET_EXTENSION: &str = "animretarget";
pub const RETARGET_VERSION: u32 = 1;
const POLICY: VersionPolicy = VersionPolicy { oldest: 1, current: RETARGET_VERSION, unversioned_as: None };

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RotationMode {
    /// Rotations are distributed along the chain by relative bone length.
    #[default]
    Interpolated,
    /// Each target bone copies its mapped source bone's rotation delta.
    OneToOne,
    /// As one-to-one with the chain order reversed (e.g. mirrored limbs).
    OneToOneReversed,
    /// The chain keeps its retarget-pose rotations.
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranslationMode {
    #[default]
    None,
    /// Translation deltas scale by the target/source skeleton size ratio.
    GloballyScaled,
    /// Target bones take the source's translation unscaled.
    Absolute,
}

/// A retarget pose: per-bone rotation offsets (quaternions, xyzw) applied on
/// the skeleton's bind pose, plus a root translation offset.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RetargetPose {
    #[serde(default)]
    pub bone_rotations: Vec<BoneRotation>,
    #[serde(default)]
    pub root_translation: Vec3,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoneRotation {
    pub bone: String,
    pub rotation: [f32; 4],
}

impl RetargetPose {
    pub fn validate(&self) -> Result<(), String> {
        ensure_unique("retarget pose bone", self.bone_rotations.iter().map(|b| b.bone.as_str()))?;
        for b in &self.bone_rotations {
            let len2: f32 = b.rotation.iter().map(|v| v * v).sum();
            if b.rotation.iter().any(|v| !v.is_finite()) || (len2 - 1.0).abs() > 1e-3 {
                return Err(format!("Retarget pose rotation of `{}` must be a unit quaternion", b.bone));
            }
        }
        self.root_translation.iter().try_for_each(|v| ensure_finite("root translation", *v))
    }

    pub fn missing_bones(&self, skeleton: &Skeleton) -> Vec<String> {
        self.bone_rotations.iter().filter(|b| skeleton.bone(&b.bone).is_none()).map(|b| b.bone.clone()).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoneMapping {
    pub id: String,
    pub source_bone: String,
    pub target_bone: String,
    #[serde(default)]
    pub rotation: RotationMode,
    #[serde(default)]
    pub translation: TranslationMode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChainMapping {
    pub id: String,
    /// Chain name in the source rig.
    pub source_chain: String,
    /// Chain name in the target rig.
    pub target_chain: String,
    #[serde(default)]
    pub rotation: RotationMode,
    #[serde(default = "one")]
    pub rotation_alpha: f32,
    #[serde(default)]
    pub translation: TranslationMode,
    #[serde(default = "one")]
    pub translation_alpha: f32,
}

fn one() -> f32 {
    1.0
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RootScale {
    /// Ratio of the target's to the source's root height in the retarget pose.
    Auto,
    Fixed { factor: f32 },
}

impl Default for RootScale {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RootPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_root: Option<String>,
    #[serde(default)]
    pub scale: RootScale,
    #[serde(default = "one")]
    pub translation_alpha: f32,
    #[serde(default = "one")]
    pub rotation_alpha: f32,
}

impl Default for RootPolicy {
    fn default() -> Self {
        Self { source_root: None, target_root: None, scale: RootScale::Auto, translation_alpha: 1.0, rotation_alpha: 1.0 }
    }
}

impl RootPolicy {
    pub fn validate(&self) -> Result<(), String> {
        for (what, v) in [("translation alpha", self.translation_alpha), ("rotation alpha", self.rotation_alpha)] {
            if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                return Err(format!("Root {what} must be in [0, 1]"));
            }
        }
        if let RootScale::Fixed { factor } = self.scale {
            if !factor.is_finite() || factor <= 0.0 {
                return Err("Root scale factor must be positive".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetargetProfileAsset {
    pub version: u32,
    pub id: AssetRef<RetargetProfileAssetKind>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_skeleton: Option<AssetRef<SkeletonAssetKind>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_skeleton: Option<AssetRef<SkeletonAssetKind>>,
    /// With both rigs set the profile maps by chain; the rigs' skeletons must
    /// be the source and target skeleton.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_rig: Option<AssetRef<IkRigAssetKind>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_rig: Option<AssetRef<IkRigAssetKind>>,
    #[serde(default)]
    pub chain_map: Vec<ChainMapping>,
    #[serde(default)]
    pub bone_map: Vec<BoneMapping>,
    #[serde(default)]
    pub source_pose: RetargetPose,
    #[serde(default)]
    pub target_pose: RetargetPose,
    #[serde(default)]
    pub root: RootPolicy,
    #[serde(flatten)]
    pub extra: Extra,
}

impl RetargetProfileAsset {
    pub fn new(
        name: impl Into<String>,
        source: impl Into<Option<AssetRef<SkeletonAssetKind>>>,
        target: impl Into<Option<AssetRef<SkeletonAssetKind>>>,
    ) -> Self {
        Self {
            version: RETARGET_VERSION,
            id: AssetRef::new(),
            name: name.into(),
            source_skeleton: source.into(),
            target_skeleton: target.into(),
            source_rig: None,
            target_rig: None,
            chain_map: Vec::new(),
            bone_map: Vec::new(),
            source_pose: RetargetPose::default(),
            target_pose: RetargetPose::default(),
            root: RootPolicy::default(),
            extra: Extra::new(),
        }
    }

    pub fn maps_by_chain(&self) -> bool {
        self.source_rig.is_some() && self.target_rig.is_some()
    }

    pub fn parse(text: &str) -> Result<Self, CodecError> {
        let (value, _) = read_envelope(text, POLICY)?;
        if value.get("id").is_none() {
            return Err(CodecError::MissingId);
        }
        let asset: Self = from_value(value)?;
        asset.validate_structure().map_err(CodecError::Invalid)?;
        Ok(asset)
    }

    pub fn to_text(&self) -> anyhow::Result<String> {
        self.validate_structure().map_err(|e| anyhow::anyhow!(e))?;
        let mut asset = self.clone();
        asset.version = RETARGET_VERSION;
        Ok(serde_json::to_string_pretty(&asset)?)
    }

    pub fn peek_id(text: &str) -> Option<String> {
        codec::peek_uuid(text)
    }

    pub fn validate_structure(&self) -> Result<(), String> {
        if self.source_rig.is_some() != self.target_rig.is_some() {
            return Err("Set both IK rigs or neither".into());
        }
        if self.maps_by_chain() && !self.bone_map.is_empty() {
            return Err("A profile maps by chain or by bone, not both".into());
        }
        if !self.maps_by_chain() && !self.chain_map.is_empty() {
            return Err("Chain mappings need a source and a target IK rig".into());
        }
        ensure_unique("chain mapping", self.chain_map.iter().map(|m| m.id.as_str()))?;
        ensure_unique("bone mapping", self.bone_map.iter().map(|m| m.id.as_str()))?;
        // A target chain/bone takes its animation from exactly one source.
        ensure_unique("target chain", self.chain_map.iter().map(|m| m.target_chain.as_str()))
            .map_err(|e| format!("{e} (a target chain is mapped twice)"))?;
        ensure_unique("target bone", self.bone_map.iter().map(|m| m.target_bone.as_str()))
            .map_err(|e| format!("{e} (a target bone is mapped twice)"))?;
        for m in &self.chain_map {
            for (what, v) in [("rotation alpha", m.rotation_alpha), ("translation alpha", m.translation_alpha)] {
                if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                    return Err(format!("Chain `{}` {what} must be in [0, 1]", m.target_chain));
                }
            }
        }
        self.source_pose.validate()?;
        self.target_pose.validate()?;
        self.root.validate()
    }

    /// Problems that need no skeleton.
    pub fn reference_diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if self.source_skeleton.is_none() {
            out.push(Diagnostic::error("retarget.no_source_skeleton", "No source skeleton").at("source_skeleton").repair("Pick the source skeleton"));
        }
        if self.target_skeleton.is_none() {
            out.push(Diagnostic::error("retarget.no_target_skeleton", "No target skeleton").at("target_skeleton").repair("Pick the target skeleton"));
        }
        if self.source_skeleton.is_some() && self.source_skeleton == self.target_skeleton {
            out.push(Diagnostic::warning("retarget.same_skeleton", "Source and target are the same skeleton").at("target_skeleton"));
        }
        out
    }

    /// Checks against the two skeletons (and rigs, for chain mapping).
    pub fn diagnostics(&self, source: &Skeleton, target: &Skeleton, rigs: Option<(&IkRigAsset, &IkRigAsset)>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for bone in self.source_pose.missing_bones(source) {
            out.push(Diagnostic::error("retarget.unknown_bone", format!("Source retarget pose names bone `{bone}` which the source skeleton does not have")).at("source_pose"));
        }
        for bone in self.target_pose.missing_bones(target) {
            out.push(Diagnostic::error("retarget.unknown_bone", format!("Target retarget pose names bone `{bone}` which the target skeleton does not have")).at("target_pose"));
        }
        if let Some(root) = &self.root.source_root {
            if source.bone(root).is_none() {
                out.push(Diagnostic::error("retarget.unknown_root", format!("Source root `{root}` is not a source bone")).at("root"));
            }
        }
        if let Some(root) = &self.root.target_root {
            if target.bone(root).is_none() {
                out.push(Diagnostic::error("retarget.unknown_root", format!("Target root `{root}` is not a target bone")).at("root"));
            }
        }
        if self.root.source_root.is_none() || self.root.target_root.is_none() {
            out.push(Diagnostic::warning("retarget.no_root", "Root bones are not set; root motion and height scaling cannot be retargeted").at("root").repair("Pick the source and target root bones"));
        }
        match rigs {
            Some((source_rig, target_rig)) if self.maps_by_chain() => {
                for m in &self.chain_map {
                    if source_rig.chain(&m.source_chain).is_none() {
                        out.push(Diagnostic::error("retarget.unknown_chain", format!("Source chain `{}` is not in the source rig", m.source_chain)).at(&m.id).repair("Pick a chain of the source rig"));
                    }
                    if target_rig.chain(&m.target_chain).is_none() {
                        out.push(Diagnostic::error("retarget.unknown_chain", format!("Target chain `{}` is not in the target rig", m.target_chain)).at(&m.id).repair("Pick a chain of the target rig"));
                    }
                }
                for chain in &target_rig.chains {
                    if !self.chain_map.iter().any(|m| m.target_chain == chain.name) {
                        out.push(Diagnostic::warning("retarget.unmapped_chain", format!("Target chain `{}` has no source; it will hold its retarget pose", chain.name)).at(&chain.id).repair("Map a source chain to it"));
                    }
                }
            }
            _ => {
                for m in &self.bone_map {
                    if source.bone(&m.source_bone).is_none() {
                        out.push(Diagnostic::error("retarget.unknown_bone", format!("Mapped source bone `{}` does not exist", m.source_bone)).at(&m.id));
                    }
                    if target.bone(&m.target_bone).is_none() {
                        out.push(Diagnostic::error("retarget.unknown_bone", format!("Mapped target bone `{}` does not exist", m.target_bone)).at(&m.id));
                    }
                }
                let unmapped = target.bones.iter().filter(|b| !self.bone_map.iter().any(|m| m.target_bone == b.id)).count();
                if unmapped > 0 && !self.bone_map.is_empty() {
                    out.push(Diagnostic::warning("retarget.unmapped_bones", format!("{unmapped} target bone(s) have no source and hold their retarget pose")).at("bone_map"));
                }
                if self.bone_map.is_empty() {
                    out.push(Diagnostic::error("retarget.empty", "The profile maps nothing").at("bone_map").repair("Map bones, or set both IK rigs and map chains"));
                }
            }
        }
        out
    }
}

crate::asset_file_io!(RetargetProfileAsset, load_retarget_profile, save_retarget_profile);

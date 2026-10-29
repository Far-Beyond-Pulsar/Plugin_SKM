//! The `.ikretarget` asset (IK Retargeter): source and target IK rigs, per-chain
//! FK and IK settings, a target retarget pose and root settings.
//!
//! The retargeter pairs the *target* rig's chains with *source* rig chains by
//! name. FK settings decide how rotation and translation carry over; IK
//! settings decide how the target rig's goals follow the source's end bones.

use pulsar_reflection::{AssetRef, IkRetargeterAssetKind, IkRigAssetKind};
use serde::{Deserialize, Serialize};

use crate::codec::{self, ensure_finite, ensure_unique, from_value, read_envelope, CodecError, Extra, VersionPolicy};
use crate::diagnostics::Diagnostic;
use crate::ik_rig::{IkRigAsset, Vec3};
use crate::retarget::{RetargetPose, RotationMode, TranslationMode};

pub const IK_RETARGET_EXTENSION: &str = "ikretarget";
pub const IK_RETARGET_VERSION: u32 = 1;
const POLICY: VersionPolicy = VersionPolicy { oldest: 1, current: IK_RETARGET_VERSION, unversioned_as: None };

fn one() -> f32 {
    1.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FkSettings {
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub rotation: RotationMode,
    #[serde(default = "one")]
    pub rotation_alpha: f32,
    #[serde(default)]
    pub translation: TranslationMode,
    #[serde(default = "one")]
    pub translation_alpha: f32,
}

fn enabled() -> bool {
    true
}

impl Default for FkSettings {
    fn default() -> Self {
        Self { enabled: true, rotation: RotationMode::default(), rotation_alpha: 1.0, translation: TranslationMode::default(), translation_alpha: 1.0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IkSettings {
    /// Drive the target chain's goal from the source chain's end bone.
    #[serde(default)]
    pub enabled: bool,
    /// How far the goal follows the source (1) versus the FK result (0).
    #[serde(default = "one")]
    pub blend_to_source: f32,
    /// Offset of the goal from the source end bone, in the target's mesh space.
    #[serde(default)]
    pub static_offset: Vec3,
    /// Scales the goal's vertical offset from the root.
    #[serde(default = "one")]
    pub scale_vertical: f32,
    /// Extends (>1) or shortens the reach.
    #[serde(default = "one")]
    pub extension: f32,
}

impl Default for IkSettings {
    fn default() -> Self {
        Self { enabled: false, blend_to_source: 1.0, static_offset: [0.0; 3], scale_vertical: 1.0, extension: 1.0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChainRetarget {
    pub id: String,
    /// Chain name in the target rig.
    pub target_chain: String,
    /// Chain name in the source rig; `None` leaves the target chain unmapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_chain: Option<String>,
    #[serde(default)]
    pub fk: FkSettings,
    #[serde(default)]
    pub ik: IkSettings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RootRetarget {
    #[serde(default = "one")]
    pub rotation_alpha: f32,
    #[serde(default = "one")]
    pub translation_alpha: f32,
    #[serde(default = "one")]
    pub scale_horizontal: f32,
    #[serde(default = "one")]
    pub scale_vertical: f32,
    #[serde(default)]
    pub offset: Vec3,
}

impl Default for RootRetarget {
    fn default() -> Self {
        Self { rotation_alpha: 1.0, translation_alpha: 1.0, scale_horizontal: 1.0, scale_vertical: 1.0, offset: [0.0; 3] }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IkRetargetAsset {
    pub version: u32,
    pub id: AssetRef<IkRetargeterAssetKind>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_rig: Option<AssetRef<IkRigAssetKind>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_rig: Option<AssetRef<IkRigAssetKind>>,
    #[serde(default)]
    pub chains: Vec<ChainRetarget>,
    #[serde(default)]
    pub source_pose: RetargetPose,
    #[serde(default)]
    pub target_pose: RetargetPose,
    #[serde(default)]
    pub root: RootRetarget,
    #[serde(flatten)]
    pub extra: Extra,
}

impl IkRetargetAsset {
    pub fn new(
        name: impl Into<String>,
        source: impl Into<Option<AssetRef<IkRigAssetKind>>>,
        target: impl Into<Option<AssetRef<IkRigAssetKind>>>,
    ) -> Self {
        Self {
            version: IK_RETARGET_VERSION,
            id: AssetRef::new(),
            name: name.into(),
            source_rig: source.into(),
            target_rig: target.into(),
            chains: Vec::new(),
            source_pose: RetargetPose::default(),
            target_pose: RetargetPose::default(),
            root: RootRetarget::default(),
            extra: Extra::new(),
        }
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
        asset.version = IK_RETARGET_VERSION;
        Ok(serde_json::to_string_pretty(&asset)?)
    }

    pub fn peek_id(text: &str) -> Option<String> {
        codec::peek_uuid(text)
    }

    pub fn validate_structure(&self) -> Result<(), String> {
        if self.source_rig.is_some() && self.source_rig == self.target_rig {
            return Err("Source and target IK rig must differ".into());
        }
        ensure_unique("chain setting", self.chains.iter().map(|c| c.id.as_str()))?;
        ensure_unique("target chain", self.chains.iter().map(|c| c.target_chain.as_str()))
            .map_err(|e| format!("{e} (a target chain has two settings)"))?;
        for c in &self.chains {
            for (what, v) in [
                ("FK rotation alpha", c.fk.rotation_alpha),
                ("FK translation alpha", c.fk.translation_alpha),
                ("IK blend", c.ik.blend_to_source),
            ] {
                if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                    return Err(format!("Chain `{}` {what} must be in [0, 1]", c.target_chain));
                }
            }
            for (what, v) in [("IK vertical scale", c.ik.scale_vertical), ("IK extension", c.ik.extension)] {
                if !v.is_finite() || v <= 0.0 {
                    return Err(format!("Chain `{}` {what} must be positive", c.target_chain));
                }
            }
            c.ik.static_offset.iter().try_for_each(|v| ensure_finite("IK offset", *v))?;
        }
        for (what, v) in [("rotation alpha", self.root.rotation_alpha), ("translation alpha", self.root.translation_alpha)] {
            if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                return Err(format!("Root {what} must be in [0, 1]"));
            }
        }
        for (what, v) in [("horizontal scale", self.root.scale_horizontal), ("vertical scale", self.root.scale_vertical)] {
            if !v.is_finite() || v <= 0.0 {
                return Err(format!("Root {what} must be positive"));
            }
        }
        self.source_pose.validate()?;
        self.target_pose.validate()
    }

    /// Problems that need no rig content.
    pub fn reference_diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if self.source_rig.is_none() {
            out.push(Diagnostic::error("ikretarget.no_source_rig", "No source IK rig").at("source_rig").repair("Pick the source IK rig"));
        }
        if self.target_rig.is_none() {
            out.push(Diagnostic::error("ikretarget.no_target_rig", "No target IK rig").at("target_rig").repair("Pick the target IK rig"));
        }
        out
    }

    /// Per-chain status against the two rigs.
    pub fn diagnostics(&self, source: &IkRigAsset, target: &IkRigAsset) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for c in &self.chains {
            let Some(target_chain) = target.chain(&c.target_chain) else {
                out.push(Diagnostic::error("ikretarget.unknown_target_chain", format!("Target chain `{}` is not in the target rig", c.target_chain)).at(&c.id).repair("Pick a chain of the target rig or delete the setting"));
                continue;
            };
            match &c.source_chain {
                None => out.push(Diagnostic::warning("ikretarget.unmapped_chain", format!("Target chain `{}` has no source chain", c.target_chain)).at(&c.id).repair("Pick a source chain")),
                Some(s) if source.chain(s).is_none() => out.push(Diagnostic::error("ikretarget.unknown_source_chain", format!("Source chain `{s}` is not in the source rig")).at(&c.id).repair("Pick a chain of the source rig")),
                Some(_) => {}
            }
            if c.ik.enabled && target_chain.goal.is_none() {
                out.push(Diagnostic::error("ikretarget.ik_without_goal", format!("IK is on for `{}` but the target chain has no goal", c.target_chain)).at(&c.id).repair("Give the chain a goal in the target IK rig, or turn IK off"));
            }
        }
        for chain in &target.chains {
            if !self.chains.iter().any(|c| c.target_chain == chain.name) {
                out.push(Diagnostic::warning("ikretarget.no_setting", format!("Target chain `{}` has no retarget setting", chain.name)).at(&chain.id).repair("Add a setting and pick its source chain"));
            }
        }
        for chain in &source.chains {
            if !self.chains.iter().any(|c| c.source_chain.as_deref() == Some(&chain.name)) {
                out.push(Diagnostic::new(crate::issue::Severity::Info, "ikretarget.unused_source_chain", format!("Source chain `{}` is not used", chain.name)).at(&chain.id));
            }
        }
        if source.skeleton.is_some() && source.skeleton == target.skeleton {
            out.push(Diagnostic::new(crate::issue::Severity::Info, "ikretarget.same_skeleton", "Both rigs use the same skeleton; retargeting between them is an identity unless chains differ"));
        }
        out
    }
}

crate::asset_file_io!(IkRetargetAsset, load_ik_retarget, save_ik_retarget);

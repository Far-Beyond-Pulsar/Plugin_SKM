//! The `.ikrig` asset: bone chains, goals and an ordered solver stack on one
//! skeleton.
//!
//! A rig is reusable authoring data. The IK Rig graph node applies it to a
//! pose, and the IK Retargeter maps one rig's chains onto another's. Chains
//! and goals have stable ids (solvers and retarget settings refer to ids, never
//! to display names), and solver order is list order.

use pulsar_reflection::{AssetRef, IkRigAssetKind, SkeletonAssetKind};
use serde::{Deserialize, Serialize};

use crate::codec::{self, ensure_finite, ensure_unique, from_value, read_envelope, CodecError, Extra, VersionPolicy};
use crate::diagnostics::Diagnostic;
use crate::skeleton::Skeleton;

pub const IK_RIG_EXTENSION: &str = "ikrig";
pub const IK_RIG_VERSION: u32 = 1;
const POLICY: VersionPolicy = VersionPolicy { oldest: 1, current: IK_RIG_VERSION, unversioned_as: None };

pub type Vec3 = [f32; 3];

fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A named run of bones from `start_bone` down to `end_bone`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IkChain {
    pub id: String,
    /// Unique identifier-style name; retargeters pair chains by it.
    pub name: String,
    pub start_bone: String,
    pub end_bone: String,
    /// The goal that drives this chain's end bone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IkGoal {
    pub id: String,
    pub name: String,
    /// The bone the goal is attached to (usually a chain's end bone).
    pub bone: String,
    #[serde(default = "one")]
    pub position_alpha: f32,
    #[serde(default = "one")]
    pub rotation_alpha: f32,
    /// Where the goal sits relative to its bone when nothing drives it.
    #[serde(default)]
    pub default_offset: Vec3,
}

fn one() -> f32 {
    1.0
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoleSpace {
    #[default]
    Mesh,
    Component,
    Goal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PoleSettings {
    /// A bone to aim the joint at; `None` uses `offset` alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone: Option<String>,
    #[serde(default)]
    pub offset: Vec3,
    #[serde(default)]
    pub space: PoleSpace,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SolverKind {
    TwoBone {
        chain: String,
        goal: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pole: Option<PoleSettings>,
        #[serde(default)]
        allow_stretch: bool,
        #[serde(default = "default_stretch")]
        max_stretch: f32,
        #[serde(default)]
        take_goal_rotation: bool,
    },
    Fabrik {
        chain: String,
        goal: String,
        #[serde(default = "default_precision")]
        precision: f32,
        #[serde(default = "default_iterations")]
        max_iterations: u32,
        #[serde(default = "yes")]
        respect_joint_limits: bool,
        #[serde(default)]
        take_goal_rotation: bool,
    },
}

fn default_stretch() -> f32 {
    1.25
}
fn default_precision() -> f32 {
    0.001
}
fn default_iterations() -> u32 {
    10
}
fn yes() -> bool {
    true
}

impl SolverKind {
    pub fn chain(&self) -> &str {
        match self {
            Self::TwoBone { chain, .. } | Self::Fabrik { chain, .. } => chain,
        }
    }
    pub fn goal(&self) -> &str {
        match self {
            Self::TwoBone { goal, .. } | Self::Fabrik { goal, .. } => goal,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IkSolver {
    pub id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(flatten)]
    pub kind: SolverKind,
}

/// Rotation limits of one joint, in degrees about each local axis.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JointLimit {
    pub bone: String,
    pub min_degrees: Vec3,
    pub max_degrees: Vec3,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IkRigAsset {
    pub version: u32,
    pub id: AssetRef<IkRigAssetKind>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeleton: Option<AssetRef<SkeletonAssetKind>>,
    /// The bone retargeting scales and translates the whole character from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retarget_root: Option<String>,
    #[serde(default)]
    pub chains: Vec<IkChain>,
    #[serde(default)]
    pub goals: Vec<IkGoal>,
    /// Solvers run in this order.
    #[serde(default)]
    pub solvers: Vec<IkSolver>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joint_limits: Vec<JointLimit>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl IkRigAsset {
    pub fn new(name: impl Into<String>, skeleton: impl Into<Option<AssetRef<SkeletonAssetKind>>>) -> Self {
        Self {
            version: IK_RIG_VERSION,
            id: AssetRef::new(),
            name: name.into(),
            skeleton: skeleton.into(),
            retarget_root: None,
            chains: Vec::new(),
            goals: Vec::new(),
            solvers: Vec::new(),
            joint_limits: Vec::new(),
            extra: Extra::new(),
        }
    }

    pub fn chain(&self, name: &str) -> Option<&IkChain> {
        self.chains.iter().find(|c| c.name == name)
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
        asset.version = IK_RIG_VERSION;
        Ok(serde_json::to_string_pretty(&asset)?)
    }

    pub fn peek_id(text: &str) -> Option<String> {
        codec::peek_uuid(text)
    }

    pub fn validate_structure(&self) -> Result<(), String> {
        ensure_unique("chain", self.chains.iter().map(|c| c.id.as_str()))?;
        ensure_unique("chain name", self.chains.iter().map(|c| c.name.as_str()))?;
        ensure_unique("goal", self.goals.iter().map(|g| g.id.as_str()))?;
        ensure_unique("goal name", self.goals.iter().map(|g| g.name.as_str()))?;
        ensure_unique("solver", self.solvers.iter().map(|s| s.id.as_str()))?;
        for chain in &self.chains {
            if !identifier(&chain.name) {
                return Err(format!("Chain name `{}` must be an identifier", chain.name));
            }
            if let Some(goal) = &chain.goal {
                if !self.goals.iter().any(|g| &g.id == goal) {
                    return Err(format!("Chain `{}` uses a goal that does not exist", chain.name));
                }
            }
        }
        for goal in &self.goals {
            if !identifier(&goal.name) {
                return Err(format!("Goal name `{}` must be an identifier", goal.name));
            }
            for (what, v) in [("position alpha", goal.position_alpha), ("rotation alpha", goal.rotation_alpha)] {
                if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                    return Err(format!("Goal `{}` {what} must be in [0, 1]", goal.name));
                }
            }
            goal.default_offset.iter().try_for_each(|v| ensure_finite("goal offset", *v))?;
        }
        for solver in &self.solvers {
            if !self.chains.iter().any(|c| c.id == solver.kind.chain()) {
                return Err(format!("Solver `{}` uses a chain that does not exist", solver.name));
            }
            if !self.goals.iter().any(|g| g.id == solver.kind.goal()) {
                return Err(format!("Solver `{}` uses a goal that does not exist", solver.name));
            }
            match &solver.kind {
                SolverKind::TwoBone { max_stretch, .. } => {
                    if !max_stretch.is_finite() || *max_stretch < 1.0 {
                        return Err("Two-bone max stretch must be >= 1".into());
                    }
                }
                SolverKind::Fabrik { precision, max_iterations, .. } => {
                    if !precision.is_finite() || *precision <= 0.0 {
                        return Err("FABRIK precision must be positive".into());
                    }
                    if *max_iterations == 0 || *max_iterations > 256 {
                        return Err("FABRIK iterations must be 1..=256".into());
                    }
                }
            }
        }
        for limit in &self.joint_limits {
            for i in 0..3 {
                ensure_finite("joint limit", limit.min_degrees[i])?;
                ensure_finite("joint limit", limit.max_degrees[i])?;
                if limit.min_degrees[i] > limit.max_degrees[i] {
                    return Err(format!("Joint limit of `{}` has min above max", limit.bone));
                }
            }
        }
        ensure_unique("joint limit bone", self.joint_limits.iter().map(|l| l.bone.as_str()))?;
        Ok(())
    }

    /// The bones of `chain` from start to end in order, or `None` if the end
    /// bone is not a descendant of (or equal to) the start bone.
    pub fn chain_bones(&self, skeleton: &Skeleton, chain: &IkChain) -> Option<Vec<String>> {
        let mut bones = Vec::new();
        let mut cursor = Some(chain.end_bone.as_str());
        while let Some(id) = cursor {
            skeleton.bone(id)?;
            bones.push(id.to_string());
            if id == chain.start_bone {
                bones.reverse();
                return Some(bones);
            }
            cursor = skeleton.bone(id)?.parent.as_deref();
        }
        None
    }

    /// Problems that need no skeleton.
    pub fn reference_diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if self.skeleton.is_none() {
            out.push(Diagnostic::error("ikrig.no_skeleton", "The rig has no skeleton").repair("Pick a skeleton for the rig"));
        }
        out
    }

    /// Checks that need the skeleton the rig is authored on.
    pub fn diagnostics(&self, skeleton: &Skeleton) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let mut known = |bone: &str, what: String, at: &str| {
            if skeleton.bone(bone).is_none() {
                out.push(Diagnostic::error("ikrig.unknown_bone", format!("{what} names bone `{bone}` which the skeleton does not have")).at(at.to_string()).repair("Pick a bone of the rig's skeleton"));
            }
        };
        if let Some(root) = &self.retarget_root {
            known(root, "Retarget root".into(), "retarget_root");
        }
        for chain in &self.chains {
            known(&chain.start_bone, format!("Chain `{}` start", chain.name), &chain.id);
            known(&chain.end_bone, format!("Chain `{}` end", chain.name), &chain.id);
        }
        for goal in &self.goals {
            known(&goal.bone, format!("Goal `{}`", goal.name), &goal.id);
        }
        for limit in &self.joint_limits {
            known(&limit.bone, "Joint limit".into(), &limit.bone);
        }
        for solver in &self.solvers {
            if let SolverKind::TwoBone { pole: Some(PoleSettings { bone: Some(b), .. }), .. } = &solver.kind {
                known(b, format!("Solver `{}` pole", solver.name), &solver.id);
            }
        }
        let mut lengths: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for chain in &self.chains {
            if skeleton.bone(&chain.start_bone).is_none() || skeleton.bone(&chain.end_bone).is_none() {
                continue;
            }
            match self.chain_bones(skeleton, chain) {
                None => out.push(Diagnostic::error("ikrig.chain_not_contiguous", format!("Chain `{}`: `{}` is not below `{}`", chain.name, chain.end_bone, chain.start_bone)).at(&chain.id).repair("Swap the start and end bones or pick bones on one branch")),
                Some(bones) => {
                    lengths.insert(chain.id.as_str(), bones.len());
                }
            }
        }
        for solver in &self.solvers {
            let Some(&len) = lengths.get(solver.kind.chain()) else { continue };
            let chain_name = self.chains.iter().find(|c| c.id == solver.kind.chain()).map_or("?", |c| c.name.as_str());
            match &solver.kind {
                SolverKind::TwoBone { .. } if len != 3 => out.push(Diagnostic::error("ikrig.two_bone_length", format!("Two-bone solver `{}` needs a 3-bone chain; `{chain_name}` has {len}", solver.name)).at(&solver.id)),
                SolverKind::Fabrik { .. } if len < 2 => out.push(Diagnostic::error("ikrig.fabrik_length", format!("FABRIK solver `{}` needs a chain of 2 or more bones", solver.name)).at(&solver.id)),
                _ => {}
            }
        }
        // Two chains may share bones (a shared spine), but the same solver chain twice is a mistake.
        let mut seen = std::collections::HashSet::new();
        for solver in self.solvers.iter().filter(|s| s.enabled) {
            if !seen.insert(solver.kind.chain()) {
                out.push(Diagnostic::warning("ikrig.chain_solved_twice", format!("Solver `{}` re-solves a chain an earlier solver already solved; the later one wins", solver.name)).at(&solver.id));
            }
        }
        for goal in &self.goals {
            if !self.chains.iter().any(|c| c.goal.as_deref() == Some(&goal.id)) && !self.solvers.iter().any(|s| s.kind.goal() == goal.id) {
                out.push(Diagnostic::warning("ikrig.unused_goal", format!("Goal `{}` drives nothing", goal.name)).at(&goal.id));
            }
        }
        out
    }
}

crate::asset_file_io!(IkRigAsset, load_ik_rig, save_ik_rig);

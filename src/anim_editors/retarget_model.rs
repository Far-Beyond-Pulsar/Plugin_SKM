//! Retarget Profile and IK Retargeter editor models.
//!
//! Both pair a source with a target: skeletons and rigs are editor-resolved
//! data the view loads from the referenced assets. The chain tables are derived
//! from the asset plus those rigs, with a status per row and suggestions that
//! make missing or ambiguous chains directly actionable.

use std::path::Path;
use std::sync::Arc;

use pulsar_reflection::{AssetRef, IkRigAssetKind, SkeletonAssetKind};
use skeletal_assets::diagnostics::Diagnostic;
use skeletal_assets::ik_retarget::{ChainRetarget, FkSettings, IkRetargetAsset, IkSettings};
use skeletal_assets::ik_rig::IkRigAsset;
use skeletal_assets::math::Transform;
use skeletal_assets::retarget::{BoneMapping, BoneRotation, ChainMapping, RetargetPose, RetargetProfileAsset, RootScale, RotationMode, TranslationMode};
use skeletal_assets::skeleton::Skeleton;

use super::document::AnimDocument;
use super::selection::AnimSelection;
use super::store::DocStore;

// ---- shared pose editing -----------------------------------------------

/// Which side's retarget pose an edit targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoseSide {
    Source,
    Target,
}

fn quat_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let ([ax, ay, az, aw], [bx, by, bz, bw]) = (a, b);
    [aw * bx + ax * bw + ay * bz - az * by, aw * by - ax * bz + ay * bw + az * bx, aw * bz + ax * by - ay * bx + az * bw, aw * bw - ax * bx - ay * by - az * bz]
}

/// Quaternion `[x, y, z, w]` for XYZ Euler degrees in the engine's convention
/// (rotate about X, then Y, then Z in world axes, i.e. `Rz * Ry * Rx`: the
/// matrix `Mat4::from_euler_xyz_deg` builds).
pub fn quat_from_euler_deg(deg: [f32; 3]) -> [f32; 4] {
    let half = |d: f32| (d.to_radians() * 0.5).sin_cos();
    let ((sx, cx), (sy, cy), (sz, cz)) = (half(deg[0]), half(deg[1]), half(deg[2]));
    let qx = [sx, 0.0, 0.0, cx];
    let qy = [0.0, sy, 0.0, cy];
    let qz = [0.0, 0.0, sz, cz];
    quat_mul(quat_mul(qz, qy), qx)
}

pub fn euler_deg_from_quat(q: [f32; 4]) -> [f32; 3] {
    let t = Transform::from_trs([0.0; 3], q, [1.0; 3]);
    [t.rotation.x, t.rotation.y, t.rotation.z]
}

fn pose_mut<'a>(source: &'a mut RetargetPose, target: &'a mut RetargetPose, side: PoseSide) -> &'a mut RetargetPose {
    match side {
        PoseSide::Source => source,
        PoseSide::Target => target,
    }
}

/// Set a bone's retarget-pose rotation from Euler degrees. A zero rotation
/// removes the entry, so an untouched pose stays empty.
fn set_pose_bone(pose: &mut RetargetPose, bone: &str, deg: [f32; 3]) -> Result<bool, String> {
    if deg.iter().any(|v| !v.is_finite()) {
        return Err("angles must be numbers".into());
    }
    let identity = deg.iter().all(|v| v.abs() < 1e-5);
    let existing = pose.bone_rotations.iter().position(|b| b.bone == bone);
    match (existing, identity) {
        (Some(i), true) => {
            pose.bone_rotations.remove(i);
            Ok(true)
        }
        (None, true) => Ok(false),
        (Some(i), false) => {
            let q = quat_from_euler_deg(deg);
            let changed = pose.bone_rotations[i].rotation != q;
            pose.bone_rotations[i].rotation = q;
            Ok(changed)
        }
        (None, false) => {
            pose.bone_rotations.push(BoneRotation { bone: bone.to_string(), rotation: quat_from_euler_deg(deg) });
            Ok(true)
        }
    }
}

fn pose_euler(pose: &RetargetPose, bone: &str) -> [f32; 3] {
    pose.bone_rotations.iter().find(|b| b.bone == bone).map_or([0.0; 3], |b| euler_deg_from_quat(b.rotation))
}

fn alpha_ok(v: f32) -> Result<(), String> {
    if v.is_finite() && (0.0..=1.0).contains(&v) {
        Ok(())
    } else {
        Err("alpha must be between 0 and 1".into())
    }
}

// ---- chain status ---------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainStatus {
    Mapped,
    /// A target chain no source maps onto.
    UnmappedTarget,
    /// The mapping names a source chain the source rig does not have.
    MissingSource,
    /// The mapping names a target chain the target rig does not have.
    MissingTarget,
    /// The target chain has several plausible source chains.
    Ambiguous { candidates: Vec<String> },
}

impl ChainStatus {
    pub fn needs_action(&self) -> bool {
        !matches!(self, Self::Mapped)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChainRow {
    pub target_chain: String,
    pub source_chain: Option<String>,
    pub status: ChainStatus,
    /// Source chains that could fix this row, best first.
    pub suggestions: Vec<String>,
}

fn normalise(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase()
}

/// Source chains that plausibly correspond to `target`: same name ignoring
/// case and separators first, then names that contain each other.
pub fn suggest_chains(target: &str, source: &IkRigAsset) -> Vec<String> {
    let t = normalise(target);
    let mut exact: Vec<String> = source.chains.iter().filter(|c| normalise(&c.name) == t).map(|c| c.name.clone()).collect();
    let mut near: Vec<String> = source
        .chains
        .iter()
        .filter(|c| {
            let s = normalise(&c.name);
            s != t && !t.is_empty() && !s.is_empty() && (s.contains(&t) || t.contains(&s))
        })
        .map(|c| c.name.clone())
        .collect();
    exact.append(&mut near);
    exact
}

/// Status rows for the target rig's chains plus mappings that name chains no
/// rig has.
fn chain_rows(source: Option<&IkRigAsset>, target: Option<&IkRigAsset>, pairs: &[(String, Option<String>)]) -> Vec<ChainRow> {
    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let status_of = |t: &str, s: Option<&String>| -> (ChainStatus, Vec<String>) {
        let suggestions = source.map(|src| suggest_chains(t, src)).unwrap_or_default();
        let target_exists = target.map_or(true, |r| r.chain(t).is_some());
        if !target_exists {
            return (ChainStatus::MissingTarget, suggestions);
        }
        match s {
            None => {
                if suggestions.len() > 1 {
                    (ChainStatus::Ambiguous { candidates: suggestions.clone() }, suggestions)
                } else {
                    (ChainStatus::UnmappedTarget, suggestions)
                }
            }
            Some(name) if source.is_some_and(|r| r.chain(name).is_none()) => (ChainStatus::MissingSource, suggestions),
            Some(_) => (ChainStatus::Mapped, suggestions),
        }
    };
    if let Some(target) = target {
        for chain in &target.chains {
            let source_chain = pairs.iter().find(|(t, _)| *t == chain.name).and_then(|(_, s)| s.clone());
            let (status, suggestions) = status_of(&chain.name, source_chain.as_ref());
            seen.insert(chain.name.clone());
            rows.push(ChainRow { target_chain: chain.name.clone(), source_chain, status, suggestions });
        }
    }
    for (t, s) in pairs {
        if seen.contains(t) {
            continue;
        }
        let (status, suggestions) = status_of(t, s.as_ref());
        rows.push(ChainRow { target_chain: t.clone(), source_chain: s.clone(), status, suggestions });
    }
    rows
}

// ---- Retarget Profile ------------------------------------------------------

pub struct RetargetProfileModel {
    pub doc: AnimDocument<RetargetProfileAsset>,
    pub selection: AnimSelection,
    pub source_skeleton: Option<Skeleton>,
    pub target_skeleton: Option<Skeleton>,
    pub source_rig: Option<IkRigAsset>,
    pub target_rig: Option<IkRigAsset>,
}

impl RetargetProfileModel {
    pub fn open(store: Arc<dyn DocStore>, path: &Path, default_text: &str) -> Result<Self, String> {
        let doc = AnimDocument::<RetargetProfileAsset>::open(store, path, default_text)?;
        Ok(Self { doc, selection: AnimSelection::None, source_skeleton: None, target_skeleton: None, source_rig: None, target_rig: None })
    }

    pub fn asset(&self) -> &RetargetProfileAsset {
        self.doc.value()
    }

    pub fn set_skeletons(&mut self, source: Option<AssetRef<SkeletonAssetKind>>, target: Option<AssetRef<SkeletonAssetKind>>) -> bool {
        self.doc.edit("Set skeletons", None, |a| {
            let changed = a.source_skeleton != source || a.target_skeleton != target;
            a.source_skeleton = source;
            a.target_skeleton = target;
            (changed, changed)
        })
    }

    /// Set both IK rigs, or neither. Switching to or from rig mapping clears the
    /// other mapping table, which the format forbids holding both of.
    pub fn set_rigs(&mut self, source: Option<AssetRef<IkRigAssetKind>>, target: Option<AssetRef<IkRigAssetKind>>) -> Result<(), String> {
        if source.is_some() != target.is_some() {
            return Err("set both IK rigs, or neither".into());
        }
        let by_chain = source.is_some();
        self.doc.edit("Set IK rigs", None, |a| {
            let changed = a.source_rig != source || a.target_rig != target;
            a.source_rig = source;
            a.target_rig = target;
            if by_chain {
                a.bone_map.clear();
            } else {
                a.chain_map.clear();
            }
            (changed, ())
        });
        Ok(())
    }

    pub fn rows(&self) -> Vec<ChainRow> {
        let pairs: Vec<_> = self.asset().chain_map.iter().map(|m| (m.target_chain.clone(), Some(m.source_chain.clone()))).collect();
        chain_rows(self.source_rig.as_ref(), self.target_rig.as_ref(), &pairs)
    }

    pub fn select_chain(&mut self, target_chain: &str) -> bool {
        if self.rows().iter().any(|r| r.target_chain == target_chain) {
            self.selection = AnimSelection::RetargetChain { name: target_chain.into() };
            true
        } else {
            false
        }
    }

    fn mapping_index(&self, target_chain: &str) -> Option<usize> {
        self.asset().chain_map.iter().position(|m| m.target_chain == target_chain)
    }

    /// Map `source_chain` onto `target_chain`, replacing any earlier mapping of
    /// that target. Both must exist in their rigs.
    pub fn map_chain(&mut self, source_chain: &str, target_chain: &str) -> Result<(), String> {
        if !self.asset().maps_by_chain() {
            return Err("set both IK rigs to map by chain".into());
        }
        if self.source_rig.as_ref().is_some_and(|r| r.chain(source_chain).is_none()) {
            return Err(format!("`{source_chain}` is not a chain of the source rig"));
        }
        if self.target_rig.as_ref().is_some_and(|r| r.chain(target_chain).is_none()) {
            return Err(format!("`{target_chain}` is not a chain of the target rig"));
        }
        let (s, t) = (source_chain.to_string(), target_chain.to_string());
        let existing = self.mapping_index(target_chain);
        self.doc.apply("Map chain", |a| match existing {
            Some(i) => a.chain_map[i].source_chain = s,
            None => a.chain_map.push(ChainMapping {
                id: uuid::Uuid::new_v4().to_string(),
                source_chain: s,
                target_chain: t,
                rotation: RotationMode::default(),
                rotation_alpha: 1.0,
                translation: TranslationMode::default(),
                translation_alpha: 1.0,
            }),
        });
        self.selection = AnimSelection::RetargetChain { name: target_chain.into() };
        Ok(())
    }

    pub fn unmap_chain(&mut self, target_chain: &str) -> bool {
        let Some(i) = self.mapping_index(target_chain) else { return false };
        self.doc.apply("Unmap chain", |a| {
            a.chain_map.remove(i);
        });
        true
    }

    /// Map every unmapped target chain that has exactly one same-named source
    /// chain (ignoring case and separators). Ambiguous ones are left for the
    /// user. Returns how many were mapped.
    pub fn auto_map_chains(&mut self) -> usize {
        let (Some(source), Some(target)) = (self.source_rig.clone(), self.target_rig.clone()) else { return 0 };
        let todo: Vec<(String, String)> = target
            .chains
            .iter()
            .filter(|c| self.mapping_index(&c.name).is_none())
            .filter_map(|c| {
                let exact: Vec<_> = suggest_chains(&c.name, &source).into_iter().filter(|s| normalise(s) == normalise(&c.name)).collect();
                (exact.len() == 1).then(|| (exact[0].clone(), c.name.clone()))
            })
            .collect();
        if todo.is_empty() {
            return 0;
        }
        let count = todo.len();
        self.doc.apply("Auto-map chains", |a| {
            for (s, t) in todo {
                a.chain_map.push(ChainMapping {
                    id: uuid::Uuid::new_v4().to_string(),
                    source_chain: s,
                    target_chain: t,
                    rotation: RotationMode::default(),
                    rotation_alpha: 1.0,
                    translation: TranslationMode::default(),
                    translation_alpha: 1.0,
                });
            }
        });
        count
    }

    pub fn set_chain_rotation(&mut self, target_chain: &str, mode: RotationMode, alpha: f32) -> Result<(), String> {
        alpha_ok(alpha)?;
        let i = self.mapping_index(target_chain).ok_or("that chain is not mapped")?;
        self.doc.edit("Set chain rotation", Some(&format!("crot:{target_chain}")), |a| {
            a.chain_map[i].rotation = mode;
            a.chain_map[i].rotation_alpha = alpha;
            (true, ())
        });
        Ok(())
    }

    pub fn set_chain_translation(&mut self, target_chain: &str, mode: TranslationMode, alpha: f32) -> Result<(), String> {
        alpha_ok(alpha)?;
        let i = self.mapping_index(target_chain).ok_or("that chain is not mapped")?;
        self.doc.edit("Set chain translation", Some(&format!("ctr:{target_chain}")), |a| {
            a.chain_map[i].translation = mode;
            a.chain_map[i].translation_alpha = alpha;
            (true, ())
        });
        Ok(())
    }

    // bone mapping (no rigs) -----------------------------------------------------

    pub fn map_bone(&mut self, source_bone: &str, target_bone: &str) -> Result<(), String> {
        if self.asset().maps_by_chain() {
            return Err("this profile maps by chain".into());
        }
        if self.source_skeleton.as_ref().is_some_and(|s| s.bone(source_bone).is_none()) || self.target_skeleton.as_ref().is_some_and(|s| s.bone(target_bone).is_none()) {
            return Err("pick bones of the source and target skeletons".into());
        }
        let existing = self.asset().bone_map.iter().position(|m| m.target_bone == target_bone);
        let (s, t) = (source_bone.to_string(), target_bone.to_string());
        self.doc.apply("Map bone", |a| match existing {
            Some(i) => a.bone_map[i].source_bone = s,
            None => a.bone_map.push(BoneMapping { id: uuid::Uuid::new_v4().to_string(), source_bone: s, target_bone: t, rotation: RotationMode::default(), translation: TranslationMode::default() }),
        });
        Ok(())
    }

    pub fn unmap_bone(&mut self, target_bone: &str) -> bool {
        let before = self.asset().bone_map.len();
        if !self.asset().bone_map.iter().any(|m| m.target_bone == target_bone) {
            return false;
        }
        self.doc.apply("Unmap bone", |a| a.bone_map.retain(|m| m.target_bone != target_bone));
        before != self.asset().bone_map.len()
    }

    /// Map target bones to same-named source bones. Returns how many were added.
    pub fn auto_map_bones(&mut self) -> usize {
        let (Some(source), Some(target)) = (&self.source_skeleton, &self.target_skeleton) else { return 0 };
        if self.asset().maps_by_chain() {
            return 0;
        }
        let todo: Vec<(String, String)> = target
            .bones
            .iter()
            .filter(|t| !self.asset().bone_map.iter().any(|m| m.target_bone == t.id))
            .filter_map(|t| source.bones.iter().find(|s| normalise(&s.name) == normalise(&t.name)).map(|s| (s.id.clone(), t.id.clone())))
            .collect();
        let count = todo.len();
        if count > 0 {
            self.doc.apply("Auto-map bones", |a| {
                for (s, t) in todo {
                    a.bone_map.push(BoneMapping { id: uuid::Uuid::new_v4().to_string(), source_bone: s, target_bone: t, rotation: RotationMode::default(), translation: TranslationMode::default() });
                }
            });
        }
        count
    }

    // root + pose ---------------------------------------------------------------

    pub fn set_roots(&mut self, source: Option<&str>, target: Option<&str>) -> Result<(), String> {
        if source.is_some_and(|b| self.source_skeleton.as_ref().is_some_and(|s| s.bone(b).is_none())) || target.is_some_and(|b| self.target_skeleton.as_ref().is_some_and(|s| s.bone(b).is_none())) {
            return Err("pick bones of the source and target skeletons".into());
        }
        let (s, t) = (source.map(str::to_string), target.map(str::to_string));
        self.doc.edit("Set root bones", None, |a| {
            let changed = a.root.source_root != s || a.root.target_root != t;
            a.root.source_root = s;
            a.root.target_root = t;
            (changed, ())
        });
        Ok(())
    }

    pub fn set_root_scale(&mut self, scale: RootScale) -> Result<(), String> {
        if let RootScale::Fixed { factor } = scale {
            if !factor.is_finite() || factor <= 0.0 {
                return Err("scale factor must be positive".into());
            }
        }
        self.doc.edit("Set root scale", Some("rscale"), |a| {
            a.root.scale = scale;
            (true, ())
        });
        Ok(())
    }

    pub fn set_root_alphas(&mut self, translation: f32, rotation: f32) -> Result<(), String> {
        alpha_ok(translation)?;
        alpha_ok(rotation)?;
        self.doc.edit("Set root alphas", Some("ralpha"), |a| {
            a.root.translation_alpha = translation;
            a.root.rotation_alpha = rotation;
            (true, ())
        });
        Ok(())
    }

    /// Rotate one bone of a retarget pose (the "retarget pose" tool).
    pub fn set_pose_bone(&mut self, side: PoseSide, bone: &str, euler_deg: [f32; 3]) -> Result<bool, String> {
        let known = match side {
            PoseSide::Source => self.source_skeleton.as_ref(),
            PoseSide::Target => self.target_skeleton.as_ref(),
        };
        if known.is_some_and(|s| s.bone(bone).is_none()) {
            return Err("pick a bone of that skeleton".into());
        }
        let bone = bone.to_string();
        let mut result = Ok(false);
        self.doc.edit("Edit retarget pose", Some(&format!("pose:{side:?}:{bone}")), |a| {
            let RetargetProfileAsset { source_pose, target_pose, .. } = a;
            result = set_pose_bone(pose_mut(source_pose, target_pose, side), &bone, euler_deg);
            let changed = matches!(result, Ok(true));
            (changed, ())
        });
        result
    }

    pub fn pose_bone_euler(&self, side: PoseSide, bone: &str) -> [f32; 3] {
        let a = self.asset();
        pose_euler(if side == PoseSide::Source { &a.source_pose } else { &a.target_pose }, bone)
    }

    pub fn reset_pose(&mut self, side: PoseSide) -> bool {
        self.doc.edit("Reset retarget pose", None, |a| {
            let pose = pose_mut(&mut a.source_pose, &mut a.target_pose, side);
            let changed = *pose != RetargetPose::default();
            *pose = RetargetPose::default();
            (changed, changed)
        })
    }

    pub fn rename(&mut self, name: &str) -> bool {
        let name = name.trim().to_string();
        if name.is_empty() {
            return false;
        }
        self.doc.edit("Rename", Some("rename"), |a| {
            let changed = a.name != name;
            a.name = name;
            (changed, changed)
        })
    }

    pub fn validation(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if let Err(m) = self.asset().validate_structure() {
            out.push(Diagnostic::error("retarget.invalid", m));
        }
        out.extend(self.asset().reference_diagnostics());
        if let (Some(s), Some(t)) = (&self.source_skeleton, &self.target_skeleton) {
            let rigs = self.source_rig.as_ref().zip(self.target_rig.as_ref());
            out.extend(self.asset().diagnostics(s, t, rigs));
        }
        out
    }

    pub fn save(&mut self) -> anyhow::Result<super::document::SaveOutcome> {
        self.doc.save()
    }
}

// ---- IK Retargeter -----------------------------------------------------------

pub struct IkRetargetModel {
    pub doc: AnimDocument<IkRetargetAsset>,
    pub selection: AnimSelection,
    pub source_rig: Option<IkRigAsset>,
    pub target_rig: Option<IkRigAsset>,
    pub source_skeleton: Option<Skeleton>,
    pub target_skeleton: Option<Skeleton>,
    /// Side-by-side or overlaid preview. Editor state.
    pub overlay: bool,
}

impl IkRetargetModel {
    pub fn open(store: Arc<dyn DocStore>, path: &Path, default_text: &str) -> Result<Self, String> {
        let doc = AnimDocument::<IkRetargetAsset>::open(store, path, default_text)?;
        Ok(Self { doc, selection: AnimSelection::None, source_rig: None, target_rig: None, source_skeleton: None, target_skeleton: None, overlay: false })
    }

    pub fn asset(&self) -> &IkRetargetAsset {
        self.doc.value()
    }

    pub fn set_rigs(&mut self, source: Option<AssetRef<IkRigAssetKind>>, target: Option<AssetRef<IkRigAssetKind>>) -> Result<(), String> {
        if source.is_some() && source == target {
            return Err("the source and target rig must differ".into());
        }
        self.doc.edit("Set IK rigs", None, |a| {
            let changed = a.source_rig != source || a.target_rig != target;
            a.source_rig = source;
            a.target_rig = target;
            (changed, ())
        });
        Ok(())
    }

    fn chain_index(&self, target_chain: &str) -> Option<usize> {
        self.asset().chains.iter().position(|c| c.target_chain == target_chain)
    }

    /// Add a setting for every target-rig chain that has none, pairing it with
    /// the single same-named source chain when there is one. Settings for
    /// chains the target rig no longer has are kept (and reported). Returns the
    /// number added.
    pub fn sync_chains(&mut self) -> usize {
        let Some(target) = self.target_rig.clone() else { return 0 };
        let source = self.source_rig.clone();
        let add: Vec<ChainRetarget> = target
            .chains
            .iter()
            .filter(|c| self.chain_index(&c.name).is_none())
            .map(|c| {
                let exact = source.as_ref().map(|s| suggest_chains(&c.name, s).into_iter().filter(|n| normalise(n) == normalise(&c.name)).collect::<Vec<_>>()).unwrap_or_default();
                ChainRetarget {
                    id: uuid::Uuid::new_v4().to_string(),
                    target_chain: c.name.clone(),
                    source_chain: (exact.len() == 1).then(|| exact[0].clone()),
                    fk: FkSettings::default(),
                    ik: IkSettings { enabled: c.goal.is_some(), ..IkSettings::default() },
                }
            })
            .collect();
        let count = add.len();
        if count > 0 {
            self.doc.apply("Add chain settings", |a| a.chains.extend(add));
        }
        count
    }

    pub fn rows(&self) -> Vec<ChainRow> {
        let pairs: Vec<_> = self.asset().chains.iter().map(|c| (c.target_chain.clone(), c.source_chain.clone())).collect();
        chain_rows(self.source_rig.as_ref(), self.target_rig.as_ref(), &pairs)
    }

    pub fn select_chain(&mut self, target_chain: &str) -> bool {
        if self.chain_index(target_chain).is_some() {
            self.selection = AnimSelection::RetargetChain { name: target_chain.into() };
            true
        } else {
            false
        }
    }

    pub fn set_source_chain(&mut self, target_chain: &str, source: Option<&str>) -> Result<(), String> {
        let i = self.chain_index(target_chain).ok_or("no setting for that chain")?;
        if let (Some(s), Some(rig)) = (source, &self.source_rig) {
            if rig.chain(s).is_none() {
                return Err(format!("`{s}` is not a chain of the source rig"));
            }
        }
        let source = source.map(str::to_string);
        self.doc.edit("Set source chain", None, |a| {
            let changed = a.chains[i].source_chain != source;
            a.chains[i].source_chain = source;
            (changed, ())
        });
        Ok(())
    }

    pub fn delete_chain_setting(&mut self, target_chain: &str) -> bool {
        let Some(i) = self.chain_index(target_chain) else { return false };
        self.doc.apply("Delete chain setting", |a| {
            a.chains.remove(i);
        });
        true
    }

    pub fn set_fk(&mut self, target_chain: &str, fk: FkSettings) -> Result<(), String> {
        alpha_ok(fk.rotation_alpha)?;
        alpha_ok(fk.translation_alpha)?;
        let i = self.chain_index(target_chain).ok_or("no setting for that chain")?;
        self.doc.edit("Edit FK", Some(&format!("fk:{target_chain}")), |a| {
            let changed = a.chains[i].fk != fk;
            a.chains[i].fk = fk;
            (changed, ())
        });
        Ok(())
    }

    /// Edit IK settings. IK needs a goal on the target chain when the rig is known.
    pub fn set_ik(&mut self, target_chain: &str, ik: IkSettings) -> Result<(), String> {
        alpha_ok(ik.blend_to_source)?;
        if !ik.scale_vertical.is_finite() || ik.scale_vertical <= 0.0 || !ik.extension.is_finite() || ik.extension <= 0.0 {
            return Err("scale and extension must be positive".into());
        }
        if ik.static_offset.iter().any(|v| !v.is_finite()) {
            return Err("offset must be numbers".into());
        }
        let i = self.chain_index(target_chain).ok_or("no setting for that chain")?;
        if ik.enabled {
            if let Some(chain) = self.target_rig.as_ref().and_then(|r| r.chain(target_chain)) {
                if chain.goal.is_none() {
                    return Err("the target chain has no IK goal; give it one in the target IK rig".into());
                }
            }
        }
        self.doc.edit("Edit IK", Some(&format!("ik:{target_chain}")), |a| {
            let changed = a.chains[i].ik != ik;
            a.chains[i].ik = ik;
            (changed, ())
        });
        Ok(())
    }

    pub fn set_root(&mut self, edit: impl FnOnce(&mut skeletal_assets::ik_retarget::RootRetarget)) -> Result<(), String> {
        let mut candidate = self.asset().root.clone();
        edit(&mut candidate);
        alpha_ok(candidate.rotation_alpha)?;
        alpha_ok(candidate.translation_alpha)?;
        if !candidate.scale_horizontal.is_finite() || candidate.scale_horizontal <= 0.0 || !candidate.scale_vertical.is_finite() || candidate.scale_vertical <= 0.0 {
            return Err("scales must be positive".into());
        }
        self.doc.edit("Edit root", Some("root"), |a| {
            a.root = candidate;
            (true, ())
        });
        Ok(())
    }

    pub fn set_pose_bone(&mut self, side: PoseSide, bone: &str, euler_deg: [f32; 3]) -> Result<bool, String> {
        let known = match side {
            PoseSide::Source => self.source_skeleton.as_ref(),
            PoseSide::Target => self.target_skeleton.as_ref(),
        };
        if known.is_some_and(|s| s.bone(bone).is_none()) {
            return Err("pick a bone of that skeleton".into());
        }
        let bone = bone.to_string();
        let mut result = Ok(false);
        self.doc.edit("Edit retarget pose", Some(&format!("pose:{side:?}:{bone}")), |a| {
            let IkRetargetAsset { source_pose, target_pose, .. } = a;
            result = set_pose_bone(pose_mut(source_pose, target_pose, side), &bone, euler_deg);
            (matches!(result, Ok(true)), ())
        });
        result
    }

    pub fn pose_bone_euler(&self, side: PoseSide, bone: &str) -> [f32; 3] {
        let a = self.asset();
        pose_euler(if side == PoseSide::Source { &a.source_pose } else { &a.target_pose }, bone)
    }

    pub fn reset_pose(&mut self, side: PoseSide) -> bool {
        self.doc.edit("Reset retarget pose", None, |a| {
            let pose = pose_mut(&mut a.source_pose, &mut a.target_pose, side);
            let changed = *pose != RetargetPose::default();
            *pose = RetargetPose::default();
            (changed, changed)
        })
    }

    pub fn validation(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if let Err(m) = self.asset().validate_structure() {
            out.push(Diagnostic::error("ikretarget.invalid", m));
        }
        out.extend(self.asset().reference_diagnostics());
        if let (Some(s), Some(t)) = (&self.source_rig, &self.target_rig) {
            out.extend(self.asset().diagnostics(s, t));
        }
        out
    }

    pub fn save(&mut self) -> anyhow::Result<super::document::SaveOutcome> {
        self.doc.save()
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::MemStore;
    use super::*;
    use skeletal_assets::ik_rig::IkChain;

    fn rig(name: &str, chains: &[(&str, Option<&str>)]) -> IkRigAsset {
        let mut r = IkRigAsset::new(name, None);
        for (c, goal) in chains {
            let goal_id = goal.map(|g| {
                let id = format!("goal-{g}");
                r.goals.push(skeletal_assets::ik_rig::IkGoal { id: id.clone(), name: g.to_string(), bone: "b".into(), position_alpha: 1.0, rotation_alpha: 1.0, default_offset: [0.0; 3] });
                id
            });
            r.chains.push(IkChain { id: format!("id-{c}"), name: c.to_string(), start_bone: "a".into(), end_bone: "b".into(), goal: goal_id });
        }
        r
    }

    fn profile() -> RetargetProfileModel {
        let store = MemStore::default();
        let mut m = RetargetProfileModel::open(Arc::new(store), Path::new("p.animretarget"), &RetargetProfileAsset::new("P", None, None).to_text().unwrap()).unwrap();
        m.source_rig = Some(rig("S", &[("Spine", None), ("LeftArm", None), ("RightArm", None), ("Leg_L", None)]));
        m.target_rig = Some(rig("T", &[("spine", None), ("left_arm", None), ("RightArm", None), ("Tail", None), ("Leg", None)]));
        m.set_rigs(Some(AssetRef::new()), Some(AssetRef::new())).unwrap();
        m
    }

    #[test]
    fn euler_pose_edits_round_trip_and_identity_removes_the_entry() {
        for deg in [[10.0, 20.0, 30.0], [-45.0, 0.0, 90.0], [0.0, 60.0, -10.0]] {
            let back = euler_deg_from_quat(quat_from_euler_deg(deg));
            for i in 0..3 {
                assert!((back[i] - deg[i]).abs() < 1e-3, "{deg:?} -> {back:?}");
            }
        }
        let q = quat_from_euler_deg([30.0, 40.0, 50.0]);
        let len: f32 = q.iter().map(|v| v * v).sum();
        assert!((len - 1.0).abs() < 1e-5, "unit quaternion");
        let mut pose = RetargetPose::default();
        assert_eq!(set_pose_bone(&mut pose, "arm", [0.0; 3]), Ok(false));
        assert_eq!(set_pose_bone(&mut pose, "arm", [0.0, 0.0, 45.0]), Ok(true));
        assert_eq!(set_pose_bone(&mut pose, "arm", [0.0, 0.0, 45.0]), Ok(false));
        assert!(set_pose_bone(&mut pose, "arm", [f32::NAN, 0.0, 0.0]).is_err());
        assert_eq!(set_pose_bone(&mut pose, "arm", [0.0; 3]), Ok(true));
        assert!(pose.bone_rotations.is_empty());
        assert!(pose.validate().is_ok());
    }

    #[test]
    fn chain_table_flags_unmapped_missing_and_ambiguous_rows_with_suggestions() {
        let mut m = profile();
        let rows = m.rows();
        let row = |name: &str| rows.iter().find(|r| r.target_chain == name).unwrap().clone();
        assert_eq!(row("Tail").status, ChainStatus::UnmappedTarget);
        assert!(row("Tail").suggestions.is_empty());
        assert_eq!(row("RightArm").suggestions, ["RightArm"]);
        assert_eq!(row("left_arm").suggestions, ["LeftArm"], "separators and case are ignored");
        // Leg is contained in Leg_L only: a near suggestion, still unmapped
        assert_eq!(row("Leg").status, ChainStatus::UnmappedTarget);
        assert_eq!(row("Leg").suggestions, ["Leg_L"]);
        m.map_chain("Spine", "spine").unwrap();
        assert_eq!(m.rows().iter().find(|r| r.target_chain == "spine").unwrap().status, ChainStatus::Mapped);
        assert!(m.map_chain("Nope", "spine").is_err());
        assert!(m.map_chain("Spine", "Nope").is_err());
        // a mapping to a chain that left the rig is a missing-target row
        m.doc.apply("break", |a| a.chain_map.push(ChainMapping { id: "x".into(), source_chain: "Spine".into(), target_chain: "Gone".into(), rotation: RotationMode::None, rotation_alpha: 1.0, translation: TranslationMode::None, translation_alpha: 1.0 }));
        assert_eq!(m.rows().iter().find(|r| r.target_chain == "Gone").unwrap().status, ChainStatus::MissingTarget);
        m.source_skeleton = Some(super::super::ik_rig_model::tests::skeleton());
        m.target_skeleton = Some(super::super::ik_rig_model::tests::skeleton());
        assert!(m.validation().iter().any(|d| d.code == "retarget.unknown_chain"));
    }

    #[test]
    fn auto_map_takes_exact_matches_only_and_remap_replaces() {
        let mut m = profile();
        assert_eq!(m.auto_map_chains(), 3, "spine, left_arm and RightArm have exactly one same-named source");
        assert_eq!(m.auto_map_chains(), 0);
        assert_eq!(m.asset().chain_map.len(), 3);
        m.map_chain("LeftArm", "RightArm").unwrap();
        assert_eq!(m.asset().chain_map.iter().find(|c| c.target_chain == "RightArm").unwrap().source_chain, "LeftArm");
        assert_eq!(m.asset().chain_map.len(), 3, "remapping a target replaces its mapping");
        assert!(m.unmap_chain("RightArm") && !m.unmap_chain("RightArm"));
        assert!(m.asset().validate_structure().is_ok());
    }

    #[test]
    fn chain_settings_and_root_are_validated_and_rigs_are_both_or_neither() {
        let mut m = profile();
        m.auto_map_chains();
        assert!(m.set_chain_rotation("spine", RotationMode::OneToOne, 1.5).is_err());
        m.set_chain_rotation("spine", RotationMode::OneToOne, 0.5).unwrap();
        m.set_chain_translation("spine", TranslationMode::GloballyScaled, 0.25).unwrap();
        assert!(m.set_chain_rotation("Tail", RotationMode::None, 1.0).is_err(), "unmapped");
        assert!(m.set_root_scale(RootScale::Fixed { factor: 0.0 }).is_err());
        m.set_root_scale(RootScale::Fixed { factor: 0.9 }).unwrap();
        assert!(m.set_root_alphas(2.0, 1.0).is_err());
        assert!(m.set_rigs(Some(AssetRef::new()), None).is_err());
        m.set_rigs(None, None).unwrap();
        assert!(m.asset().chain_map.is_empty(), "leaving rig mode drops chain mappings");
        assert!(m.map_chain("Spine", "spine").is_err());
    }

    #[test]
    fn bone_mapping_mode_maps_by_name_and_refuses_chain_ops() {
        let store = MemStore::default();
        let mut m = RetargetProfileModel::open(Arc::new(store), Path::new("b.animretarget"), &RetargetProfileAsset::new("B", None, None).to_text().unwrap()).unwrap();
        m.source_skeleton = Some(super::super::ik_rig_model::tests::skeleton());
        m.target_skeleton = Some(super::super::ik_rig_model::tests::skeleton());
        assert_eq!(m.auto_map_bones(), 6);
        assert_eq!(m.auto_map_bones(), 0);
        assert!(m.map_bone("ghost", "foot").is_err());
        m.map_bone("shin", "foot").unwrap();
        assert_eq!(m.asset().bone_map.iter().find(|b| b.target_bone == "foot").unwrap().source_bone, "shin");
        assert!(m.unmap_bone("foot"));
        m.set_roots(Some("root"), Some("root")).unwrap();
        assert!(m.set_roots(Some("ghost"), None).is_err());
        assert!(m.map_chain("a", "b").is_err());
    }

    #[test]
    fn retarget_poses_edit_per_side_and_reset() {
        let mut m = profile();
        m.source_skeleton = Some(super::super::ik_rig_model::tests::skeleton());
        m.target_skeleton = Some(super::super::ik_rig_model::tests::skeleton());
        assert!(m.set_pose_bone(PoseSide::Target, "ghost", [1.0, 0.0, 0.0]).is_err());
        assert_eq!(m.set_pose_bone(PoseSide::Target, "shin", [0.0, 0.0, 30.0]), Ok(true));
        assert!(m.asset().source_pose.bone_rotations.is_empty());
        assert!((m.pose_bone_euler(PoseSide::Target, "shin")[2] - 30.0).abs() < 1e-3);
        assert!(m.reset_pose(PoseSide::Target) && !m.reset_pose(PoseSide::Target));
        // a drag over one bone is one undo step
        for z in [5.0, 10.0, 15.0] {
            m.set_pose_bone(PoseSide::Source, "shin", [0.0, 0.0, z]).unwrap();
        }
        m.doc.end_gesture();
        assert!(m.doc.undo());
        assert!(m.asset().source_pose.bone_rotations.is_empty());
    }

    fn ik_model() -> IkRetargetModel {
        let store = MemStore::default();
        let mut m = IkRetargetModel::open(Arc::new(store), Path::new("r.ikretarget"), &IkRetargetAsset::new("R", None, None).to_text().unwrap()).unwrap();
        m.source_rig = Some(rig("S", &[("Spine", None), ("Arm", None)]));
        m.target_rig = Some(rig("T", &[("spine", None), ("arm", Some("HandGoal")), ("Wing", None)]));
        m.set_rigs(Some(AssetRef::new()), Some(AssetRef::new())).unwrap();
        m
    }

    #[test]
    fn ik_retargeter_syncs_chain_settings_and_reports_status() {
        let mut m = ik_model();
        assert_eq!(m.sync_chains(), 3);
        assert_eq!(m.sync_chains(), 0);
        let rows = m.rows();
        assert_eq!(rows.iter().find(|r| r.target_chain == "spine").unwrap().status, ChainStatus::Mapped);
        assert_eq!(rows.iter().find(|r| r.target_chain == "Wing").unwrap().status, ChainStatus::UnmappedTarget);
        assert!(m.asset().chains.iter().find(|c| c.target_chain == "arm").unwrap().ik.enabled, "IK defaults on for a chain with a goal");
        assert!(!m.asset().chains.iter().find(|c| c.target_chain == "spine").unwrap().ik.enabled);
        m.set_source_chain("Wing", Some("Arm")).unwrap();
        assert!(m.set_source_chain("Wing", Some("Ghost")).is_err());
        assert!(m.set_source_chain("Nope", None).is_err());
        assert!(m.validation().iter().all(|d| d.code != "ikretarget.unmapped_chain"));
    }

    #[test]
    fn ik_settings_need_a_goal_and_sane_values() {
        let mut m = ik_model();
        m.sync_chains();
        let on = IkSettings { enabled: true, ..IkSettings::default() };
        assert!(m.set_ik("spine", on.clone()).is_err(), "no goal on the target chain");
        m.set_ik("arm", IkSettings { blend_to_source: 0.5, extension: 1.2, ..on.clone() }).unwrap();
        assert!(m.set_ik("arm", IkSettings { blend_to_source: 2.0, ..on.clone() }).is_err());
        assert!(m.set_ik("arm", IkSettings { extension: 0.0, ..on.clone() }).is_err());
        assert!(m.set_ik("arm", IkSettings { static_offset: [f32::INFINITY, 0.0, 0.0], ..on }).is_err());
        assert!(m.set_fk("arm", FkSettings { rotation_alpha: -1.0, ..FkSettings::default() }).is_err());
        m.set_fk("arm", FkSettings { enabled: false, rotation: RotationMode::OneToOneReversed, ..FkSettings::default() }).unwrap();
        assert!(m.set_root(|r| r.scale_horizontal = 0.0).is_err());
        m.set_root(|r| {
            r.scale_vertical = 1.1;
            r.offset = [0.0, 0.05, 0.0];
        })
        .unwrap();
        assert!(m.set_rigs(Some(AssetRef::parse("11111111-1111-4111-8111-111111111111").unwrap()), Some(AssetRef::parse("11111111-1111-4111-8111-111111111111").unwrap())).is_err());
        assert!(m.delete_chain_setting("Wing") && !m.delete_chain_setting("Wing"));
        assert!(m.validation().iter().any(|d| d.code == "ikretarget.no_setting"), "deleting a setting makes the chain actionable again");
    }

    #[test]
    fn retargeter_round_trips_through_save_and_reopen() {
        let store = MemStore::default();
        let mut m = IkRetargetModel::open(Arc::new(store.clone()), Path::new("r.ikretarget"), &IkRetargetAsset::new("R", None, None).to_text().unwrap()).unwrap();
        m.source_rig = Some(rig("S", &[("Arm", None)]));
        m.target_rig = Some(rig("T", &[("arm", Some("G"))]));
        let (s, t) = (AssetRef::new(), AssetRef::new());
        m.set_rigs(Some(s.clone()), Some(t.clone())).unwrap();
        m.sync_chains();
        m.set_ik("arm", IkSettings { enabled: true, blend_to_source: 0.75, extension: 1.1, static_offset: [0.0, 0.1, 0.0], scale_vertical: 1.0 }).unwrap();
        m.set_root(|r| r.translation_alpha = 0.5).unwrap();
        m.overlay = true;
        m.save().unwrap();
        let r = IkRetargetModel::open(Arc::new(store), Path::new("r.ikretarget"), "").unwrap();
        assert_eq!((r.asset().source_rig.clone(), r.asset().target_rig.clone()), (Some(s), Some(t)));
        let c = &r.asset().chains[0];
        assert_eq!((c.ik.blend_to_source, c.ik.extension, c.ik.static_offset, c.source_chain.as_deref()), (0.75, 1.1, [0.0, 0.1, 0.0], Some("Arm")));
        assert_eq!(r.asset().root.translation_alpha, 0.5);
        assert!(!r.overlay, "preview layout is not saved");
    }
}

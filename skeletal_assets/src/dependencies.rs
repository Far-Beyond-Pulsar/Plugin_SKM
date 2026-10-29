//! Typed dependency edges between animation assets, and the checks over them.
//!
//! Each asset kind has an extractor that reads the file with its real codec and
//! lists the stable-id references it holds ([`extract`]). The project file
//! index records these edges (`asset_deps`) so "Used By" is a query, never a
//! stored back-reference. [`check_closure`] walks the edges from one asset
//! through an [`AssetSource`] and reports, for every edge, a missing or
//! ambiguous target, a target of the wrong kind, an incompatible skeleton, and
//! any dependency cycle.
//!
//! Editor-only hints (`preview_mesh`) are recorded as [`EdgeRole::Hint`]: they
//! show up in Used By but never take part in cycle or compatibility checks.

use std::collections::{HashMap, HashSet};

use pulsar_reflection::{
    AnimationGraphAssetKind, AnimationMontageAssetKind, AnimationSequenceAssetKind, AssetKind,
    BlendSpaceAssetKind, IkRetargeterAssetKind, IkRigAssetKind, PoseDatabaseAssetKind,
    RetargetProfileAssetKind, SkeletalMeshAssetKind, SkeletonAssetKind,
};

use crate::anim_graph::{validate::asset_references, AnimationGraphAsset};
use crate::assets::{AnimSeqAsset, SkeletonAsset};
use crate::blend_space::BlendSpaceAsset;
use crate::codec::CodecError;
use crate::diagnostics::Diagnostic;
use crate::ik_retarget::IkRetargetAsset;
use crate::ik_rig::IkRigAsset;
use crate::montage::AnimationMontageAsset;
use crate::pose_db::{PoseDbAsset, PoseSource};
use crate::retarget::RetargetProfileAsset;
use crate::skmesh::SkeletalMeshAsset;

/// Every asset kind in the animation family, by its `AssetKind::KIND`.
pub const KINDS: [&str; 10] = [
    SkeletonAssetKind::KIND,
    SkeletalMeshAssetKind::KIND,
    AnimationSequenceAssetKind::KIND,
    BlendSpaceAssetKind::KIND,
    AnimationMontageAssetKind::KIND,
    AnimationGraphAssetKind::KIND,
    RetargetProfileAssetKind::KIND,
    IkRigAssetKind::KIND,
    IkRetargeterAssetKind::KIND,
    PoseDatabaseAssetKind::KIND,
];

/// The file extension (no dot) that stores `kind`.
pub fn extension_of(kind: &str) -> Option<&'static str> {
    Some(match kind {
        k if k == SkeletonAssetKind::KIND => SkeletonAssetKind::EXTENSIONS[0],
        k if k == SkeletalMeshAssetKind::KIND => SkeletalMeshAssetKind::EXTENSIONS[0],
        k if k == AnimationSequenceAssetKind::KIND => AnimationSequenceAssetKind::EXTENSIONS[0],
        k if k == BlendSpaceAssetKind::KIND => BlendSpaceAssetKind::EXTENSIONS[0],
        k if k == AnimationMontageAssetKind::KIND => AnimationMontageAssetKind::EXTENSIONS[0],
        k if k == AnimationGraphAssetKind::KIND => AnimationGraphAssetKind::EXTENSIONS[0],
        k if k == RetargetProfileAssetKind::KIND => RetargetProfileAssetKind::EXTENSIONS[0],
        k if k == IkRigAssetKind::KIND => IkRigAssetKind::EXTENSIONS[0],
        k if k == IkRetargeterAssetKind::KIND => IkRetargeterAssetKind::EXTENSIONS[0],
        k if k == PoseDatabaseAssetKind::KIND => PoseDatabaseAssetKind::EXTENSIONS[0],
        _ => return None,
    })
}

/// The kind stored by a file with extension `extension`, if it is one of ours.
pub fn kind_of_extension(extension: &str) -> Option<&'static str> {
    KINDS
        .into_iter()
        .find(|k| extension_of(k).is_some_and(|e| e.eq_ignore_ascii_case(extension)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EdgeRole {
    /// Needed to load or evaluate the owner.
    Hard,
    /// An editor convenience; never a dependency for checks.
    Hint,
}

impl EdgeRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hard => "hard",
            Self::Hint => "hint",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "hard" => Some(Self::Hard),
            "hint" => Some(Self::Hint),
            _ => None,
        }
    }
}

/// How the skeleton of the target must relate to the owner's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SkeletonRule {
    /// The target's primary skeleton must equal the owner's.
    SameAsOwner,
    /// The edge is not about skeletons (e.g. a reference to a skeleton itself,
    /// or an IK retargeter's rigs, which differ by design).
    Unconstrained,
    /// The target's primary skeleton must equal the owner's *source* skeleton
    /// (retarget profile rigs).
    OwnerSource,
    /// The target's primary skeleton must equal the owner's *target* skeleton.
    OwnerTarget,
}

impl SkeletonRule {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SameAsOwner => "owner",
            Self::Unconstrained => "none",
            Self::OwnerSource => "source",
            Self::OwnerTarget => "target",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "owner" => Some(Self::SameAsOwner),
            "none" => Some(Self::Unconstrained),
            "source" => Some(Self::OwnerSource),
            "target" => Some(Self::OwnerTarget),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetEdge {
    pub target_kind: &'static str,
    pub target_id: String,
    pub role: EdgeRole,
    /// The authored field holding the reference, e.g. `samples/<id>/sequence`.
    pub via: String,
    pub skeleton: SkeletonRule,
}

/// What an extractor learned from one asset file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extracted {
    pub kind: &'static str,
    pub id: String,
    /// The skeleton this asset's animation is *for* (see [`SkeletonRule`]).
    /// `None` when unset, or when it depends on another asset (IK retargeter).
    pub primary_skeleton: Option<String>,
    pub edges: Vec<AssetEdge>,
}

fn edge(target_kind: &'static str, target_id: &str, via: impl Into<String>, skeleton: SkeletonRule) -> AssetEdge {
    AssetEdge { target_kind, target_id: target_id.to_string(), role: EdgeRole::Hard, via: via.into(), skeleton }
}

fn codec(e: CodecError) -> String {
    e.to_string()
}

/// Read `bytes` as an asset of `kind` and list its references. Errors if the
/// file cannot be read with its codec: an unreadable asset has no known edges.
pub fn extract(kind: &str, bytes: &[u8]) -> Result<Extracted, String> {
    use SkeletonRule::{OwnerSource, OwnerTarget, SameAsOwner, Unconstrained};
    let text = || std::str::from_utf8(bytes).map_err(|e| e.to_string());
    let mut edges = Vec::new();
    match kind {
        k if k == SkeletonAssetKind::KIND => {
            let a = SkeletonAsset::parse(text()?)?;
            if let Some(mesh) = &a.preview_mesh {
                edges.push(AssetEdge { role: EdgeRole::Hint, ..edge(SkeletalMeshAssetKind::KIND, mesh.as_str(), "preview_mesh", Unconstrained) });
            }
            Ok(Extracted { kind: SkeletonAssetKind::KIND, id: a.id.as_str().to_string(), primary_skeleton: Some(a.id.as_str().to_string()), edges })
        }
        k if k == SkeletalMeshAssetKind::KIND => {
            let a = SkeletalMeshAsset::decode(bytes)?;
            let skeleton = a.skeleton.as_ref().map(|s| s.as_str().to_string());
            if let Some(s) = &skeleton {
                edges.push(edge(SkeletonAssetKind::KIND, s, "skeleton", Unconstrained));
            }
            Ok(Extracted { kind: SkeletalMeshAssetKind::KIND, id: a.id.as_str().to_string(), primary_skeleton: skeleton, edges })
        }
        k if k == AnimationSequenceAssetKind::KIND => {
            let a = AnimSeqAsset::parse(text()?)?;
            let skeleton = a.skeleton.as_ref().map(|s| s.as_str().to_string());
            if let Some(s) = &skeleton {
                edges.push(edge(SkeletonAssetKind::KIND, s, "skeleton", Unconstrained));
            }
            if let Some(mesh) = &a.preview_mesh {
                edges.push(AssetEdge { role: EdgeRole::Hint, ..edge(SkeletalMeshAssetKind::KIND, mesh.as_str(), "preview_mesh", Unconstrained) });
            }
            Ok(Extracted { kind: AnimationSequenceAssetKind::KIND, id: a.id.as_str().to_string(), primary_skeleton: skeleton, edges })
        }
        k if k == BlendSpaceAssetKind::KIND => {
            let a = BlendSpaceAsset::parse(text()?).map_err(codec)?;
            if let Some(s) = &a.skeleton {
                edges.push(edge(SkeletonAssetKind::KIND, s.as_str(), "skeleton", Unconstrained));
            }
            for sample in &a.samples {
                if let Some(seq) = &sample.sequence {
                    edges.push(edge(AnimationSequenceAssetKind::KIND, seq.as_str(), format!("samples/{}/sequence", sample.id), SameAsOwner));
                }
            }
            Ok(Extracted { kind: BlendSpaceAssetKind::KIND, id: a.id.as_str().to_string(), primary_skeleton: a.skeleton.as_ref().map(|s| s.as_str().to_string()), edges })
        }
        k if k == AnimationMontageAssetKind::KIND => {
            let a = AnimationMontageAsset::parse(text()?).map_err(codec)?;
            if let Some(s) = &a.skeleton {
                edges.push(edge(SkeletonAssetKind::KIND, s.as_str(), "skeleton", Unconstrained));
            }
            for track in &a.tracks {
                for segment in &track.segments {
                    if let Some(seq) = &segment.sequence {
                        edges.push(edge(AnimationSequenceAssetKind::KIND, seq.as_str(), format!("tracks/{}/segments/{}/sequence", track.id, segment.id), SameAsOwner));
                    }
                }
            }
            Ok(Extracted { kind: AnimationMontageAssetKind::KIND, id: a.id.as_str().to_string(), primary_skeleton: a.skeleton.as_ref().map(|s| s.as_str().to_string()), edges })
        }
        k if k == AnimationGraphAssetKind::KIND => {
            let a = AnimationGraphAsset::parse(text()?).map_err(codec)?;
            if let Some(s) = &a.skeleton {
                edges.push(edge(SkeletonAssetKind::KIND, s.as_str(), "skeleton", Unconstrained));
            }
            for r in asset_references(&a) {
                // The retarget node's profile maps *onto* this graph's skeleton.
                let rule = if r.kind == RetargetProfileAssetKind::KIND || r.kind == IkRetargeterAssetKind::KIND {
                    OwnerTarget
                } else {
                    SameAsOwner
                };
                edges.push(edge(r.kind, &r.id, format!("graphs/{}/nodes/{}/{}", r.graph, r.node, r.property), rule));
            }
            Ok(Extracted { kind: AnimationGraphAssetKind::KIND, id: a.id.as_str().to_string(), primary_skeleton: a.skeleton.as_ref().map(|s| s.as_str().to_string()), edges })
        }
        k if k == RetargetProfileAssetKind::KIND => {
            let a = RetargetProfileAsset::parse(text()?).map_err(codec)?;
            if let Some(s) = &a.source_skeleton {
                edges.push(edge(SkeletonAssetKind::KIND, s.as_str(), "source_skeleton", Unconstrained));
            }
            if let Some(s) = &a.target_skeleton {
                edges.push(edge(SkeletonAssetKind::KIND, s.as_str(), "target_skeleton", Unconstrained));
            }
            if let Some(r) = &a.source_rig {
                edges.push(edge(IkRigAssetKind::KIND, r.as_str(), "source_rig", OwnerSource));
            }
            if let Some(r) = &a.target_rig {
                edges.push(edge(IkRigAssetKind::KIND, r.as_str(), "target_rig", OwnerTarget));
            }
            // Animation retargeted by a profile ends up on its target skeleton.
            Ok(Extracted { kind: RetargetProfileAssetKind::KIND, id: a.id.as_str().to_string(), primary_skeleton: a.target_skeleton.as_ref().map(|s| s.as_str().to_string()), edges })
        }
        k if k == IkRigAssetKind::KIND => {
            let a = IkRigAsset::parse(text()?).map_err(codec)?;
            if let Some(s) = &a.skeleton {
                edges.push(edge(SkeletonAssetKind::KIND, s.as_str(), "skeleton", Unconstrained));
            }
            Ok(Extracted { kind: IkRigAssetKind::KIND, id: a.id.as_str().to_string(), primary_skeleton: a.skeleton.as_ref().map(|s| s.as_str().to_string()), edges })
        }
        k if k == IkRetargeterAssetKind::KIND => {
            let a = IkRetargetAsset::parse(text()?).map_err(codec)?;
            if let Some(r) = &a.source_rig {
                edges.push(edge(IkRigAssetKind::KIND, r.as_str(), "source_rig", Unconstrained));
            }
            if let Some(r) = &a.target_rig {
                edges.push(edge(IkRigAssetKind::KIND, r.as_str(), "target_rig", Unconstrained));
            }
            Ok(Extracted { kind: IkRetargeterAssetKind::KIND, id: a.id.as_str().to_string(), primary_skeleton: None, edges })
        }
        k if k == PoseDatabaseAssetKind::KIND => {
            let a = PoseDbAsset::parse(text()?).map_err(codec)?;
            if let Some(s) = &a.skeleton {
                edges.push(edge(SkeletonAssetKind::KIND, s.as_str(), "skeleton", Unconstrained));
            }
            for s in &a.sources {
                match &s.source {
                    PoseSource::Sequence { sequence } => edges.push(edge(AnimationSequenceAssetKind::KIND, sequence.as_str(), format!("sources/{}/sequence", s.id), SameAsOwner)),
                    PoseSource::BlendSpace { blend_space } => edges.push(edge(BlendSpaceAssetKind::KIND, blend_space.as_str(), format!("sources/{}/blend_space", s.id), SameAsOwner)),
                }
            }
            Ok(Extracted { kind: PoseDatabaseAssetKind::KIND, id: a.id.as_str().to_string(), primary_skeleton: a.skeleton.as_ref().map(|s| s.as_str().to_string()), edges })
        }
        other => Err(format!("`{other}` is not an animation asset kind")),
    }
}

/// How a target id resolves in the project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    Found(Vec<u8>),
    Missing,
    /// The id resolves to a file whose extension belongs to another kind.
    WrongKind(String),
    /// More than one file claims the id; the owner must repair the copy.
    Ambiguous(Vec<String>),
}

/// Where the checker reads assets from: the project file index in the editor,
/// a map in tests.
pub trait AssetSource {
    fn read(&self, kind: &str, id: &str) -> Lookup;
}

impl AssetSource for HashMap<(String, String), Vec<u8>> {
    fn read(&self, kind: &str, id: &str) -> Lookup {
        self.get(&(kind.to_string(), id.to_string())).cloned().map_or(Lookup::Missing, Lookup::Found)
    }
}

/// Check the hard dependency closure of the asset `(kind, id)`.
///
/// Reports, per edge: `dep.missing`, `dep.ambiguous`, `dep.unreadable`,
/// `dep.wrong_kind` (the id resolves to a file of another kind),
/// `dep.skeleton_mismatch` and `dep.skeleton_unset`; and `dep.cycle` for any
/// loop of hard edges. Each asset is read once.
pub fn check_closure(source: &dyn AssetSource, kind: &str, id: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let mut loaded: HashMap<(String, String), Option<Extracted>> = HashMap::new();
    let mut order: Vec<(String, String)> = vec![(kind.to_string(), id.to_string())];
    let mut queue = vec![(kind.to_string(), id.to_string())];
    let mut reported: HashSet<(String, String)> = HashSet::new();

    while let Some(key) = queue.pop() {
        if loaded.contains_key(&key) {
            continue;
        }
        let extracted = match source.read(&key.0, &key.1) {
            Lookup::Found(bytes) => match extract(&key.0, &bytes) {
                Ok(e) if e.kind == key.0 && e.id == key.1 => Some(e),
                Ok(e) => {
                    out.push(Diagnostic::error("dep.stale_id", format!("{} `{}` resolves to a file whose id is `{}`", key.0, key.1, e.id)).at(format!("{}:{}", key.0, key.1)).repair("Refresh the project asset index"));
                    None
                }
                Err(message) => {
                    out.push(Diagnostic::error("dep.unreadable", format!("{} `{}` cannot be read: {message}", key.0, key.1)).at(format!("{}:{}", key.0, key.1)));
                    None
                }
            },
            Lookup::Missing => None,
            Lookup::WrongKind(path) => {
                out.push(Diagnostic::error("dep.wrong_kind", format!("{} `{}` resolves to `{path}`, which is not a {} file", key.0, key.1, key.0)).at(format!("{}:{}", key.0, key.1)).repair("Pick an asset of the right kind"));
                None
            }
            Lookup::Ambiguous(paths) => {
                if reported.insert(key.clone()) {
                    out.push(Diagnostic::error("dep.ambiguous", format!("{} `{}` is claimed by {} files: {}", key.0, key.1, paths.len(), paths.join(", "))).at(format!("{}:{}", key.0, key.1)).repair("Give the copied asset a new id"));
                }
                None
            }
        };
        if let Some(e) = &extracted {
            for edge in e.edges.iter().filter(|x| x.role == EdgeRole::Hard) {
                let next = (edge.target_kind.to_string(), edge.target_id.clone());
                if !loaded.contains_key(&next) && !queue.contains(&next) {
                    order.push(next.clone());
                    queue.push(next);
                }
            }
        }
        loaded.insert(key, extracted);
    }

    // per-edge checks, in discovery order for stable output
    let mut done: HashSet<(String, String)> = HashSet::new();
    for key in order {
        if !done.insert(key.clone()) {
            continue;
        }
        let Some(Some(owner)) = loaded.get(&key) else { continue };
        for edge in owner.edges.iter().filter(|e| e.role == EdgeRole::Hard) {
            let at = format!("{}:{}/{}", owner.kind, owner.id, edge.via);
            let target_key = (edge.target_kind.to_string(), edge.target_id.clone());
            let Some(target) = loaded.get(&target_key) else { continue };
            let Some(target) = target else {
                // unreadable/wrong-kind/ambiguous were reported when loading; a plain miss is ours
                if matches!(source.read(&target_key.0, &target_key.1), Lookup::Missing) {
                    out.push(
                        Diagnostic::error("dep.missing", format!("{} `{}` does not exist in the project", edge.target_kind, edge.target_id))
                            .at(at)
                            .repair("Pick another asset or restore the missing file"),
                    );
                }
                continue;
            };
            let expected = match edge.skeleton {
                SkeletonRule::Unconstrained => continue,
                SkeletonRule::SameAsOwner | SkeletonRule::OwnerTarget => owner.primary_skeleton.clone(),
                SkeletonRule::OwnerSource => retarget_source_skeleton(source, owner),
            };
            let actual = target_skeleton(source, target, edge.skeleton);
            match (&expected, &actual) {
                (Some(e), Some(a)) if e != a => out.push(
                    Diagnostic::error("dep.skeleton_mismatch", format!("{} `{}` is for skeleton {} but {} `{}` uses {}", edge.target_kind, edge.target_id, short(a), owner.kind, owner.id, short(e)))
                        .at(at)
                        .repair("Pick an asset made for the same skeleton"),
                ),
                (None, Some(_)) | (Some(_), None) => out.push(
                    Diagnostic::warning("dep.skeleton_unset", format!("Cannot compare skeletons: {} has {}, {} has {}", owner.kind, opt(&expected), edge.target_kind, opt(&actual)))
                        .at(at)
                        .repair("Set the skeleton on the asset that has none"),
                ),
                _ => {}
            }
        }
    }

    // cycles over hard edges
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    for ((k, i), e) in &loaded {
        if let Some(e) = e {
            let from = format!("{k}:{i}");
            let tos: Vec<String> = e.edges.iter().filter(|x| x.role == EdgeRole::Hard).map(|x| format!("{}:{}", x.target_kind, x.target_id)).collect();
            graph.insert(from, tos);
        }
    }
    if let Some(cycle) = crate::anim_graph::validate::find_cycle(&graph) {
        out.push(Diagnostic::error("dep.cycle", format!("Assets depend on each other in a loop: {}", cycle.join(" -> "))).at(cycle[0].clone()).repair("Remove one of the references in the loop"));
    }
    out
}

fn short(id: &str) -> &str {
    id.split('-').next().unwrap_or(id)
}

fn opt(id: &Option<String>) -> String {
    id.as_deref().map_or("no skeleton".to_string(), |i| format!("skeleton {}", short(i)))
}

/// An IK retargeter has no skeleton of its own: its *target* rig's skeleton is
/// the one animation ends up on (that is what a graph retarget node needs).
fn target_skeleton(source: &dyn AssetSource, target: &Extracted, rule: SkeletonRule) -> Option<String> {
    if target.primary_skeleton.is_some() {
        return target.primary_skeleton.clone();
    }
    if target.kind == IkRetargeterAssetKind::KIND {
        let rig_edge = target.edges.iter().find(|e| e.via == "target_rig")?;
        let _ = rule;
        if let Lookup::Found(bytes) = source.read(rig_edge.target_kind, &rig_edge.target_id) {
            return extract(rig_edge.target_kind, &bytes).ok().and_then(|e| e.primary_skeleton);
        }
    }
    None
}

fn retarget_source_skeleton(_source: &dyn AssetSource, owner: &Extracted) -> Option<String> {
    owner.edges.iter().find(|e| e.via == "source_skeleton").map(|e| e.target_id.clone())
}

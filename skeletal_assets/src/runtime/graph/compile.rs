//! Lowering a validated `.animgraph` into immutable programs.
//!
//! Compilation is all-or-nothing for the graph: any error leaves no
//! [`CompiledGraph`], only diagnostics naming the graph, node and a repair. The
//! one runtime fallback the catalogue promises (Pose Search playing its
//! `fallback` input when the database cannot be used) is decided here: the
//! database is built (or read from the derived cache) at compile time and a
//! missing one is only accepted when a fallback pose is connected.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use glam::Vec3;
use graphy_blueprint::{GraphDescription, NodeInstance};
use serde_json::Value as Json;

use crate::anim_graph::catalogue::{
    default_properties, node_def, resolve_pins, AnimPinType, GraphKind, NodeDef, ParamType,
};
use crate::anim_graph::validate::validate;
use crate::anim_graph::{AnimSubGraph, AnimationGraphAsset};
use crate::assets::{AnimSeqAsset, SkeletonAsset};
use crate::blend_space::BlendSpaceAsset;
use crate::bone_mask::BoneMaskSpec;
use crate::diagnostics::{has_errors, Diagnostic};
use crate::ik_retarget::IkRetargetAsset;
use crate::ik_rig::IkRigAsset;
use crate::montage::{AnimationMontageAsset, BlendCurve};
use crate::pose_db::{PoseDbAsset, PoseSource};
use crate::retarget::RetargetProfileAsset;

use super::super::blendspace::CompiledBlendSpace;
use super::super::clip::CompiledClip;
use super::super::ik::TwoBoneParams;
use super::super::ikrig::CompiledIkRig;
use super::super::montage::CompiledMontage;
use super::super::pose::RuntimeSkeleton;
use super::super::posesearch::{MirrorTable, PoseIndex, SearchClip};
use super::super::retarget::RetargetPlan;
use super::machine::{Interrupt, Machine, StateDef, TransDef};
use super::*;

/// Where compilation finds assets. Implementations load and cache by the
/// asset's stable id; `None` means the id could not be resolved or read.
pub trait AssetSource {
    fn skeleton(&self, id: &str) -> Option<Arc<SkeletonAsset>>;
    fn sequence(&self, id: &str) -> Option<Arc<AnimSeqAsset>>;
    fn blend_space(&self, id: &str) -> Option<Arc<BlendSpaceAsset>>;
    fn montage(&self, id: &str) -> Option<Arc<AnimationMontageAsset>>;
    fn graph(&self, id: &str) -> Option<Arc<AnimationGraphAsset>>;
    fn ik_rig(&self, id: &str) -> Option<Arc<IkRigAsset>>;
    fn retarget_profile(&self, id: &str) -> Option<Arc<RetargetProfileAsset>>;
    fn ik_retargeter(&self, id: &str) -> Option<Arc<IkRetargetAsset>>;
    fn pose_db(&self, id: &str) -> Option<Arc<PoseDbAsset>>;
    /// A previously stored derived pose index payload for `(database id, key hash)`.
    fn cached_pose_index(&self, _database: &str, _key: &str) -> Option<Vec<u8>> {
        None
    }
    /// Store a freshly built payload. Failure is not an error: the index is rebuilt next time.
    fn store_pose_index(&self, _database: &str, _key: &str, _payload: &[u8]) {}
}

pub struct CompileOutput {
    pub graph: Option<Arc<CompiledGraph>>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Compile `asset` against `source`.
pub fn compile_graph(asset: &AnimationGraphAsset, source: &dyn AssetSource) -> CompileOutput {
    let mut cx = Cx::new(source);
    let graph = cx.compile_asset(asset);
    let mut diagnostics = cx.diags;
    diagnostics.sort_by(|a, b| (b.severity, a.code, &a.location).cmp(&(a.severity, b.code, &b.location)));
    let ok = graph.is_some() && !has_errors(&diagnostics);
    CompileOutput { graph: graph.filter(|_| ok).map(Arc::new), diagnostics }
}

struct Cx<'a> {
    source: &'a dyn AssetSource,
    skeletons: HashMap<String, Arc<RuntimeSkeleton>>,
    clips: HashMap<(String, String), Arc<CompiledClip>>,
    rigs: HashMap<(String, String), Arc<CompiledIkRig>>,
    programs: HashMap<(String, String), Arc<Program>>,
    diags: Vec<Diagnostic>,
    deps: BTreeSet<(String, String)>,
    stack: Vec<String>,
}

struct Scope<'a> {
    asset: &'a AnimationGraphAsset,
    params: &'a HashMap<String, (usize, ParamType)>,
}

fn kind_name(kind: &str) -> String {
    kind.to_string()
}

impl<'a> Cx<'a> {
    fn new(source: &'a dyn AssetSource) -> Self {
        Self {
            source,
            skeletons: HashMap::new(),
            clips: HashMap::new(),
            rigs: HashMap::new(),
            programs: HashMap::new(),
            diags: Vec::new(),
            deps: BTreeSet::new(),
            stack: Vec::new(),
        }
    }

    fn error(&mut self, code: &'static str, message: impl Into<String>, at: impl Into<String>, repair: &str) {
        let mut d = Diagnostic::error(code, message).at(at);
        if !repair.is_empty() {
            d = d.repair(repair);
        }
        self.diags.push(d);
    }

    fn dep(&mut self, kind: &str, id: &str) {
        self.deps.insert((kind_name(kind), id.to_string()));
    }

    fn skeleton(&mut self, id: &str, at: &str) -> Option<Arc<RuntimeSkeleton>> {
        if let Some(s) = self.skeletons.get(id) {
            return Some(s.clone());
        }
        self.dep("skeleton", id);
        let Some(asset) = self.source.skeleton(id) else {
            self.error("graph.skeleton_unavailable", format!("Skeleton {id} could not be loaded"), at, "Restore the skeleton asset or pick another");
            return None;
        };
        match RuntimeSkeleton::new(id, &asset.skeleton()).map(|s| s.with_mask_library(asset.masks.clone())) {
            Ok(s) => {
                let s = Arc::new(s);
                self.skeletons.insert(id.to_string(), s.clone());
                Some(s)
            }
            Err(e) => {
                self.error("graph.skeleton_invalid", format!("Skeleton {id} is unusable: {e}"), at, "Repair the skeleton hierarchy");
                None
            }
        }
    }

    fn clip(&mut self, id: &str, skel: &Arc<RuntimeSkeleton>, at: &str) -> Option<Arc<CompiledClip>> {
        let key = (skel.id.clone(), id.to_string());
        if let Some(c) = self.clips.get(&key) {
            return Some(c.clone());
        }
        self.dep("animseq", id);
        let Some(asset) = self.source.sequence(id) else {
            self.error("graph.sequence_unavailable", format!("Sequence {id} could not be loaded"), at, "Restore the sequence or pick another");
            return None;
        };
        if let Some(s) = &asset.skeleton {
            if s.as_str() != skel.id {
                self.error("graph.skeleton_mismatch", format!("Sequence `{}` is for another skeleton than the one this part of the graph poses", asset.clip.name), at, "Pick a sequence made for the same skeleton");
                return None;
            }
        }
        let clip = Arc::new(CompiledClip::compile_shared(&asset, skel));
        for orphan in &clip.orphan_tracks {
            self.diags.push(Diagnostic::warning("graph.orphan_track", format!("Sequence `{}` animates bone `{orphan}`, which the skeleton does not have; the track is ignored", clip.name)).at(at));
        }
        self.clips.insert(key, clip.clone());
        Some(clip)
    }

    fn rig(&mut self, id: &str, skel: &Arc<RuntimeSkeleton>, at: &str) -> Option<Arc<CompiledIkRig>> {
        let key = (skel.id.clone(), id.to_string());
        if let Some(r) = self.rigs.get(&key) {
            return Some(r.clone());
        }
        self.dep("ik_rig", id);
        let Some(asset) = self.source.ik_rig(id) else {
            self.error("graph.rig_unavailable", format!("IK rig {id} could not be loaded"), at, "Restore the IK rig or pick another");
            return None;
        };
        if asset.skeleton.as_ref().is_some_and(|s| s.as_str() != skel.id) {
            self.error("graph.skeleton_mismatch", format!("IK rig `{}` is for another skeleton", asset.name), at, "Pick a rig made for the graph's skeleton");
            return None;
        }
        let (rig, diags) = CompiledIkRig::compile(&asset, skel);
        for mut d in diags {
            d.location = Some(format!("{at}/{}", d.location.unwrap_or_default()));
            self.diags.push(d);
        }
        let rig = Arc::new(rig);
        self.rigs.insert(key, rig.clone());
        Some(rig)
    }

    fn compile_asset(&mut self, asset: &AnimationGraphAsset) -> Option<CompiledGraph> {
        let id = asset.id.as_str().to_string();
        if self.stack.contains(&id) {
            self.error("graph.linked_cycle", format!("Graph `{}` links itself, directly or through other graphs", asset.name), id.clone(), "Remove the cycle");
            return None;
        }
        self.stack.push(id.clone());
        let result = self.compile_asset_inner(asset, &id);
        self.stack.pop();
        result
    }

    fn compile_asset_inner(&mut self, asset: &AnimationGraphAsset, id: &str) -> Option<CompiledGraph> {
        let found = validate(asset);
        let invalid = has_errors(&found);
        self.diags.extend(found);
        if invalid {
            return None;
        }
        let Some(skeleton_ref) = &asset.skeleton else {
            // validation only requires a skeleton when a node needs one; a graph
            // of pure value nodes cannot pose anything either way
            self.error("graph.skeleton_required", "The graph has no skeleton", id, "Pick the graph's skeleton");
            return None;
        };
        let skeleton = self.skeleton(skeleton_ref.as_str(), id)?;
        let params: HashMap<String, (usize, ParamType)> = asset.parameters.iter().enumerate().map(|(i, p)| (p.name.clone(), (i, p.ty))).collect();
        let defs: Vec<ParamDef> = asset
            .parameters
            .iter()
            .map(|p| ParamDef { name: p.name.clone(), ty: p.ty, default: json_value(p.ty, &p.default) })
            .collect();
        let scope = Scope { asset, params: &params };
        let main = self.lower_program(&scope, "main", GraphKind::Main, &asset.main, &skeleton)?;
        let mut diagnostics: Vec<Diagnostic> = self.diags.iter().filter(|d| d.severity != crate::issue::Severity::Error).cloned().collect();
        diagnostics.sort_by(|a, b| a.code.cmp(b.code));
        Some(CompiledGraph {
            id: id.to_string(),
            name: asset.name.clone(),
            skeleton,
            main,
            params: defs,
            diagnostics,
            dependencies: self.deps.iter().cloned().collect(),
        })
    }

    /// Lower one Graphy document. `skel` is the skeleton this graph's poses
    /// are over (the source skeleton inside a retarget source subtree).
    fn lower_program(
        &mut self,
        scope: &Scope,
        label: &str,
        kind: GraphKind,
        graph: &GraphDescription,
        skel: &Arc<RuntimeSkeleton>,
    ) -> Option<Arc<Program>> {
        let cache_key = (label.to_string(), skel.id.clone());
        if let Some(p) = self.programs.get(&cache_key) {
            return Some(p.clone());
        }
        let errors_before = self.diags.iter().filter(|d| d.severity == crate::issue::Severity::Error).count();
        let mut ids: Vec<&String> = graph.nodes.keys().collect();
        ids.sort();
        let index: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
        let nodes: Vec<&NodeInstance> = ids.iter().map(|id| &graph.nodes[*id]).collect();
        let defs: Vec<&'static NodeDef> = nodes.iter().map(|n| node_def(&n.node_type).expect("validated")).collect();
        let props: Vec<HashMap<String, Json>> = nodes
            .iter()
            .zip(&defs)
            .map(|(n, d)| {
                let mut p = default_properties(d);
                p.extend(n.properties.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), v.clone())));
                p
            })
            .collect();
        let param_types = |name: &str| scope.params.get(name).map(|(_, t)| *t);
        let pins: Vec<Vec<crate::anim_graph::catalogue::PinSpec>> = nodes
            .iter()
            .zip(&defs)
            .zip(&props)
            .map(|((_, d), p)| resolve_pins(d, p, param_types).0)
            .collect();
        // inputs: for each node, the producer of each input pin
        let mut inputs: Vec<Vec<Option<usize>>> = pins
            .iter()
            .map(|p| vec![None; p.len()])
            .collect();
        for c in &graph.connections {
            let (Some(&src), Some(&dst)) = (index.get(c.source_node.as_str()), index.get(c.target_node.as_str())) else { continue };
            if let Some(pin) = pins[dst].iter().position(|p| p.name == c.target_pin) {
                inputs[dst][pin] = Some(src);
            }
        }

        // skeleton domain of each node: poses upstream of a Retarget `source`
        // input are over the profile's source skeleton
        let mut domain: Vec<Arc<RuntimeSkeleton>> = vec![skel.clone(); nodes.len()];
        let mut retarget_plans: HashMap<usize, (Arc<RetargetPlan>, f32)> = HashMap::new();
        for (i, n) in nodes.iter().enumerate() {
            if n.node_type != "anim.retarget" {
                continue;
            }
            let at = format!("{label}/{}", n.id);
            let Some((plan, alpha)) = self.retarget_plan(&props[i], skel, &at) else { continue };
            let source_domain = plan.source.clone();
            let pin = pins[i].iter().position(|p| p.name == "source").expect("catalogue pin");
            let mut stack: Vec<usize> = inputs[i][pin].into_iter().collect();
            let mut seen = HashSet::new();
            while let Some(u) = stack.pop() {
                if !seen.insert(u) {
                    continue;
                }
                domain[u] = source_domain.clone();
                for (p, link) in pins[u].iter().zip(&inputs[u]) {
                    if p.ty.is_pose() {
                        stack.extend(link.iter().copied());
                    }
                }
                // a cached pose read upstream reads its save node's domain too
                if nodes[u].node_type == "anim.use_cached_pose" {
                    if let Some(name) = props[u].get("name").and_then(Json::as_str) {
                        if let Some(save) = (0..nodes.len()).find(|&s| nodes[s].node_type == "anim.save_cached_pose" && props[s].get("name").and_then(Json::as_str) == Some(name)) {
                            stack.push(save);
                        }
                    }
                }
            }
            retarget_plans.insert(i, (plan, alpha));
        }
        // a pose wire may not cross skeletons except into a Retarget source
        for (i, n) in nodes.iter().enumerate() {
            for (p, link) in pins[i].iter().zip(&inputs[i]) {
                let Some(u) = *link else { continue };
                if !p.ty.is_pose() {
                    continue;
                }
                let crossing_is_retarget = n.node_type == "anim.retarget" && p.name == "source";
                if !crossing_is_retarget && domain[u].id != domain[i].id {
                    self.error(
                        "graph.mixed_skeleton",
                        format!("Node `{}` feeds `{}` but they pose different skeletons", nodes[u].id, n.id),
                        format!("{label}/{}", n.id),
                        "Keep each Retarget source subtree separate from nodes that also feed the target pose",
                    );
                }
            }
        }

        // cached pose lookup
        let saves: HashMap<String, usize> = nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.node_type == "anim.save_cached_pose")
            .filter_map(|(i, _)| props[i].get("name").and_then(Json::as_str).map(|s| (s.to_string(), i)))
            .collect();

        let mut cnodes = Vec::with_capacity(nodes.len());
        for (i, n) in nodes.iter().enumerate() {
            let at = format!("{label}/{}", n.id);
            let node_skel = domain[i].clone();
            let kind_lowered = self.lower_node(scope, &at, n, defs[i], &props[i], &pins[i], &saves, retarget_plans.get(&i), &node_skel, label);
            let Some(k) = kind_lowered else {
                // the failure is already a diagnostic; keep a placeholder so indices stay aligned
                cnodes.push(CNode { id: n.id.clone(), kind: NodeKind::RuleResult, inputs: inputs[i].clone(), skeleton: node_skel });
                continue;
            };
            cnodes.push(CNode { id: n.id.clone(), kind: k, inputs: inputs[i].clone(), skeleton: node_skel });
        }
        let errors_after = self.diags.iter().filter(|d| d.severity == crate::issue::Severity::Error).count();
        if errors_after > errors_before {
            return None;
        }
        let terminal = nodes.iter().position(|n| n.node_type == kind.terminal())?;
        let mut program = Program { kind, label: label.to_string(), nodes: cnodes, terminal, primary_player: None };
        program.primary_player = primary_player(&program);
        let program = Arc::new(program);
        self.programs.insert(cache_key, program.clone());
        Some(program)
    }

    fn retarget_plan(&mut self, p: &HashMap<String, Json>, target: &Arc<RuntimeSkeleton>, at: &str) -> Option<(Arc<RetargetPlan>, f32)> {
        let profile = p.get("profile").and_then(Json::as_str).filter(|s| !s.is_empty());
        let retargeter = p.get("ik_retargeter").and_then(Json::as_str).filter(|s| !s.is_empty());
        let alpha = p.get("alpha").and_then(Json::as_f64).unwrap_or(1.0) as f32;
        match (profile, retargeter) {
            (Some(_), Some(_)) | (None, None) => {
                self.error("graph.retarget_source", "A Retarget node needs exactly one of a profile or an IK retargeter", at, "Clear one of them");
                None
            }
            (Some(id), None) => {
                self.dep("anim_retarget", id);
                let Some(asset) = self.source.retarget_profile(id) else {
                    self.error("graph.retarget_unavailable", format!("Retarget profile {id} could not be loaded"), at, "Restore the profile");
                    return None;
                };
                let (Some(source_id), Some(target_id)) = (asset.source_skeleton.as_ref(), asset.target_skeleton.as_ref()) else {
                    self.error("graph.retarget_skeletons", "The profile does not name both skeletons", at, "Set the profile's source and target skeletons");
                    return None;
                };
                if target_id.as_str() != target.id {
                    self.error("graph.skeleton_mismatch", "The profile retargets onto another skeleton than this part of the graph poses", at, "Use a profile whose target skeleton is the graph's");
                    return None;
                }
                let source = self.skeleton(source_id.as_str(), at)?;
                let rigs = match (&asset.source_rig, &asset.target_rig) {
                    (Some(s), Some(t)) => {
                        let (s, t) = (self.rig(s.as_str(), &source, at)?, self.rig(t.as_str(), target, at)?);
                        Some((s, t))
                    }
                    _ => None,
                };
                let plan = RetargetPlan::from_profile(&asset, source, target.clone(), rigs);
                self.report_plan(&plan, at);
                Some((Arc::new(plan), alpha))
            }
            (None, Some(id)) => {
                self.dep("ik_retarget", id);
                let Some(asset) = self.source.ik_retargeter(id) else {
                    self.error("graph.retarget_unavailable", format!("IK retargeter {id} could not be loaded"), at, "Restore the retargeter");
                    return None;
                };
                let (Some(source_rig_id), Some(target_rig_id)) = (asset.source_rig.as_ref(), asset.target_rig.as_ref()) else {
                    self.error("graph.retarget_rigs", "The IK retargeter does not name both rigs", at, "Set its source and target IK rigs");
                    return None;
                };
                let (Some(source_rig_asset), Some(target_rig_asset)) = (self.source.ik_rig(source_rig_id.as_str()), self.source.ik_rig(target_rig_id.as_str())) else {
                    self.error("graph.rig_unavailable", "One of the retargeter's IK rigs could not be loaded", at, "Restore the IK rigs");
                    return None;
                };
                self.dep("ik_rig", source_rig_id.as_str());
                self.dep("ik_rig", target_rig_id.as_str());
                let (Some(source_skel_id), Some(target_skel_id)) = (source_rig_asset.skeleton.as_ref(), target_rig_asset.skeleton.as_ref()) else {
                    self.error("graph.retarget_skeletons", "A retargeter rig has no skeleton", at, "Set the rigs' skeletons");
                    return None;
                };
                if target_skel_id.as_str() != target.id {
                    self.error("graph.skeleton_mismatch", "The IK retargeter's target rig is for another skeleton than this part of the graph poses", at, "Use a retargeter whose target rig is for the graph's skeleton");
                    return None;
                }
                let source = self.skeleton(source_skel_id.as_str(), at)?;
                let (srig, trig) = (self.rig(source_rig_id.as_str(), &source, at)?, self.rig(target_rig_id.as_str(), target, at)?);
                let plan = RetargetPlan::from_ik_retargeter(&asset, source, target.clone(), srig, trig);
                self.report_plan(&plan, at);
                Some((Arc::new(plan), alpha))
            }
        }
    }

    fn report_plan(&mut self, plan: &RetargetPlan, at: &str) {
        for d in &plan.diagnostics {
            let mut d = d.clone();
            d.location = Some(format!("{at}/{}", d.location.unwrap_or_default()));
            self.diags.push(d);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_node(
        &mut self,
        scope: &Scope,
        at: &str,
        node: &NodeInstance,
        def: &'static NodeDef,
        p: &HashMap<String, Json>,
        pins: &[crate::anim_graph::catalogue::PinSpec],
        saves: &HashMap<String, usize>,
        retarget: Option<&(Arc<RetargetPlan>, f32)>,
        skel: &Arc<RuntimeSkeleton>,
        label: &str,
    ) -> Option<NodeKind> {
        let f = |name: &str, default: f32| p.get(name).and_then(Json::as_f64).map_or(default, |v| v as f32);
        let b = |name: &str, default: bool| p.get(name).and_then(Json::as_bool).unwrap_or(default);
        let i = |name: &str, default: i64| p.get(name).and_then(Json::as_i64).unwrap_or(default);
        let s = |name: &str| p.get(name).and_then(Json::as_str).filter(|v| !v.is_empty());
        let curve = |name: &str| -> BlendCurve { p.get(name).and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or(BlendCurve::Linear) };
        let vec3 = |name: &str| -> Vec3 {
            p.get(name)
                .and_then(Json::as_array)
                .map(|a| Vec3::new(a.first().and_then(Json::as_f64).unwrap_or(0.0) as f32, a.get(1).and_then(Json::as_f64).unwrap_or(0.0) as f32, a.get(2).and_then(Json::as_f64).unwrap_or(0.0) as f32))
                .unwrap_or(Vec3::ZERO)
        };
        let sync = || {
            s("sync_group").map(|g| SyncCfg {
                group: g.to_string(),
                role: match s("sync_role") {
                    Some("leader") => SyncRole::Leader,
                    Some("follower") => SyncRole::Follower,
                    _ => SyncRole::Auto,
                },
            })
        };
        let looping = || !matches!(s("looping"), Some("once"));
        let bone = |cx: &mut Self, name: &str| -> Option<usize> {
            let key = s(name)?;
            let r = skel.bone_by_id_or_name(key);
            if r.is_none() {
                cx.error("graph.unknown_bone", format!("Bone `{key}` is not in the skeleton"), at, "Pick a bone of the graph's skeleton");
            }
            r
        };
        let _ = (def, pins, node);
        Some(match node.node_type.as_str() {
            "anim.sequence_player" => {
                let id = s("sequence")?;
                let clip = self.clip(id, skel, at)?;
                NodeKind::SequencePlayer(SeqCfg { clip, rate: f("play_rate", 1.0), start: f("start_position", 0.0), looping: looping(), sync: sync() })
            }
            "anim.blend_space_player" => {
                let id = s("blend_space")?;
                self.dep("blendspace", id);
                let Some(asset) = self.source.blend_space(id) else {
                    self.error("graph.blend_space_unavailable", format!("Blend space {id} could not be loaded"), at, "Restore the blend space");
                    return None;
                };
                if asset.skeleton.as_ref().is_some_and(|sk| sk.as_str() != skel.id) {
                    self.error("graph.skeleton_mismatch", format!("Blend space `{}` is for another skeleton", asset.name), at, "Pick a blend space made for the graph's skeleton");
                    return None;
                }
                let mut clips = Vec::new();
                for sample in &asset.samples {
                    let Some(seq) = &sample.sequence else {
                        self.error("graph.blend_space_sample", format!("Blend space `{}` has a sample without a sequence", asset.name), at, "Assign a sequence to every sample");
                        return None;
                    };
                    clips.push(self.clip(seq.as_str(), skel, at)?);
                }
                let space = match CompiledBlendSpace::compile(&asset, clips) {
                    Ok(sp) => Arc::new(sp),
                    Err(e) => {
                        self.error("graph.blend_space_invalid", e, at, "Add at least one sample");
                        return None;
                    }
                };
                NodeKind::BlendSpacePlayer(SpaceCfg { space, x: f("x", 0.0), y: f("y", 0.0), rate: f("play_rate", 1.0), looping: looping(), sync: sync() })
            }
            "anim.pose_search" => {
                let has_fallback = pins.iter().position(|pin| pin.name == "fallback").is_some();
                let db = s("database").and_then(|id| self.pose_db(id, skel, at));
                if db.is_none() {
                    // the fallback pin is the only legal way to run without a database
                    let connected = node_inputs_connected(scope, node, "fallback");
                    if !(has_fallback && connected) {
                        self.error("graph.pose_search_unavailable", "The pose database could not be used and no fallback pose is connected", at, "Connect the Fallback pose or repair the database");
                        return None;
                    }
                    self.diags.push(Diagnostic::warning("graph.pose_search_fallback", "The pose database could not be used; the Fallback pose will play").at(at));
                }
                NodeKind::PoseSearch(SearchCfg { db, interval: f("search_interval", 0.1), blend_time: f("blend_time", 0.2), continuing_bias: f("continuing_bias", 0.1), debug: b("query_debug", false), speed: f("trajectory_speed", 0.0) })
            }
            "anim.reference_pose" => NodeKind::ReferencePose,
            "anim.blend" => NodeKind::Blend { alpha: f("alpha", 0.5), curve: curve("curve") },
            "anim.blend_multi" => NodeKind::BlendMulti { count: i("count", 3) as usize },
            "anim.blend_by_bool" => NodeKind::BlendByBool { condition: b("condition", false), blend_time: f("blend_time", 0.2), curve: curve("curve") },
            "anim.blend_by_int" => NodeKind::BlendByInt { count: i("count", 3) as usize, selector: i("selector", 0) as i32, blend_time: f("blend_time", 0.2) },
            "anim.make_additive" => NodeKind::MakeAdditive { mesh_space: s("space") == Some("mesh") },
            "anim.apply_additive" => NodeKind::ApplyAdditive { alpha: f("alpha", 1.0) },
            "anim.layered_blend_per_bone" => {
                let layers = i("count", 1) as usize;
                let specs: Vec<BoneMaskSpec> = p
                    .get("masks")
                    .and_then(Json::as_array)
                    .map(|a| a.iter().filter_map(|m| serde_json::from_value(m.clone()).ok()).collect())
                    .unwrap_or_default();
                let sk = skeleton_for_masks(skel);
                let mut masks = Vec::new();
                for l in 0..layers {
                    masks.push(match specs.get(l) {
                        Some(spec) => {
                            match spec.missing_bones_in(&sk, &skel.mask_library) {
                                Ok(missing) => {
                                    for missing in missing {
                                        self.error("graph.mask_bone", format!("Layer {l} mask names bone `{missing}`, which the skeleton does not have"), at, "Pick bones of the graph's skeleton");
                                    }
                                    spec.weights_in(&sk, &skel.mask_library).ok()
                                }
                                Err(e) => {
                                    self.error("graph.mask_profile", format!("Layer {l}: {e}"), at, "Pick a mask the skeleton defines, or author the mask inline");
                                    None
                                }
                            }
                        }
                        None => None,
                    });
                }
                NodeKind::LayeredBlend(LayeredCfg {
                    layers,
                    masks,
                    mesh_space_rotation: b("mesh_space_rotation", false),
                    curves: match s("curve_blend") {
                        Some("layer") => CurveBlendMode::Layer,
                        Some("max") => CurveBlendMode::Max,
                        Some("sum") => CurveBlendMode::Sum,
                        _ => CurveBlendMode::Base,
                    },
                    root_motion_from_layer0: s("root_motion_from") == Some("layer0"),
                })
            }
            "anim.slot" => NodeKind::Slot(SlotCfg { slot: s("slot")?.to_string(), group: s("group").unwrap_or("DefaultGroup").to_string(), always_update_source: b("always_update_source", false) }),
            "anim.save_cached_pose" => NodeKind::SaveCachedPose,
            "anim.use_cached_pose" => NodeKind::UseCachedPose { save: *saves.get(s("name")?)? },
            "anim.get_curve" => NodeKind::GetCurve { curve: s("curve")?.to_string() },
            "anim.modify_curve" => NodeKind::ModifyCurve {
                curve: s("curve")?.to_string(),
                value: f("value", 0.0),
                mode: match s("mode") {
                    Some("add") => CurveEditMode::Add,
                    Some("scale") => CurveEditMode::Scale,
                    _ => CurveEditMode::Set,
                },
            },
            "anim.two_bone_ik" => {
                let end = bone(self, "end_bone")?;
                if super::super::ik::two_bone_limb(skel, end).is_none() {
                    self.error("graph.ik_chain", "Two Bone IK needs an end bone with a parent and a grandparent", at, "Pick a bone at least two levels below the root");
                    return None;
                }
                NodeKind::TwoBoneIk(TwoBoneCfg {
                    end,
                    alpha: f("alpha", 1.0),
                    space: if s("effector_space") == Some("bone") { EffectorSpace::Bone } else { EffectorSpace::Mesh },
                    params: TwoBoneParams { alpha: 1.0, allow_stretch: b("allow_stretch", false), max_stretch: f("max_stretch", 1.25), take_rotation: b("take_rotation", false) },
                })
            }
            "anim.fabrik" => {
                let (tip, root) = (bone(self, "tip_bone")?, bone(self, "root_bone")?);
                let Some(chain) = skel.chain(root, tip).filter(|c| c.len() >= 2) else {
                    self.error("graph.ik_chain", "FABRIK needs a root bone above its tip bone", at, "Pick a root that is an ancestor of the tip");
                    return None;
                };
                NodeKind::Fabrik(FabrikCfg { chain, alpha: f("alpha", 1.0), precision: f("precision", 0.001), max_iterations: i("max_iterations", 10) as u32 })
            }
            "anim.ik_rig" => {
                let rig = self.rig(s("rig")?, skel, at)?;
                let mut goal_inputs = Vec::new();
                for (n, pin) in pins.iter().enumerate() {
                    if let Some(name) = pin.name.strip_prefix("goal_") {
                        match rig.goal_index(name) {
                            Some(g) => goal_inputs.push((g, n)),
                            None => {
                                self.error("graph.ik_goal", format!("The IK rig has no goal named `{name}`"), at, "Remove the goal from the node or add it to the rig");
                                return None;
                            }
                        }
                    }
                }
                NodeKind::IkRig(IkRigCfg { rig, goal_inputs, alpha: f("alpha", 1.0) })
            }
            "anim.retarget" => {
                let (plan, alpha) = retarget?.clone();
                NodeKind::Retarget { plan, alpha }
            }
            "anim.state_machine" => {
                let id = s("machine")?;
                let sub = scope.asset.subgraph(id)?;
                NodeKind::StateMachine(Arc::new(self.lower_machine(scope, sub, skel)?))
            }
            "anim.layer" => {
                let sub = scope.asset.subgraph(s("layer")?)?;
                let program = self.lower_program(scope, &format!("{label}>{}", sub.id), GraphKind::Layer, &sub.graph, skel)?;
                NodeKind::Layer { program }
            }
            "anim.linked_graph" => {
                let id = s("graph")?;
                self.dep("anim_graph", id);
                let Some(linked) = self.source.graph(id) else {
                    self.error("graph.linked_unavailable", format!("Linked graph {id} could not be loaded"), at, "Restore the linked graph");
                    return None;
                };
                if linked.skeleton.as_ref().map(|sk| sk.as_str()) != Some(skel.id.as_str()) {
                    self.error("graph.skeleton_mismatch", "The linked graph is for another skeleton than this part of the graph poses", at, "Link a graph made for the same skeleton");
                    return None;
                }
                // compile the linked asset in isolation (own parameters, own programs)
                let mut inner = Cx::new(self.source);
                inner.stack = self.stack.clone();
                inner.skeletons = self.skeletons.clone();
                inner.clips = self.clips.clone();
                inner.rigs = self.rigs.clone();
                let compiled = inner.compile_asset(&linked);
                self.deps.extend(inner.deps);
                for d in inner.diags {
                    let mut d = d;
                    d.location = Some(format!("{at}>{}", d.location.unwrap_or_default()));
                    self.diags.push(d);
                }
                let graph = Arc::new(compiled?);
                let param_map = graph
                    .params
                    .iter()
                    .enumerate()
                    .filter_map(|(ci, cp)| scope.params.get(&cp.name).filter(|(_, ty)| *ty == cp.ty).map(|(pi, _)| (ci, *pi)))
                    .collect();
                NodeKind::LinkedGraph(LinkedCfg { graph, param_map })
            }
            "anim.output_pose" => NodeKind::OutputPose,
            "anim.layer_input" => NodeKind::LayerInput,
            "anim.layer_output" => NodeKind::LayerOutput,
            "anim.state_result" => NodeKind::StateResult,
            "anim.get_parameter" => NodeKind::GetParameter { index: scope.params.get(s("parameter")?)?.0 },
            "math.constant_float" => NodeKind::ConstFloat(f("value", 0.0)),
            "math.constant_bool" => NodeKind::ConstBool(b("value", false)),
            "math.constant_vector" => NodeKind::ConstVector(vec3("value")),
            "math.make_vector" => NodeKind::MakeVector { x: f("x", 0.0), y: f("y", 0.0), z: f("z", 0.0) },
            "anim.make_transform" => NodeKind::MakeTransform { location: vec3("location"), rotation: vec3("rotation") },
            "anim.get_bone_transform" => NodeKind::GetBoneTransform { bone: bone(self, "bone")?, mesh_space: s("space") != Some("local") },
            "math.float_op" => NodeKind::FloatOp {
                op: match s("op") {
                    Some("sub") => FloatOp::Sub,
                    Some("mul") => FloatOp::Mul,
                    Some("div") => FloatOp::Div,
                    Some("min") => FloatOp::Min,
                    Some("max") => FloatOp::Max,
                    Some("abs_a") => FloatOp::AbsA,
                    Some("pow") => FloatOp::Pow,
                    _ => FloatOp::Add,
                },
                a: f("a", 0.0),
                b: f("b", 0.0),
            },
            "math.clamp" => NodeKind::Clamp { min: f("min", 0.0), max: f("max", 1.0) },
            "math.int_to_float" => NodeKind::IntToFloat,
            "math.compare" => NodeKind::Compare {
                op: match s("op") {
                    Some("ge") => CompareOp::Ge,
                    Some("lt") => CompareOp::Lt,
                    Some("le") => CompareOp::Le,
                    Some("eq") => CompareOp::Eq,
                    Some("ne") => CompareOp::Ne,
                    _ => CompareOp::Gt,
                },
                b: f("b", 0.0),
                tolerance: f("tolerance", 0.0001),
            },
            "logic.and" => NodeKind::And,
            "logic.or" => NodeKind::Or,
            "logic.not" => NodeKind::Not,
            "rule.state_time" => NodeKind::StateTime,
            "rule.time_remaining" => NodeKind::TimeRemaining,
            "rule.state_complete" => NodeKind::StateComplete,
            "rule.result" => NodeKind::RuleResult,
            other => {
                self.error("graph.unknown_node", format!("No evaluator for node type `{other}`"), at, "");
                return None;
            }
        })
    }

    fn pose_db(&mut self, id: &str, skel: &Arc<RuntimeSkeleton>, at: &str) -> Option<Arc<CompiledPoseDb>> {
        self.dep("posedb", id);
        let Some(asset) = self.source.pose_db(id) else {
            self.diags.push(Diagnostic::warning("graph.pose_db_unavailable", format!("Pose database {id} could not be loaded")).at(at));
            return None;
        };
        if asset.skeleton.as_ref().is_some_and(|s| s.as_str() != skel.id) {
            self.diags.push(Diagnostic::warning("graph.skeleton_mismatch", format!("Pose database `{}` is for another skeleton", asset.name)).at(at));
            return None;
        }
        let mirror = MirrorTable::from_names(skel);
        let mut clips: Vec<SearchClip> = Vec::new();
        let mut hashes: Vec<(String, String)> = Vec::new();
        for source in asset.sources.iter().filter(|s| s.enabled) {
            let push = |cx: &mut Self, source_id: String, seq: &str, clips: &mut Vec<SearchClip>, hashes: &mut Vec<(String, String)>| -> Option<()> {
                let seq_asset = cx.source.sequence(seq)?;
                let clip = cx.clip(seq, skel, at)?;
                hashes.push((source_id.clone(), crate::codec::sha256_hex(seq_asset.to_text().as_bytes())));
                for mirrored in if source.mirrored { vec![false, true] } else { vec![false] } {
                    clips.push(SearchClip {
                        source_id: if mirrored { format!("{source_id}~mirrored") } else { source_id.clone() },
                        clip: clip.clone(),
                        mirrored,
                        looping: source.looping,
                        exclusions: source.exclusions.clone(),
                        cost_bias: source.cost_bias,
                    });
                }
                Some(())
            };
            match &source.source {
                PoseSource::Sequence { sequence } => {
                    if push(self, source.id.clone(), sequence.as_str(), &mut clips, &mut hashes).is_none() {
                        self.diags.push(Diagnostic::warning("graph.pose_db_source", format!("Pose database source {} could not be loaded and is not indexed", source.id)).at(at));
                    }
                }
                PoseSource::BlendSpace { blend_space } => {
                    let Some(bs) = self.source.blend_space(blend_space.as_str()) else {
                        self.diags.push(Diagnostic::warning("graph.pose_db_source", format!("Pose database blend space source {} could not be loaded", source.id)).at(at));
                        continue;
                    };
                    self.dep("blendspace", blend_space.as_str());
                    for sample in &bs.samples {
                        if let Some(seq) = &sample.sequence {
                            let _ = push(self, format!("{}/{}", source.id, sample.id), seq.as_str(), &mut clips, &mut hashes);
                        }
                    }
                }
            }
        }
        if clips.is_empty() {
            self.diags.push(Diagnostic::warning("graph.pose_db_empty", format!("Pose database `{}` has no usable source clips", asset.name)).at(at));
            return None;
        }
        let key = asset.index_key(&hashes);
        let mirror_opt = clips.iter().any(|c| c.mirrored).then_some(mirror);
        if let Some(bytes) = self.source.cached_pose_index(id, &key.hash) {
            if let Some(index) = PoseIndex::from_payload(&asset, &bytes) {
                if index.entries.iter().all(|e| (e.clip as usize) < clips.len()) {
                    return Some(Arc::new(CompiledPoseDb { asset: (*asset).clone(), clips, index, mirror: mirror_opt, from_cache: true }));
                }
            }
        }
        match PoseIndex::build(&asset, skel, &clips, mirror_opt.as_ref()) {
            Ok(index) => {
                self.source.store_pose_index(id, &key.hash, &index.to_payload());
                Some(Arc::new(CompiledPoseDb { asset: (*asset).clone(), clips, index, mirror: mirror_opt, from_cache: false }))
            }
            Err(errors) => {
                for mut d in errors {
                    d.severity = crate::issue::Severity::Warning;
                    d.location = Some(format!("{at}/{}", d.location.unwrap_or_default()));
                    self.diags.push(d);
                }
                None
            }
        }
    }

    fn lower_machine(&mut self, scope: &Scope, sub: &AnimSubGraph, skel: &Arc<RuntimeSkeleton>) -> Option<Machine> {
        let g = &sub.graph;
        let label = format!("sm:{}", sub.id);
        let mut state_ids: Vec<&String> = g.nodes.iter().filter(|(_, n)| n.node_type == "sm.state").map(|(k, _)| k).collect();
        state_ids.sort();
        let mut states = Vec::new();
        for id in &state_ids {
            let n = &g.nodes[*id];
            let name = n.properties.get("name").and_then(Json::as_str).unwrap_or("").to_string();
            let pose_sub = n.properties.get("pose_graph").and_then(Json::as_str).and_then(|id| scope.asset.subgraph(id))?;
            let program = self.lower_program(scope, &format!("{label}/{}", pose_sub.id), GraphKind::StatePose, &pose_sub.graph, skel)?;
            states.push(StateDef { name, node_id: (*id).clone(), program, reset_on_entry: n.properties.get("reset_on_entry").and_then(Json::as_bool).unwrap_or(true) });
        }
        let state_of = |node: &str| state_ids.iter().position(|id| id.as_str() == node);
        let default = g
            .connections
            .iter()
            .find(|c| g.nodes.get(&c.source_node).is_some_and(|n| n.node_type == "sm.entry") && c.target_pin == "default")
            .and_then(|c| state_of(&c.target_node))?;
        let mut tids: Vec<&String> = g.nodes.iter().filter(|(_, n)| n.node_type == "sm.transition").map(|(k, _)| k).collect();
        tids.sort();
        let mut transitions = Vec::new();
        for id in tids {
            let n = &g.nodes[id];
            let end = |pin: &str| g.connections.iter().find(|c| c.target_node == *id && c.target_pin == pin).and_then(|c| state_of(&c.source_node));
            let (from, to) = (end("from")?, end("to")?);
            let rule_sub = n.properties.get("rule").and_then(Json::as_str).and_then(|r| scope.asset.subgraph(r))?;
            let rule = self.lower_program(scope, &format!("{label}/{}", rule_sub.id), GraphKind::TransitionRule, &rule_sub.graph, skel)?;
            let prop_f = |k: &str, d: f64| n.properties.get(k).and_then(Json::as_f64).unwrap_or(d) as f32;
            transitions.push(TransDef {
                id: id.clone(),
                from,
                to,
                rule,
                duration: prop_f("duration", 0.2),
                curve: n.properties.get("curve").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or(BlendCurve::EaseInOut),
                priority: n.properties.get("priority").and_then(Json::as_i64).unwrap_or(0) as i32,
                interrupt: match n.properties.get("interrupt").and_then(Json::as_str) {
                    Some("source") => Interrupt::Source,
                    Some("target") => Interrupt::Target,
                    Some("both") => Interrupt::Both,
                    _ => Interrupt::None,
                },
                reentry: n.properties.get("allow_reentry").and_then(Json::as_bool).unwrap_or(false),
            });
        }
        Some(Machine { name: sub.name.clone(), states, transitions, default })
    }
}

fn node_inputs_connected(scope: &Scope, node: &NodeInstance, pin: &str) -> bool {
    scope
        .asset
        .graphs()
        .any(|(_, _, g)| g.connections.iter().any(|c| c.target_node == node.id && c.target_pin == pin && g.nodes.contains_key(&node.id)))
}

fn json_value(ty: ParamType, v: &Json) -> Value {
    match ty {
        ParamType::Float => Value::Float(v.as_f64().unwrap_or(0.0) as f32),
        ParamType::Int => Value::Int(v.as_i64().unwrap_or(0) as i32),
        ParamType::Bool => Value::Bool(v.as_bool().unwrap_or(false)),
        ParamType::Vector => {
            let a = v.as_array().cloned().unwrap_or_default();
            let g = |i: usize| a.get(i).and_then(Json::as_f64).unwrap_or(0.0) as f32;
            Value::Vector(Vec3::new(g(0), g(1), g(2)))
        }
    }
}

fn skeleton_for_masks(skel: &RuntimeSkeleton) -> crate::skeleton::Skeleton {
    crate::skeleton::Skeleton {
        bones: (0..skel.len())
            .map(|i| crate::skeleton::Bone {
                id: skel.ids[i].clone(),
                name: skel.names[i].clone(),
                parent: skel.parents[i].map(|p| skel.ids[p].clone()),
                bind_transform: crate::math::Transform::IDENTITY,
            })
            .collect(),
    }
}

/// Follow the terminal's first pose input down to the player that sets the pace.
fn primary_player(program: &Program) -> Option<usize> {
    let mut at = program.nodes[program.terminal].inputs.first().copied().flatten()?;
    for _ in 0..program.nodes.len() {
        match &program.nodes[at].kind {
            NodeKind::SequencePlayer(_) | NodeKind::BlendSpacePlayer(_) | NodeKind::PoseSearch(_) => return Some(at),
            NodeKind::UseCachedPose { save } => at = *save,
            _ => at = program.nodes[at].inputs.iter().flatten().next().copied()?,
        }
    }
    None
}

#[allow(dead_code)]
fn _types(_: AnimPinType, _: &CompiledMontage) {}

/// Compile the montage `id` for `skeleton`, resolving its segments' sequences
/// through `source`. Problems come back as diagnostics, never a partial montage.
pub fn compile_montage(id: &str, skeleton: &Arc<RuntimeSkeleton>, source: &dyn AssetSource) -> Result<Arc<CompiledMontage>, Vec<Diagnostic>> {
    let mut cx = Cx::new(source);
    let Some(asset) = source.montage(id) else {
        return Err(vec![Diagnostic::error("montage.unavailable", format!("Montage {id} could not be loaded")).at(id).repair("Restore the montage asset")]);
    };
    if asset.skeleton.as_ref().is_some_and(|s| s.as_str() != skeleton.id) {
        return Err(vec![Diagnostic::error("montage.skeleton_mismatch", format!("Montage `{}` is for another skeleton than the component poses", asset.name)).at(id).repair("Play a montage made for the mesh's skeleton")]);
    }
    let mut failed: Vec<String> = Vec::new();
    let mut lookup = |seq: &str| -> Option<Arc<CompiledClip>> {
        let clip = cx.clip(seq, skeleton, id);
        if clip.is_none() {
            failed.push(seq.to_string());
        }
        clip
    };
    let compiled = CompiledMontage::compile(&asset, skeleton, &mut lookup);
    drop(lookup);
    match compiled {
        Ok(m) if !has_errors(&cx.diags) => Ok(Arc::new(m)),
        Ok(_) => Err(cx.diags),
        Err(e) => {
            let mut d = cx.diags;
            d.push(Diagnostic::error("montage.invalid", e).at(id));
            Err(d)
        }
    }
}

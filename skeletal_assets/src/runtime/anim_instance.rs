//! The animation state one component owns: its graph instance, parameters,
//! Montage instances, evaluated pose and previous pose.
//!
//! This is the Unreal `AnimInstance` equivalent. It holds no asset payloads of
//! its own: the compiled graph and every compiled clip are shared (`Arc`) with
//! every other instance that plays them.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Mat4;

use crate::anim_graph::catalogue::{GraphKind, ParamType};
use crate::diagnostics::Diagnostic;

use super::clip::CompiledClip;
use super::graph::instance::{GraphInstance, SyncRegistry};
use super::graph::{
    CNode, CompiledGraph, Env, NodeKind, ParamDef, ParamError, ParamValues, PlaybackSpan, Program, SearchRecord, SeqCfg, Value,
};
use super::montage::{CompiledMontage, MontageEvent, MontageHandle, MontageSet, PlayRefused};
use super::pose::{Curves, Pose, RootMotion, RuntimeSkeleton};

/// How a call to [`AnimInstance::advance`] treats time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TimeStep {
    /// One update of exactly the given delta.
    Variable,
    /// Updates of exactly `step` seconds; the remainder carries to the next call
    /// and at most `max_substeps` run per call (excess time is dropped).
    Fixed { step: f32, max_substeps: u32 },
}

/// What an advance is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateMode {
    /// Gameplay: clocks advance and playback spans and montage events are reported.
    Play,
    /// Preview scrubbing: clocks advance (or not) but nothing is reported.
    Scrub,
}

/// Everything the latest advance produced besides the pose.
#[derive(Clone, Debug, Default)]
pub struct UpdateReport {
    /// Sequence clock advances, for notify consumers (empty when scrubbing).
    pub spans: Vec<PlaybackSpan>,
    pub montage_events: Vec<MontageEvent>,
    pub searches: Vec<SearchRecord>,
    /// State machine and IK debug lines of the latest update.
    pub trace: Vec<String>,
    /// Accumulated root motion of the advance, in the actor's space.
    pub root_motion: RootMotion,
    /// Number of updates the advance ran (fixed stepping can run several or none).
    pub substeps: u32,
}

pub struct AnimInstance {
    skeleton: Arc<RuntimeSkeleton>,
    graph: Option<Arc<CompiledGraph>>,
    root: Option<GraphInstance>,
    params: ParamValues,
    param_defs: Vec<ParamDef>,
    pub montages: MontageSet,
    sync: SyncRegistry,
    next_uid: u64,
    pose: Arc<Pose>,
    previous: Arc<Pose>,
    accumulator: f32,
    pub report: UpdateReport,
}

impl std::fmt::Debug for AnimInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnimInstance").field("graph", &self.graph.as_ref().map(|g| &g.name)).field("bones", &self.skeleton.len()).finish()
    }
}

impl AnimInstance {
    /// An instance with no graph: it holds the skeleton's bind pose.
    pub fn new(skeleton: Arc<RuntimeSkeleton>) -> Self {
        let bind = Arc::new(skeleton.bind_pose());
        Self {
            skeleton,
            graph: None,
            root: None,
            params: ParamValues::default(),
            param_defs: Vec::new(),
            montages: MontageSet::default(),
            sync: SyncRegistry::default(),
            next_uid: 1,
            pose: bind.clone(),
            previous: bind,
            accumulator: 0.0,
            report: UpdateReport::default(),
        }
    }

    pub fn skeleton(&self) -> &Arc<RuntimeSkeleton> {
        &self.skeleton
    }

    pub fn graph(&self) -> Option<&Arc<CompiledGraph>> {
        self.graph.as_ref()
    }

    pub fn pose(&self) -> &Arc<Pose> {
        &self.pose
    }

    pub fn previous_pose(&self) -> &Arc<Pose> {
        &self.previous
    }

    /// Replace the graph (first use, ref change or hot reload). State of nodes
    /// that survive the edit (same node id and kind) is kept, and parameters
    /// keep their values when name and type still match. A graph for another
    /// skeleton is refused.
    pub fn set_graph(&mut self, graph: Arc<CompiledGraph>) -> Result<(), String> {
        if graph.skeleton.id != self.skeleton.id {
            return Err(format!("graph `{}` is for another skeleton than this instance poses", graph.name));
        }
        let old_params: HashMap<String, (ParamType, Value)> =
            self.param_defs.iter().zip(&self.params.values).map(|(d, v)| (d.name.clone(), (d.ty, v.clone()))).collect();
        let mut params = ParamValues::from_defs(&graph.params);
        for (def, value) in graph.params.iter().zip(&mut params.values) {
            if let Some((ty, v)) = old_params.get(&def.name) {
                if *ty == def.ty {
                    *value = v.clone();
                }
            }
        }
        let mut root = GraphInstance::new(graph.main.clone(), &mut self.next_uid);
        if let Some(old) = self.root.take() {
            root.adopt(old);
        }
        self.params = params;
        self.param_defs = graph.params.clone();
        self.root = Some(root);
        self.graph = Some(graph);
        Ok(())
    }

    /// Stop using a graph: the instance returns to the bind pose. Montages are cancelled.
    pub fn clear_graph(&mut self) {
        self.graph = None;
        self.root = None;
        self.params = ParamValues::default();
        self.param_defs.clear();
        self.montages.cancel_all();
        self.sync.clear();
        let bind = Arc::new(self.skeleton.bind_pose());
        self.previous = self.pose.clone();
        self.pose = bind;
    }

    /// Cancel everything before the owner goes away. Each montage reports `Cancelled`.
    pub fn shutdown(&mut self) {
        self.montages.cancel_all();
        self.report.montage_events.extend(self.montages.drain_events());
        self.root = None;
        self.graph = None;
    }

    // ---- parameters ---------------------------------------------------------------

    pub fn parameters(&self) -> impl Iterator<Item = (&str, ParamType, &Value)> {
        self.param_defs.iter().zip(&self.params.values).map(|(d, v)| (d.name.as_str(), d.ty, v))
    }

    pub fn parameter(&self, name: &str) -> Option<&Value> {
        let i = self.param_defs.iter().position(|d| d.name == name)?;
        self.params.values.get(i)
    }

    /// Write a parameter. The type must match the declaration exactly and the
    /// value must be finite. The graph reads it on its next update.
    pub fn set_parameter(&mut self, name: &str, value: Value) -> Result<(), ParamError> {
        let i = self.param_defs.iter().position(|d| d.name == name).ok_or_else(|| ParamError::Unknown(name.to_string()))?;
        let ty = self.param_defs[i].ty;
        let ok = match (&value, ty) {
            (Value::Float(v), ParamType::Float) => v.is_finite(),
            (Value::Int(_), ParamType::Int) | (Value::Bool(_), ParamType::Bool) => true,
            (Value::Vector(v), ParamType::Vector) => v.is_finite(),
            _ => return Err(ParamError::WrongType { name: name.to_string(), expected: ty }),
        };
        if !ok {
            return Err(ParamError::NotFinite(name.to_string()));
        }
        self.params.values[i] = value;
        Ok(())
    }

    pub fn set_float(&mut self, name: &str, v: f32) -> Result<(), ParamError> {
        self.set_parameter(name, Value::Float(v))
    }
    pub fn set_bool(&mut self, name: &str, v: bool) -> Result<(), ParamError> {
        self.set_parameter(name, Value::Bool(v))
    }
    pub fn set_int(&mut self, name: &str, v: i32) -> Result<(), ParamError> {
        self.set_parameter(name, Value::Int(v))
    }
    pub fn set_vector(&mut self, name: &str, v: [f32; 3]) -> Result<(), ParamError> {
        self.set_parameter(name, Value::Vector(glam::Vec3::from_array(v)))
    }

    // ---- montages -----------------------------------------------------------------

    pub fn play_montage(&mut self, montage: Arc<CompiledMontage>, rate: f32, section: Option<&str>) -> Result<MontageHandle, PlayRefused> {
        self.montages.play(montage, rate, section)
    }

    // ---- time ---------------------------------------------------------------------

    /// Advance by `dt` seconds of animation time (the caller applies pause and
    /// rate scaling: a paused component passes 0). Returns the new pose.
    pub fn advance(&mut self, dt: f32, step: TimeStep, mode: UpdateMode) -> Arc<Pose> {
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        self.report = UpdateReport::default();
        let mut deltas: Vec<f32> = Vec::new();
        match step {
            TimeStep::Variable => deltas.push(dt),
            TimeStep::Fixed { step, max_substeps } => {
                let step = step.max(1e-4);
                self.accumulator += dt;
                while self.accumulator + 1e-5 >= step && (deltas.len() as u32) < max_substeps.max(1) {
                    self.accumulator = (self.accumulator - step).max(0.0);
                    deltas.push(step);
                }
                if self.accumulator >= step {
                    // spiral guard: drop what could not be simulated this call
                    self.accumulator %= step;
                }
                if deltas.is_empty() {
                    // no whole step yet: keep the pose, report nothing
                    return self.pose.clone();
                }
            }
        }
        let mut root_motion = RootMotion::IDENTITY;
        for d in deltas {
            self.report.substeps += 1;
            let out = self.update_once(d, mode);
            root_motion = root_motion.then(&out.root_motion);
        }
        self.report.root_motion = root_motion;
        self.pose.clone()
    }

    fn update_once(&mut self, dt: f32, mode: UpdateMode) -> Arc<Pose> {
        let quiet = mode == UpdateMode::Scrub;
        self.montages.update(dt);
        if !quiet {
            self.report.montage_events.extend(self.montages.drain_events());
        } else {
            self.montages.drain_events();
        }
        let mut pose = None;
        if let (Some(root), Some(_graph)) = (self.root.as_mut(), self.graph.as_ref()) {
            self.sync.clear();
            let mut spans = Vec::new();
            let mut searches = Vec::new();
            let mut trace = Vec::new();
            let mut env = Env {
                dt,
                params: &self.params,
                montages: &self.montages,
                sync: &mut self.sync,
                rule: None,
                spans: &mut spans,
                searches: &mut searches,
                trace: &mut trace,
                next_uid: &mut self.next_uid,
                quiet,
            };
            root.update(&mut env, 1.0);
            if !env.sync.is_empty() {
                let groups = env.sync.resolve(dt);
                let mut tmp = Vec::new();
                root.apply_sync(&groups, dt, &mut tmp, quiet);
                env.spans.extend(tmp);
            }
            let out = root.evaluate(&mut env);
            root.machine_trace(env.trace);
            pose = Some(out);
            if !quiet {
                self.report.spans.extend(spans);
                self.report.searches.extend(searches);
            }
            self.report.trace = trace;
        }
        if !quiet {
            let mut montage_spans = Vec::new();
            for inst in self.montages.instances() {
                montage_spans.extend(montage_playback(inst));
            }
            self.report.spans.extend(montage_spans);
        }
        // a pose with a non-finite bone (a degenerate clip or solver input) never
        // reaches the palette: the previous pose is kept and the problem is traced
        let new = match pose {
            Some(p) if !p.is_finite() => {
                self.report.trace.push("non-finite pose rejected; holding the previous pose".into());
                self.pose.clone()
            }
            Some(p) => p,
            None => Arc::new(self.skeleton.bind_pose()),
        };
        self.previous = std::mem::replace(&mut self.pose, new.clone());
        new
    }

    /// Re-evaluate the current state without advancing time or reporting anything
    /// (after a seek, a parameter write or a graph change).
    pub fn refresh(&mut self) -> Arc<Pose> {
        let keep = self.previous.clone();
        let report = std::mem::take(&mut self.report);
        let pose = self.update_once(0.0, UpdateMode::Scrub);
        self.previous = keep;
        self.report = report;
        pose
    }

    /// Skinning palette: mesh-space bone matrices times the skeleton's inverse
    /// bind pose, in skeleton bone order.
    pub fn skin_matrices(&self, pose: &Pose) -> Vec<Mat4> {
        let mesh = self.skeleton.to_mesh_space(&pose.bones);
        let inv = self.skeleton.inverse_bind();
        mesh.iter().zip(&inv).map(|(m, i)| *m * *i).collect()
    }

    pub fn curves(&self) -> &Curves {
        &self.pose.curves
    }

    // ---- the internal single-sequence player -----------------------------------------

    /// Position of the single-sequence player's clock, in unbounded seconds.
    pub fn single_sequence_position(&self) -> Option<f64> {
        let root = self.root.as_ref()?;
        self.graph.as_ref().filter(|g| g.name == SINGLE_SEQUENCE_NAME)?;
        root.sequence_position(0)
    }

    /// Move the single-sequence player's clock without raising anything.
    pub fn seek_single_sequence(&mut self, seconds: f64) -> bool {
        match (self.root.as_mut(), self.graph.as_ref().filter(|g| g.name == SINGLE_SEQUENCE_NAME)) {
            (Some(root), Some(_)) => {
                root.seek_sequence(0, seconds);
                true
            }
            _ => false,
        }
    }
}

/// Spans for the clips a montage instance advanced through in its last update.
fn montage_playback(inst: &super::montage::MontageInstance) -> Vec<PlaybackSpan> {
    use super::graph::SpanOrigin;
    let (a, b) = inst.last_span;
    if a == b {
        return Vec::new();
    }
    let (lo, hi) = (a.min(b), a.max(b));
    let mut out = Vec::new();
    for track in &inst.montage.tracks {
        for seg in &track.segments {
            let (s, e) = (lo.max(seg.start as f64), hi.min((seg.start + seg.length) as f64));
            if e <= s {
                continue;
            }
            let rate = seg.rate as f64;
            let (cs, ce) = ((s - seg.start as f64) * rate + seg.clip_start as f64, (e - seg.start as f64) * rate + seg.clip_start as f64);
            let (from, to) = if b >= a { (cs, ce) } else { (ce, cs) };
            out.push(PlaybackSpan {
                clip: seg.clip.clone(),
                from,
                to,
                looping: seg.loop_count > 1,
                weight: inst.weight,
                origin: SpanOrigin::Montage { montage: inst.montage.id.clone(), segment: seg.id.clone() },
            });
        }
    }
    out
}

pub const SINGLE_SEQUENCE_NAME: &str = "single sequence";

/// The compiled form of "just play this sequence": a Sequence Player feeding an
/// Output Pose. Components migrate a plain animation reference to this so there
/// is one runtime, not a second player.
pub fn single_sequence_graph(skeleton: Arc<RuntimeSkeleton>, clip: Arc<CompiledClip>, looping: bool) -> Arc<CompiledGraph> {
    let nodes = vec![
        CNode {
            id: "player".into(),
            kind: NodeKind::SequencePlayer(SeqCfg { clip: clip.clone(), rate: 1.0, start: 0.0, looping, sync: None }),
            inputs: vec![None, None],
            skeleton: skeleton.clone(),
        },
        CNode { id: "output".into(), kind: NodeKind::OutputPose, inputs: vec![Some(0)], skeleton: skeleton.clone() },
    ];
    let main = Arc::new(Program { kind: GraphKind::Main, label: "main".into(), nodes, terminal: 1, primary_player: Some(0) });
    Arc::new(CompiledGraph {
        id: format!("single:{}", clip.id),
        name: SINGLE_SEQUENCE_NAME.into(),
        skeleton,
        main,
        params: Vec::new(),
        diagnostics: Vec::<Diagnostic>::new(),
        dependencies: vec![("animseq".into(), clip.id.clone())],
    })
}

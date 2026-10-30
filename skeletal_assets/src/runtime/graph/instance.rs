//! Per-instance graph state and the update / evaluate passes.
//!
//! One update runs two passes over a [`GraphInstance`]:
//!
//! 1. **update** decides weights and advances clocks. Only nodes that
//!    contribute (weight above zero) are touched, so inactive branches freeze
//!    and a player that becomes relevant again restarts from its start
//!    position. Sync-grouped players register instead of advancing; the
//!    component resolves the groups once the pass has seen every member.
//! 2. **evaluate** samples poses using the decisions the update stored. Each
//!    node is evaluated at most once per update however many wires read it.
//!
//! Value nodes that read a pose (Get Curve, Get Bone Transform) return the
//! previous update's value during the update pass, because weights have to be
//! known before poses can be sampled; during evaluation they read the pose.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use glam::{EulerRot, Quat, Vec3};

use crate::anim_graph::catalogue::GraphKind;
use crate::montage::BlendCurve;

use super::super::blendspace::SampleWeight;
use super::super::clip::wrap_position;
use super::super::ik::{solve_fabrik, solve_two_bone, Effector, FabrikParams, JointLimits};
use super::super::pose::{apply_additive, blend_weighted, make_additive, BoneTransform, Pose, RootMotion, RuntimeSkeleton};
use super::super::posesearch::{DesiredTrajectory, FrameFeatures};
use super::machine::MachineState;
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    Update,
    Eval,
}

// ---- sync groups -----------------------------------------------------------

#[derive(Clone, Debug)]
pub struct SyncMember {
    pub key: (u64, usize),
    pub group: String,
    pub role: SyncRole,
    pub weight: f32,
    /// Seconds of one cycle at the member's own rate.
    pub duration: f32,
    pub rate: f32,
    /// Current unbounded position in seconds.
    pub position: f64,
    pub looping: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct GroupPhase {
    pub leader: (u64, usize),
    /// Normalised phase the group moves to, in [0, 1].
    pub phase: f64,
    pub leader_position: f64,
}

#[derive(Debug, Default)]
pub struct SyncRegistry {
    members: Vec<SyncMember>,
}

impl SyncRegistry {
    pub fn register(&mut self, member: SyncMember) {
        self.members.push(member);
    }

    pub fn clear(&mut self) {
        self.members.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Choose each group's leader and its new phase after `dt` seconds.
    /// Leader: highest weight among explicit leaders, else among auto members,
    /// else among whatever is left; ties go to the lower key.
    pub fn resolve(&self, dt: f32) -> HashMap<String, GroupPhase> {
        let mut out = HashMap::new();
        let mut groups: Vec<&str> = self.members.iter().map(|m| m.group.as_str()).collect();
        groups.sort_unstable();
        groups.dedup();
        for g in groups {
            let members: Vec<&SyncMember> = self.members.iter().filter(|m| m.group == g && m.weight > 0.0).collect();
            let pick = |role: SyncRole| -> Option<&SyncMember> {
                members.iter().copied().filter(|m| m.role == role).max_by(|a, b| a.weight.total_cmp(&b.weight).then(b.key.cmp(&a.key)))
            };
            let Some(leader) = pick(SyncRole::Leader).or_else(|| pick(SyncRole::Auto)).or_else(|| pick(SyncRole::Follower)) else { continue };
            let mut pos = leader.position + dt as f64 * leader.rate as f64;
            let dur = leader.duration.max(1e-6) as f64;
            let phase = if leader.looping {
                (pos / dur).rem_euclid(1.0)
            } else {
                pos = pos.clamp(0.0, dur);
                pos / dur
            };
            out.insert(g.to_string(), GroupPhase { leader: leader.key, phase, leader_position: pos });
        }
        out
    }
}

// ---- node state --------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct SeqState {
    pub position: f64,
    pub prev: f64,
    pub last_tick: u64,
    pub started: bool,
    pub weight: f32,
    /// Registered with a sync group this update; position comes from the group.
    pub synced: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SpaceState {
    pub x: f32,
    pub y: f32,
    pub started: bool,
    pub last_tick: u64,
    pub clocks: Vec<f64>,
    pub prev_clocks: Vec<f64>,
    /// Shared normalised phase in cycles (unbounded), for phase-synced samples.
    pub phase: f64,
    pub prev_phase: f64,
    pub length: f32,
    pub weights: Vec<SampleWeight>,
    pub synced: bool,
    pub weight: f32,
}

#[derive(Clone, Debug, Default)]
pub struct FadeState {
    pub weights: Vec<f32>,
    pub from: Vec<f32>,
    pub selected: usize,
    pub elapsed: f32,
    pub duration: f32,
    pub curve: Option<BlendCurve>,
    pub init: bool,
    pub last_tick: u64,
}

impl FadeState {
    fn select(&mut self, selected: usize, n: usize, duration: f32, curve: BlendCurve, dt: f32, tick: u64) {
        if !self.init || self.weights.len() != n || self.last_tick + 1 != tick {
            // first use, or relevant again after a gap: no fade from a stale choice
            self.weights = (0..n).map(|i| (i == selected) as u8 as f32).collect();
            self.from = self.weights.clone();
            self.selected = selected;
            self.elapsed = duration;
            self.init = true;
        } else if selected != self.selected {
            self.from = self.weights.clone();
            self.selected = selected;
            self.elapsed = 0.0;
        }
        self.last_tick = tick;
        self.duration = duration;
        self.curve = Some(curve);
        if self.elapsed < self.duration {
            self.elapsed = (self.elapsed + dt).min(self.duration);
        }
        let t = if self.duration <= 0.0 { 1.0 } else { (self.elapsed / self.duration).clamp(0.0, 1.0) };
        let s = curve.evaluate(t);
        for i in 0..n {
            let target = (i == selected) as u8 as f32;
            self.weights[i] = self.from[i] + (target - self.from[i]) * s;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cursor {
    pub clip: u32,
    pub time: f32,
    pub mirrored: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SearchState {
    pub current: Option<Cursor>,
    pub previous: Option<Cursor>,
    pub fade_elapsed: f32,
    pub fade_duration: f32,
    pub since_search: f32,
    pub last_pose: Option<Arc<Pose>>,
    pub prev_pose: Option<Arc<Pose>>,
    pub used_fallback: bool,
    pub history: VecDeque<SearchRecord>,
    pub last_tick: u64,
    pub weight: f32,
    pub prev_time: f32,
}

#[derive(Debug, Default)]
pub struct LinkedState {
    pub inst: Option<GraphInstance>,
    pub params: ParamValues,
}

#[derive(Debug, Default)]
pub enum NodeState {
    #[default]
    None,
    Seq(SeqState),
    Space(Box<SpaceState>),
    Search(Box<SearchState>),
    Fade(FadeState),
    /// Decisions made by the update pass for a stateless node.
    Scalars(Vec<f32>),
    Machine(Box<MachineState>),
    Layer(Box<GraphInstance>),
    Linked(Box<LinkedState>),
}

// ---- instance ---------------------------------------------------------------

#[derive(Debug)]
pub struct GraphInstance {
    program: Arc<Program>,
    states: Vec<NodeState>,
    pub uid: u64,
    tick: u64,
    pass: Pass,
    upd_stamp: Vec<u64>,
    pose_stamp: Vec<u64>,
    pose_memo: Vec<Option<Arc<Pose>>>,
    vu_stamp: Vec<u64>,
    vu_memo: Vec<Option<Value>>,
    ve_stamp: Vec<u64>,
    ve_memo: Vec<Option<Value>>,
    /// Last evaluated value of pose-reading value nodes.
    held: Vec<Option<Value>>,
    pub layer_input: Option<Arc<Pose>>,
}

fn eff_from_euler(location: Vec3, rotation: Vec3) -> Effector {
    Effector {
        translation: location,
        rotation: Quat::from_euler(EulerRot::XYZ, rotation.x.to_radians(), rotation.y.to_radians(), rotation.z.to_radians()).normalize(),
    }
}

impl GraphInstance {
    pub fn new(program: Arc<Program>, next_uid: &mut u64) -> Self {
        let uid = *next_uid;
        *next_uid += 1;
        let n = program.nodes.len();
        let mut states = Vec::with_capacity(n);
        for node in &program.nodes {
            states.push(match &node.kind {
                NodeKind::SequencePlayer(_) => NodeState::Seq(SeqState::default()),
                NodeKind::BlendSpacePlayer(c) => {
                    let k = c.space.samples.len();
                    NodeState::Space(Box::new(SpaceState { clocks: vec![0.0; k], prev_clocks: vec![0.0; k], ..Default::default() }))
                }
                NodeKind::PoseSearch(_) => NodeState::Search(Box::default()),
                NodeKind::BlendByBool { .. } | NodeKind::BlendByInt { .. } => NodeState::Fade(FadeState::default()),
                NodeKind::Blend { .. }
                | NodeKind::BlendMulti { .. }
                | NodeKind::ApplyAdditive { .. }
                | NodeKind::LayeredBlend(_)
                | NodeKind::Slot(_)
                | NodeKind::TwoBoneIk(_)
                | NodeKind::Fabrik(_)
                | NodeKind::IkRig(_)
                | NodeKind::Retarget { .. } => NodeState::Scalars(Vec::new()),
                NodeKind::StateMachine(_) => NodeState::Machine(Box::default()),
                NodeKind::Layer { program } => NodeState::Layer(Box::new(GraphInstance::new(program.clone(), next_uid))),
                NodeKind::LinkedGraph(cfg) => {
                    let inst = GraphInstance::new(cfg.graph.main.clone(), next_uid);
                    NodeState::Linked(Box::new(LinkedState { inst: Some(inst), params: ParamValues::from_defs(&cfg.graph.params) }))
                }
                _ => NodeState::None,
            });
        }
        Self {
            program,
            states,
            uid,
            tick: 0,
            pass: Pass::Update,
            upd_stamp: vec![0; n],
            pose_stamp: vec![0; n],
            pose_memo: vec![None; n],
            vu_stamp: vec![0; n],
            vu_memo: vec![None; n],
            ve_stamp: vec![0; n],
            ve_memo: vec![None; n],
            held: vec![None; n],
            layer_input: None,
        }
    }

    pub fn program(&self) -> &Arc<Program> {
        &self.program
    }

    fn link(&self, n: usize, k: usize) -> Option<usize> {
        self.program.nodes[n].inputs.get(k).copied().flatten()
    }

    // ---- update pass --------------------------------------------------------

    /// Advance this graph by `env.dt` with contribution weight `w`.
    pub fn update(&mut self, env: &mut Env, w: f32) {
        self.tick += 1;
        self.pass = Pass::Update;
        let t = self.program.terminal;
        self.update_node(env, t, w);
    }

    fn update_input(&mut self, env: &mut Env, n: usize, k: usize, w: f32) {
        if let Some(u) = self.link(n, k) {
            self.update_node(env, u, w);
        }
    }

    fn in_f32(&mut self, env: &mut Env, n: usize, k: usize, default: f32) -> f32 {
        match self.link(n, k) {
            Some(u) => self.value(env, u).as_f32(),
            None => default,
        }
    }

    fn in_bool(&mut self, env: &mut Env, n: usize, k: usize, default: bool) -> bool {
        match self.link(n, k) {
            Some(u) => self.value(env, u).as_bool(),
            None => default,
        }
    }

    fn in_i32(&mut self, env: &mut Env, n: usize, k: usize, default: i32) -> i32 {
        match self.link(n, k) {
            Some(u) => self.value(env, u).as_i32(),
            None => default,
        }
    }

    fn update_node(&mut self, env: &mut Env, n: usize, w: f32) {
        let program = self.program.clone();
        let node = &program.nodes[n];
        let contributes = w > 1e-6;
        let always = matches!(node.kind, NodeKind::OutputPose | NodeKind::LayerOutput | NodeKind::StateResult);
        if !contributes && !always {
            return;
        }
        if self.upd_stamp[n] == self.tick {
            return;
        }
        self.upd_stamp[n] = self.tick;
        let dt = env.dt;
        match &node.kind {
            NodeKind::SequencePlayer(cfg) => {
                let rate = self.in_f32(env, n, 0, cfg.rate);
                let start = self.in_f32(env, n, 1, cfg.start).max(0.0);
                let tick = self.tick;
                let uid = self.uid;
                let NodeState::Seq(st) = &mut self.states[n] else { return };
                if !st.started || st.last_tick + 1 != tick {
                    st.position = start as f64;
                    st.prev = st.position;
                    st.started = true;
                }
                st.last_tick = tick;
                st.weight = w;
                st.prev = st.position;
                let dur = cfg.clip.duration;
                if let Some(sync) = cfg.sync.as_ref().filter(|_| rate >= 0.0) {
                    // a negative rate plays the clip backwards and leaves its sync group
                    st.synced = true;
                    env.sync.register(SyncMember { key: (uid, n), group: sync.group.clone(), role: sync.role, weight: w, duration: dur / rate.max(1e-6), rate: rate, position: st.position, looping: cfg.looping });
                } else {
                    st.synced = false;
                    st.position += dt as f64 * rate as f64;
                    if !cfg.looping {
                        st.position = st.position.clamp(0.0, dur as f64);
                    }
                    record_span(env, uid, cfg.clip.clone(), st.prev, st.position, cfg.looping, w, SpanOrigin::SequencePlayer { node: node.id.clone() });
                }
            }
            NodeKind::BlendSpacePlayer(cfg) => {
                let x_in = self.in_f32(env, n, 0, cfg.x);
                let y_in = self.in_f32(env, n, 1, cfg.y);
                let tick = self.tick;
                let uid = self.uid;
                let NodeState::Space(st) = &mut self.states[n] else { return };
                let (tx, ty) = cfg.space.clamp_input(x_in, y_in);
                if !st.started || st.last_tick + 1 != tick {
                    st.x = tx;
                    st.y = ty;
                    st.started = true;
                    st.phase = 0.0;
                    for c in &mut st.clocks {
                        *c = 0.0;
                    }
                } else {
                    st.x = smooth(st.x, tx, dt, cfg.space.x_axis.smoothing_time);
                    st.y = smooth(st.y, ty, dt, cfg.space.y_axis.as_ref().map_or(0.0, |a| a.smoothing_time));
                }
                st.last_tick = tick;
                st.weight = w;
                st.weights = cfg.space.solve(st.x, st.y);
                st.prev_clocks.clone_from(&st.clocks);
                st.prev_phase = st.phase;
                let sp = &cfg.space;
                let rate = cfg.rate;
                let phase_sync = |i: usize| match sp.sync_mode {
                    crate::blend_space::SyncMode::None => false,
                    crate::blend_space::SyncMode::Phase => true,
                    crate::blend_space::SyncMode::Markers => sp.samples[i].sync_group.is_some(),
                };
                // the cycle length of the synced samples: weighted, or the leader's
                let synced: Vec<&SampleWeight> = st.weights.iter().filter(|s| phase_sync(s.sample)).collect();
                let leader = sp.leader.as_ref().and_then(|id| synced.iter().find(|s| &sp.samples[s.sample].id == id));
                let cycle = |i: usize| (sp.samples[i].clip.duration / (sp.samples[i].rate.abs().max(1e-3) * rate.abs().max(1e-3))).max(1e-6);
                st.length = match leader {
                    Some(l) => cycle(l.sample),
                    None if !synced.is_empty() => {
                        let total: f32 = synced.iter().map(|s| s.weight).sum();
                        synced.iter().map(|s| s.weight / total * cycle(s.sample)).sum::<f32>().max(1e-6)
                    }
                    None => 1.0,
                };
                let any_synced = !synced.is_empty();
                // free-running samples advance on their own clocks
                for s in &st.weights {
                    if !phase_sync(s.sample) {
                        st.clocks[s.sample] += dt as f64 * rate as f64 * sp.samples[s.sample].rate as f64;
                    }
                }
                if let (Some(sync), true, true) = (&cfg.sync, any_synced, rate >= 0.0) {
                    st.synced = true;
                    env.sync.register(SyncMember {
                        key: (uid, n),
                        group: sync.group.clone(),
                        role: sync.role,
                        weight: w,
                        duration: st.length,
                        rate: 1.0,
                        position: st.phase * st.length as f64,
                        looping: true,
                    });
                } else {
                    st.synced = false;
                    if any_synced {
                        st.phase += dt as f64 / st.length as f64 * if rate < 0.0 { -1.0 } else { 1.0 };
                    }
                }
                if !st.synced {
                    let weights = st.weights.clone();
                    let (clocks, prev_clocks, phase, prev_phase) = (st.clocks.clone(), st.prev_clocks.clone(), st.phase, st.prev_phase);
                    for s in &weights {
                        let sample = &sp.samples[s.sample];
                        let (from, to) = if phase_sync(s.sample) {
                            ((prev_phase + sample.phase_offset as f64) * sample.clip.duration as f64, (phase + sample.phase_offset as f64) * sample.clip.duration as f64)
                        } else {
                            (prev_clocks[s.sample], clocks[s.sample])
                        };
                        record_span(env, uid, sample.clip.clone(), from, to, sample_loops(cfg, sample), w * s.weight, SpanOrigin::BlendSpaceSample { node: node.id.clone(), sample: sample.id.clone() });
                    }
                }
            }
            NodeKind::PoseSearch(cfg) => self.update_search(env, n, node, cfg, w),
            NodeKind::ReferencePose | NodeKind::LayerInput | NodeKind::ConstFloat(_) | NodeKind::ConstBool(_) => {}
            NodeKind::Blend { alpha, curve } => {
                let a = self.in_f32(env, n, 2, *alpha).clamp(0.0, 1.0);
                let shaped = curve.evaluate(a);
                self.states[n] = NodeState::Scalars(vec![shaped]);
                self.update_input(env, n, 0, w * (1.0 - shaped));
                self.update_input(env, n, 1, w * shaped);
            }
            NodeKind::BlendMulti { count } => {
                let raw: Vec<f32> = (0..*count).map(|i| if self.link(n, 2 * i + 1).is_some() { 0.0 } else { 0.0 }).collect();
                let mut weights = raw;
                for (i, slot) in weights.iter_mut().enumerate() {
                    *slot = self.in_f32(env, n, 2 * i + 1, 0.0).max(0.0);
                }
                let total: f32 = weights.iter().sum();
                if total <= 0.0 {
                    weights.iter_mut().for_each(|x| *x = 0.0);
                    weights[0] = 1.0;
                } else {
                    weights.iter_mut().for_each(|x| *x /= total);
                }
                for (i, wi) in weights.clone().into_iter().enumerate() {
                    self.update_input(env, n, 2 * i, w * wi);
                }
                self.states[n] = NodeState::Scalars(weights);
            }
            NodeKind::BlendByBool { condition, blend_time, curve } => {
                let c = self.in_bool(env, n, 2, *condition);
                let tick = self.tick;
                let NodeState::Fade(f) = &mut self.states[n] else { return };
                f.select(if c { 0 } else { 1 }, 2, *blend_time, *curve, dt, tick);
                let ws = f.weights.clone();
                for (i, wi) in ws.into_iter().enumerate() {
                    self.update_input(env, n, i, w * wi);
                }
            }
            NodeKind::BlendByInt { count, selector, blend_time } => {
                let s = self.in_i32(env, n, 0, *selector).clamp(0, *count as i32 - 1) as usize;
                let tick = self.tick;
                let NodeState::Fade(f) = &mut self.states[n] else { return };
                f.select(s, *count, *blend_time, BlendCurve::EaseInOut, dt, tick);
                let ws = f.weights.clone();
                for (i, wi) in ws.into_iter().enumerate() {
                    self.update_input(env, n, 1 + i, w * wi);
                }
            }
            NodeKind::MakeAdditive { .. } => {
                self.update_input(env, n, 0, w);
                self.update_input(env, n, 1, w);
            }
            NodeKind::ApplyAdditive { alpha } => {
                let a = self.in_f32(env, n, 2, *alpha).clamp(0.0, 1.0);
                self.states[n] = NodeState::Scalars(vec![a]);
                self.update_input(env, n, 0, w);
                self.update_input(env, n, 1, w * a);
            }
            NodeKind::LayeredBlend(cfg) => {
                let mut ws = Vec::with_capacity(cfg.layers);
                for l in 0..cfg.layers {
                    ws.push(self.in_f32(env, n, 2 + 2 * l, 1.0).clamp(0.0, 1.0));
                }
                self.update_input(env, n, 0, w);
                for (l, wl) in ws.clone().into_iter().enumerate() {
                    self.update_input(env, n, 1 + 2 * l, w * wl);
                }
                self.states[n] = NodeState::Scalars(ws);
            }
            NodeKind::Slot(cfg) => {
                let covered = !cfg.always_update_source && env.montages.covers_slot(&cfg.slot, &cfg.group);
                self.states[n] = NodeState::Scalars(vec![covered as u8 as f32]);
                if !covered {
                    self.update_input(env, n, 0, w);
                }
            }
            NodeKind::SaveCachedPose => self.update_input(env, n, 0, w),
            NodeKind::UseCachedPose { save } => self.update_node(env, *save, w),
            NodeKind::ModifyCurve { .. } => {
                self.update_input(env, n, 0, w);
                // the value is read in the eval pass from the same inputs
            }
            NodeKind::TwoBoneIk(cfg) => {
                let a = self.in_f32(env, n, 3, cfg.alpha).clamp(0.0, 1.0);
                self.states[n] = NodeState::Scalars(vec![a]);
                self.update_input(env, n, 0, w);
            }
            NodeKind::Fabrik(cfg) => {
                let a = self.in_f32(env, n, 2, cfg.alpha).clamp(0.0, 1.0);
                self.states[n] = NodeState::Scalars(vec![a]);
                self.update_input(env, n, 0, w);
            }
            NodeKind::IkRig(cfg) => {
                let a = self.in_f32(env, n, 1, cfg.alpha).clamp(0.0, 1.0);
                self.states[n] = NodeState::Scalars(vec![a]);
                self.update_input(env, n, 0, w);
            }
            NodeKind::Retarget { alpha, .. } => {
                self.states[n] = NodeState::Scalars(vec![*alpha]);
                self.update_input(env, n, 0, w);
            }
            NodeKind::StateMachine(machine) => {
                let mut st = match std::mem::take(&mut self.states[n]) {
                    NodeState::Machine(m) => m,
                    _ => Box::default(),
                };
                st.update(machine, env, w, self.tick);
                self.states[n] = NodeState::Machine(st);
            }
            NodeKind::Layer { .. } => {
                self.update_input(env, n, 0, w);
                let mut inst = match std::mem::take(&mut self.states[n]) {
                    NodeState::Layer(i) => i,
                    _ => return,
                };
                inst.update(env, w);
                self.states[n] = NodeState::Layer(inst);
            }
            NodeKind::LinkedGraph(cfg) => {
                self.update_input(env, n, 0, w);
                let mut st = match std::mem::take(&mut self.states[n]) {
                    NodeState::Linked(l) => l,
                    _ => return,
                };
                for &(child, parent) in &cfg.param_map {
                    if let Some(v) = env.params.values.get(parent) {
                        st.params.values[child] = v.clone();
                    }
                }
                if let Some(mut inst) = st.inst.take() {
                    let mut child_env = Env {
                        dt: env.dt,
                        params: &st.params,
                        montages: env.montages,
                        sync: &mut *env.sync,
                        rule: None,
                        spans: &mut *env.spans,
                        searches: &mut *env.searches,
                        state_events: &mut *env.state_events,
                        trace: &mut *env.trace,
                        next_uid: &mut *env.next_uid,
                        quiet: env.quiet,
                        root_motion_in_pose: env.root_motion_in_pose,
                    };
                    inst.update(&mut child_env, w);
                    st.inst = Some(inst);
                }
                self.states[n] = NodeState::Linked(st);
            }
            NodeKind::OutputPose | NodeKind::LayerOutput | NodeKind::StateResult => self.update_input(env, n, 0, w),
            // pure values are pulled by their consumers
            _ => {}
        }
    }

    fn update_search(&mut self, env: &mut Env, n: usize, node: &CNode, cfg: &SearchCfg, w: f32) {
        let dt = env.dt;
        let speed = cfg.speed;
        // trajectory inputs: fallback(0), speed(1), direction(2)
        let speed_in = self.in_f32(env, n, 1, speed).max(0.0);
        let dir_in = self.link(n, 2).map(|u| self.value(env, u).as_vec3());
        let uid = self.uid;
        let tick = self.tick;
        let skeleton = node.skeleton.clone();
        let fallback_linked = self.link(n, 0).is_some();
        let NodeState::Search(st) = &mut self.states[n] else { return };
        if st.last_tick + 1 != tick {
            st.current = None;
            st.previous = None;
            st.since_search = f32::MAX;
        }
        st.last_tick = tick;
        st.weight = w;
        let Some(db) = cfg.db.as_ref().filter(|d| !d.index.entries.is_empty()) else {
            st.used_fallback = true;
            st.current = None;
            if fallback_linked {
                self.update_input(env, n, 0, w);
            }
            return;
        };
        st.used_fallback = false;
        st.since_search += dt;
        let due = st.current.is_none() || st.since_search >= cfg.interval;
        if due {
            st.since_search = 0.0;
            let pose = st.last_pose.clone().map(|p| (*p).clone()).unwrap_or_else(|| skeleton.bind_pose());
            let prev = st.prev_pose.clone();
            let duration = st.current.map_or(0.0, |c| db.clips[c.clip as usize].clip.duration);
            let phase = st.current.map_or(0.0, |c| if duration > 0.0 { c.time / duration } else { 0.0 });
            let frame = FrameFeatures::from_pose(&skeleton, &pose, prev.as_deref(), dt.max(1e-3), phase);
            let direction = dir_in.unwrap_or(Vec3::Z).normalize_or_zero();
            let desired = DesiredTrajectory { velocity: direction * speed_in };
            let query = db.index.query_vector(&db.asset, &skeleton, &frame, &desired);
            let current = st.current.map(|c| (c.clip, c.time));
            let result = db.index.search(&db.clips, &query, current, cfg.continuing_bias, &db.asset.search);
            if cfg.debug || true {
                let keep = db.asset.search.query_history.max(1) as usize;
                st.history.push_back(SearchRecord { node: node.id.clone(), result: result.clone(), used_fallback: result.is_none() });
                while st.history.len() > keep {
                    st.history.pop_front();
                }
            }
            if cfg.debug {
                env.searches.push(SearchRecord { node: node.id.clone(), result: result.clone(), used_fallback: result.is_none() });
            }
            match result {
                Some(r) => {
                    let next = Cursor { clip: r.clip, time: r.clip_time, mirrored: r.mirrored };
                    let same = st.current.is_some_and(|c| c.clip == next.clip && c.mirrored == next.mirrored && (c.time - next.time).abs() < db.asset.sampling.interval.max(1e-3));
                    if !same {
                        st.previous = st.current;
                        st.fade_elapsed = 0.0;
                        st.fade_duration = cfg.blend_time;
                        st.current = Some(next);
                    }
                    st.used_fallback = false;
                }
                None => st.used_fallback = true,
            }
        }
        // advance the playing cursor(s)
        if !st.used_fallback {
            let step = |c: &mut Cursor| {
                let clip = &db.clips[c.clip as usize];
                c.time += dt;
                if clip.looping && clip.clip.duration > 0.0 {
                    c.time %= clip.clip.duration;
                } else {
                    c.time = c.time.min(clip.clip.duration);
                }
            };
            if let Some(c) = st.current.as_mut() {
                st.prev_time = c.time;
                step(c);
            }
            if let Some(c) = st.previous.as_mut() {
                step(c);
            }
            st.fade_elapsed += dt;
            if st.fade_elapsed >= st.fade_duration {
                st.previous = None;
            }
            if let Some(c) = st.current {
                let clip = &db.clips[c.clip as usize];
                let from = st.prev_time as f64;
                let to = if c.time < st.prev_time && clip.looping { c.time as f64 + clip.clip.duration as f64 } else { c.time as f64 };
                record_span(env, uid, clip.clip.clone(), from, to, clip.looping, w, SpanOrigin::PoseSearch { node: node.id.clone() });
            }
        } else if fallback_linked {
            self.update_input(env, n, 0, w);
        }
    }

    // ---- sync ---------------------------------------------------------------------

    /// Apply resolved sync group phases to every member of this graph and the
    /// graphs nested in it.
    pub fn apply_sync(&mut self, groups: &HashMap<String, GroupPhase>, dt: f32, env_spans: &mut Vec<PlaybackSpan>, quiet: bool) {
        let program = self.program.clone();
        let uid = self.uid;
        for (n, node) in program.nodes.iter().enumerate() {
            let state = std::mem::take(&mut self.states[n]);
            let state = match (state, &node.kind) {
                (NodeState::Seq(mut st), NodeKind::SequencePlayer(cfg)) if st.synced => {
                    if let Some(group) = cfg.sync.as_ref().and_then(|s| groups.get(&s.group)) {
                        let dur = cfg.clip.duration as f64;
                        let before = st.position;
                        st.prev = before;
                        st.position = if group.leader == (uid, n) {
                            group.leader_position
                        } else if cfg.looping {
                            let mut p = (before / dur.max(1e-9)).floor() * dur + group.phase * dur;
                            if p < before - 1e-9 {
                                p += dur;
                            }
                            p
                        } else {
                            (group.phase * dur).min(dur)
                        };
                        if !quiet {
                            span_push(env_spans, uid, cfg.clip.clone(), st.prev, st.position, cfg.looping, st.weight, SpanOrigin::SequencePlayer { node: node.id.clone() });
                        }
                    }
                    NodeState::Seq(st)
                }
                (NodeState::Space(mut st), NodeKind::BlendSpacePlayer(cfg)) if st.synced => {
                    if let Some(group) = cfg.sync.as_ref().and_then(|s| groups.get(&s.group)) {
                        let before = st.phase;
                        let mut p = if group.leader == (uid, n) { group.leader_position / st.length.max(1e-6) as f64 } else { before.floor() + group.phase };
                        if p < before - 1e-9 {
                            p += 1.0;
                        }
                        st.prev_phase = before;
                        st.phase = p;
                        if !quiet {
                            for s in st.weights.clone() {
                                let sample = &cfg.space.samples[s.sample];
                                if cfg.space.sync_mode == crate::blend_space::SyncMode::None {
                                    continue;
                                }
                                let d = sample.clip.duration as f64;
                                span_push(env_spans, uid, sample.clip.clone(), (st.prev_phase + sample.phase_offset as f64) * d, (st.phase + sample.phase_offset as f64) * d, sample_loops(cfg, sample), st.weight * s.weight, SpanOrigin::BlendSpaceSample { node: node.id.clone(), sample: sample.id.clone() });
                            }
                        }
                    }
                    NodeState::Space(st)
                }
                (NodeState::Machine(mut m), NodeKind::StateMachine(_)) => {
                    m.apply_sync(groups, dt, env_spans, quiet);
                    NodeState::Machine(m)
                }
                (NodeState::Layer(mut inst), _) => {
                    inst.apply_sync(groups, dt, env_spans, quiet);
                    NodeState::Layer(inst)
                }
                (NodeState::Linked(mut l), _) => {
                    if let Some(i) = l.inst.as_mut() {
                        i.apply_sync(groups, dt, env_spans, quiet);
                    }
                    NodeState::Linked(l)
                }
                (other, _) => other,
            };
            self.states[n] = state;
        }
    }

    // ---- evaluate pass -----------------------------------------------------------

    /// Sample the graph's final pose.
    pub fn evaluate(&mut self, env: &mut Env) -> Arc<Pose> {
        self.pass = Pass::Eval;
        let t = self.program.terminal;
        self.pose(env, t)
    }

    fn reference(&self, n: usize) -> Pose {
        self.program.nodes[n].skeleton.bind_pose()
    }

    fn pose_input(&mut self, env: &mut Env, n: usize, k: usize) -> Arc<Pose> {
        match self.link(n, k) {
            Some(u) => self.pose(env, u),
            None => Arc::new(self.reference(n)),
        }
    }

    fn pose(&mut self, env: &mut Env, n: usize) -> Arc<Pose> {
        if self.pose_stamp[n] == self.tick {
            if let Some(p) = &self.pose_memo[n] {
                return p.clone();
            }
        }
        if self.upd_stamp[n] != self.tick {
            // reached in evaluation without a visit in the update pass: give it a
            // zero-time update so its state is initialised without advancing
            let dt = std::mem::replace(&mut env.dt, 0.0);
            let saved = self.pass;
            self.pass = Pass::Update;
            self.update_node(env, n, 1.0);
            self.pass = saved;
            env.dt = dt;
        }
        let out = Arc::new(self.compute_pose(env, n));
        self.pose_stamp[n] = self.tick;
        self.pose_memo[n] = Some(out.clone());
        out
    }

    fn scalars(&self, n: usize) -> &[f32] {
        match &self.states[n] {
            NodeState::Scalars(v) => v,
            _ => &[],
        }
    }

    fn compute_pose(&mut self, env: &mut Env, n: usize) -> Pose {
        let program = self.program.clone();
        let node = &program.nodes[n];
        let skel = node.skeleton.clone();
        match &node.kind {
            NodeKind::SequencePlayer(cfg) => {
                let NodeState::Seq(st) = &self.states[n] else { return skel.bind_pose() };
                sample_clip(&skel, &cfg.clip, st.prev, st.position, cfg.looping, env.root_motion_in_pose)
            }
            NodeKind::BlendSpacePlayer(cfg) => {
                let NodeState::Space(st) = &self.states[n] else { return skel.bind_pose() };
                let sp = &cfg.space;
                let in_pose = env.root_motion_in_pose;
                let poses: Vec<(Pose, f32)> = st
                    .weights
                    .iter()
                    .map(|s| {
                        let sample = &sp.samples[s.sample];
                        let d = sample.clip.duration as f64;
                        let phase_synced = match sp.sync_mode {
                            crate::blend_space::SyncMode::None => false,
                            crate::blend_space::SyncMode::Phase => true,
                            crate::blend_space::SyncMode::Markers => sample.sync_group.is_some(),
                        };
                        let (from, to) = if phase_synced {
                            ((st.prev_phase + sample.phase_offset as f64) * d, (st.phase + sample.phase_offset as f64) * d)
                        } else {
                            (st.prev_clocks[s.sample] + sample.phase_offset as f64 * d, st.clocks[s.sample] + sample.phase_offset as f64 * d)
                        };
                        (sample_clip(&skel, &sample.clip, from, to, sample_loops(cfg, sample), in_pose), s.weight)
                    })
                    .collect();
                let refs: Vec<(&Pose, f32)> = poses.iter().map(|(p, w)| (p, *w)).collect();
                blend_weighted(&refs).unwrap_or_else(|| skel.bind_pose())
            }
            NodeKind::PoseSearch(cfg) => self.compute_search(env, n, node, cfg),
            NodeKind::ReferencePose => skel.bind_pose(),
            NodeKind::Blend { .. } => {
                let alpha = self.scalars(n).first().copied().unwrap_or(0.0);
                if alpha <= 1e-6 {
                    return (*self.pose_input(env, n, 0)).clone();
                }
                if alpha >= 1.0 - 1e-6 {
                    return (*self.pose_input(env, n, 1)).clone();
                }
                let a = self.pose_input(env, n, 0);
                let b = self.pose_input(env, n, 1);
                a.blend(&b, alpha)
            }
            NodeKind::BlendMulti { count } => {
                let ws = self.scalars(n).to_vec();
                let poses: Vec<(Arc<Pose>, f32)> = (0..*count).filter(|&i| ws.get(i).copied().unwrap_or(0.0) > 0.0).map(|i| (self.pose_input(env, n, 2 * i), ws[i])).collect();
                let refs: Vec<(&Pose, f32)> = poses.iter().map(|(p, w)| (&**p, *w)).collect();
                blend_weighted(&refs).unwrap_or_else(|| skel.bind_pose())
            }
            NodeKind::BlendByBool { .. } | NodeKind::BlendByInt { .. } => {
                let base = if matches!(node.kind, NodeKind::BlendByBool { .. }) { 0 } else { 1 };
                let ws = match &self.states[n] {
                    NodeState::Fade(f) => f.weights.clone(),
                    _ => Vec::new(),
                };
                let poses: Vec<(Arc<Pose>, f32)> = ws.iter().enumerate().filter(|(_, w)| **w > 1e-6).map(|(i, w)| (self.pose_input(env, n, base + i), *w)).collect();
                let refs: Vec<(&Pose, f32)> = poses.iter().map(|(p, w)| (&**p, *w)).collect();
                blend_weighted(&refs).unwrap_or_else(|| skel.bind_pose())
            }
            NodeKind::MakeAdditive { mesh_space } => {
                let source = self.pose_input(env, n, 0);
                let reference = self.pose_input(env, n, 1);
                make_additive(&skel, &source, &reference, *mesh_space)
            }
            NodeKind::ApplyAdditive { .. } => {
                let alpha = self.scalars(n).first().copied().unwrap_or(1.0);
                let base = self.pose_input(env, n, 0);
                if alpha <= 1e-6 {
                    return (*base).clone();
                }
                let additive = self.pose_input(env, n, 1);
                apply_additive(&skel, &base, &additive, alpha)
            }
            NodeKind::LayeredBlend(cfg) => {
                let ws = self.scalars(n).to_vec();
                let mut acc = (*self.pose_input(env, n, 0)).clone();
                let base_rm = acc.root_motion;
                let base_curves = acc.curves.clone();
                for l in 0..cfg.layers {
                    let wl = ws.get(l).copied().unwrap_or(0.0);
                    if wl <= 1e-6 {
                        continue;
                    }
                    let layer = self.pose_input(env, n, 1 + 2 * l);
                    blend_layer(&skel, &mut acc, &layer, cfg.masks.get(l).and_then(Option::as_ref), wl, cfg.mesh_space_rotation);
                    for (name, &v) in &layer.curves {
                        let e = acc.curves.get(name).copied();
                        let out = match cfg.curves {
                            CurveBlendMode::Base => e.unwrap_or(v * wl),
                            CurveBlendMode::Layer => e.unwrap_or(0.0) + (v - e.unwrap_or(0.0)) * wl,
                            CurveBlendMode::Max => e.unwrap_or(0.0).max(v * wl),
                            CurveBlendMode::Sum => e.unwrap_or(0.0) + v * wl,
                        };
                        acc.curves.insert(name.clone(), out);
                    }
                    if cfg.root_motion_from_layer0 && l == 0 {
                        acc.root_motion = base_rm.blend(&layer.root_motion, wl);
                    }
                }
                if !cfg.root_motion_from_layer0 {
                    acc.root_motion = base_rm;
                }
                if cfg.curves == CurveBlendMode::Base {
                    // base-owned curves keep exactly the base's values
                    for (name, v) in base_curves {
                        acc.curves.insert(name, v);
                    }
                }
                acc
            }
            NodeKind::Slot(cfg) => {
                let covered = self.scalars(n).first().copied().unwrap_or(0.0) > 0.5;
                let base = if covered { skel.bind_pose() } else { (*self.pose_input(env, n, 0)).clone() };
                env.montages.apply_to_slot(&skel, &cfg.slot, &cfg.group, base)
            }
            NodeKind::SaveCachedPose => (*self.pose_input(env, n, 0)).clone(),
            NodeKind::UseCachedPose { save } => (*self.pose(env, *save)).clone(),
            NodeKind::ModifyCurve { curve, value, mode } => {
                let mut p = (*self.pose_input(env, n, 0)).clone();
                let v = self.in_f32(env, n, 1, *value);
                let e = p.curves.entry(curve.clone()).or_insert(0.0);
                *e = match mode {
                    CurveEditMode::Set => v,
                    CurveEditMode::Add => *e + v,
                    CurveEditMode::Scale => *e * v,
                };
                p
            }
            NodeKind::TwoBoneIk(cfg) => {
                let mut p = (*self.pose_input(env, n, 0)).clone();
                let alpha = self.scalars(n).first().copied().unwrap_or(1.0);
                let Some(mut eff) = self.link(n, 1).and_then(|u| self.value(env, u).as_effector()) else { return p };
                if cfg.space == EffectorSpace::Bone {
                    let end = skel.bone_mesh_space(&p.bones, cfg.end);
                    let mesh = end * glam::Mat4::from_rotation_translation(eff.rotation, eff.translation);
                    let (_, r, t) = mesh.to_scale_rotation_translation();
                    eff = Effector { translation: t, rotation: r };
                }
                let pole = self.link(n, 2).map(|u| self.value(env, u).as_vec3());
                let mut params = cfg.params;
                params.alpha = alpha;
                solve_two_bone(&skel, &mut p.bones, cfg.end, &eff, pole, &params, &JointLimits::default());
                p
            }
            NodeKind::Fabrik(cfg) => {
                let mut p = (*self.pose_input(env, n, 0)).clone();
                let alpha = self.scalars(n).first().copied().unwrap_or(1.0);
                let Some(eff) = self.link(n, 1).and_then(|u| self.value(env, u).as_effector()) else { return p };
                let params = FabrikParams { alpha, precision: cfg.precision, max_iterations: cfg.max_iterations, take_rotation: false, respect_limits: false };
                solve_fabrik(&skel, &mut p.bones, &cfg.chain, &eff, &params, &JointLimits::default());
                p
            }
            NodeKind::IkRig(cfg) => {
                let mut p = (*self.pose_input(env, n, 0)).clone();
                let alpha = self.scalars(n).first().copied().unwrap_or(1.0);
                let mut goals: Vec<Option<Effector>> = vec![None; cfg.rig.goals.len()];
                for &(goal, input) in &cfg.goal_inputs {
                    goals[goal] = self.link(n, input).and_then(|u| self.value(env, u).as_effector());
                }
                let outcomes = cfg.rig.execute(&skel, &mut p.bones, &goals, alpha);
                if env.trace.len() < 256 {
                    for o in outcomes {
                        env.trace.push(format!("{}: {} {}", node.id, o.solver, o.detail));
                    }
                }
                p
            }
            NodeKind::Retarget { plan, .. } => {
                let alpha = self.scalars(n).first().copied().unwrap_or(1.0);
                let src = self.pose_input(env, n, 0);
                let out = plan.retarget(&src);
                if alpha >= 1.0 - 1e-6 {
                    out
                } else {
                    let mut p = skel.bind_pose().blend(&out, alpha);
                    p.curves = out.curves;
                    p
                }
            }
            NodeKind::StateMachine(machine) => {
                let mut st = match std::mem::take(&mut self.states[n]) {
                    NodeState::Machine(m) => m,
                    _ => return skel.bind_pose(),
                };
                let pose = st.evaluate(machine, env, &skel);
                self.states[n] = NodeState::Machine(st);
                pose
            }
            NodeKind::Layer { .. } => {
                let input = self.link(n, 0).map(|u| self.pose(env, u));
                let mut inst = match std::mem::take(&mut self.states[n]) {
                    NodeState::Layer(i) => i,
                    _ => return skel.bind_pose(),
                };
                inst.layer_input = input;
                let out = inst.evaluate(env);
                self.states[n] = NodeState::Layer(inst);
                (*out).clone()
            }
            NodeKind::LinkedGraph(_) => {
                let input = self.link(n, 0).map(|u| self.pose(env, u));
                let mut st = match std::mem::take(&mut self.states[n]) {
                    NodeState::Linked(l) => l,
                    _ => return skel.bind_pose(),
                };
                let out = match st.inst.take() {
                    Some(mut inst) => {
                        inst.layer_input = input;
                        let mut child_env = Env {
                            dt: env.dt,
                            params: &st.params,
                            montages: env.montages,
                            sync: &mut *env.sync,
                            rule: None,
                            spans: &mut *env.spans,
                            searches: &mut *env.searches,
                            state_events: &mut *env.state_events,
                            trace: &mut *env.trace,
                            next_uid: &mut *env.next_uid,
                            quiet: env.quiet,
                        root_motion_in_pose: env.root_motion_in_pose,
                        };
                        let out = inst.evaluate(&mut child_env);
                        st.inst = Some(inst);
                        (*out).clone()
                    }
                    None => skel.bind_pose(),
                };
                self.states[n] = NodeState::Linked(st);
                out
            }
            NodeKind::OutputPose | NodeKind::LayerOutput | NodeKind::StateResult => (*self.pose_input(env, n, 0)).clone(),
            NodeKind::LayerInput => match &self.layer_input {
                Some(p) if p.bones.len() == skel.len() => (**p).clone(),
                _ => skel.bind_pose(),
            },
            // value nodes are not poses
            _ => skel.bind_pose(),
        }
    }

    fn compute_search(&mut self, env: &mut Env, n: usize, node: &CNode, cfg: &SearchCfg) -> Pose {
        let skel = node.skeleton.clone();
        let (used_fallback, current, previous, fade) = match &self.states[n] {
            NodeState::Search(s) => (s.used_fallback, s.current, s.previous, if s.fade_duration > 0.0 { (s.fade_elapsed / s.fade_duration).clamp(0.0, 1.0) } else { 1.0 }),
            _ => return skel.bind_pose(),
        };
        let pose = match (&cfg.db, current) {
            (Some(db), Some(cur)) if !used_fallback => {
                let in_pose = env.root_motion_in_pose;
                let sample = |c: Cursor| -> Pose {
                    let clip = &db.clips[c.clip as usize].clip;
                    let mut p = clip.sample(&skel, c.time);
                    if clip.root_motion.enabled && !in_pose {
                        clip.remove_root_motion(&skel, &mut p.bones, c.time);
                    }
                    if c.mirrored {
                        if let Some(m) = &db.mirror {
                            p = m.mirror(&skel, &p);
                        }
                    }
                    p
                };
                let now = sample(cur);
                let mut out = match previous {
                    Some(prev) => sample(prev).blend(&now, BlendCurve::EaseInOut.evaluate(fade)),
                    None => now,
                };
                if let NodeState::Search(s) = &self.states[n] {
                    let clip = &db.clips[cur.clip as usize].clip;
                    let from = s.prev_time as f64;
                    let to = if cur.time < s.prev_time && db.clips[cur.clip as usize].looping { cur.time as f64 + clip.duration as f64 } else { cur.time as f64 };
                    let rm = if in_pose { RootMotion::IDENTITY } else { clip.root_motion_between(&skel, from, to, false) };
                    out.root_motion = if cur.mirrored {
                        RootMotion { translation: Vec3::new(-rm.translation.x, rm.translation.y, rm.translation.z), rotation: Quat::from_xyzw(rm.rotation.x, -rm.rotation.y, -rm.rotation.z, rm.rotation.w) }
                    } else {
                        rm
                    };
                }
                out
            }
            _ => (*self.pose_input(env, n, 0)).clone(),
        };
        let arc = Arc::new(pose.clone());
        if let NodeState::Search(s) = &mut self.states[n] {
            s.prev_pose = s.last_pose.take();
            s.last_pose = Some(arc);
        }
        pose
    }

    // ---- values -------------------------------------------------------------------

    fn value(&mut self, env: &mut Env, n: usize) -> Value {
        match self.pass {
            Pass::Update if self.vu_stamp[n] == self.tick => {
                if let Some(v) = &self.vu_memo[n] {
                    return v.clone();
                }
            }
            Pass::Eval if self.ve_stamp[n] == self.tick => {
                if let Some(v) = &self.ve_memo[n] {
                    return v.clone();
                }
            }
            _ => {}
        }
        let v = self.compute_value(env, n);
        match self.pass {
            Pass::Update => {
                self.vu_stamp[n] = self.tick;
                self.vu_memo[n] = Some(v.clone());
            }
            Pass::Eval => {
                self.ve_stamp[n] = self.tick;
                self.ve_memo[n] = Some(v.clone());
            }
        }
        v
    }

    fn compute_value(&mut self, env: &mut Env, n: usize) -> Value {
        let program = self.program.clone();
        let node = &program.nodes[n];
        match &node.kind {
            NodeKind::GetParameter { index } => env.params.values.get(*index).cloned().unwrap_or(Value::Float(0.0)),
            NodeKind::ConstFloat(v) => Value::Float(*v),
            NodeKind::ConstBool(v) => Value::Bool(*v),
            NodeKind::ConstVector(v) => Value::Vector(*v),
            NodeKind::MakeVector { x, y, z } => Value::Vector(Vec3::new(self.in_f32(env, n, 0, *x), self.in_f32(env, n, 1, *y), self.in_f32(env, n, 2, *z))),
            NodeKind::MakeTransform { location, rotation } => {
                let l = match self.link(n, 0) {
                    Some(u) => self.value(env, u).as_vec3(),
                    None => *location,
                };
                let r = match self.link(n, 1) {
                    Some(u) => self.value(env, u).as_vec3(),
                    None => *rotation,
                };
                Value::Transform(eff_from_euler(l, r))
            }
            NodeKind::GetBoneTransform { bone, mesh_space } => {
                let skel = node.skeleton.clone();
                let compute = |pose: &Pose| -> Value {
                    if *mesh_space {
                        let (_, r, t) = skel.bone_mesh_space(&pose.bones, *bone).to_scale_rotation_translation();
                        Value::Transform(Effector { translation: t, rotation: r })
                    } else {
                        Value::Transform(Effector { translation: pose.bones[*bone].translation, rotation: pose.bones[*bone].rotation })
                    }
                };
                match self.pass {
                    Pass::Eval => {
                        let pose = self.pose_input(env, n, 0);
                        let v = compute(&pose);
                        self.held[n] = Some(v.clone());
                        v
                    }
                    Pass::Update => self.held[n].clone().unwrap_or_else(|| compute(&skel.bind_pose())),
                }
            }
            NodeKind::GetCurve { curve } => match self.pass {
                Pass::Eval => {
                    let pose = self.pose_input(env, n, 0);
                    let v = Value::Float(pose.curves.get(curve).copied().unwrap_or(0.0));
                    self.held[n] = Some(v.clone());
                    v
                }
                Pass::Update => self.held[n].clone().unwrap_or(Value::Float(0.0)),
            },
            NodeKind::FloatOp { op, a, b } => {
                let (x, y) = (self.in_f32(env, n, 0, *a), self.in_f32(env, n, 1, *b));
                Value::Float(match op {
                    FloatOp::Add => x + y,
                    FloatOp::Sub => x - y,
                    FloatOp::Mul => x * y,
                    FloatOp::Div => {
                        if y == 0.0 { 0.0 } else { x / y }
                    }
                    FloatOp::Min => x.min(y),
                    FloatOp::Max => x.max(y),
                    FloatOp::AbsA => x.abs(),
                    FloatOp::Pow => {
                        if x < 0.0 { 0.0 } else { x.powf(y) }
                    }
                })
            }
            NodeKind::Clamp { min, max } => Value::Float(self.in_f32(env, n, 0, 0.0).clamp(*min, min.max(*max))),
            NodeKind::IntToFloat => Value::Float(self.in_i32(env, n, 0, 0) as f32),
            NodeKind::Compare { op, b, tolerance } => {
                let (x, y) = (self.in_f32(env, n, 0, 0.0), self.in_f32(env, n, 1, *b));
                Value::Bool(match op {
                    CompareOp::Gt => x > y,
                    CompareOp::Ge => x >= y,
                    CompareOp::Lt => x < y,
                    CompareOp::Le => x <= y,
                    CompareOp::Eq => (x - y).abs() <= *tolerance,
                    CompareOp::Ne => (x - y).abs() > *tolerance,
                })
            }
            NodeKind::And => {
                let (a, b) = (self.in_bool(env, n, 0, false), self.in_bool(env, n, 1, false));
                Value::Bool(a && b)
            }
            NodeKind::Or => {
                let (a, b) = (self.in_bool(env, n, 0, false), self.in_bool(env, n, 1, false));
                Value::Bool(a || b)
            }
            NodeKind::Not => Value::Bool(!self.in_bool(env, n, 0, false)),
            NodeKind::StateTime => Value::Float(env.rule.map_or(0.0, |r| r.state_time)),
            NodeKind::TimeRemaining => Value::Float(env.rule.map_or(0.0, |r| r.time_remaining)),
            NodeKind::StateComplete => Value::Bool(env.rule.is_some_and(|r| r.complete)),
            NodeKind::RuleResult => Value::Bool(self.in_bool(env, n, 0, false)),
            _ => Value::Float(0.0),
        }
    }

    /// Evaluate a rule graph's result with a state context.
    pub fn rule(&mut self, env: &mut Env) -> bool {
        self.tick += 1;
        self.pass = Pass::Update;
        let t = self.program.terminal;
        self.value(env, t).as_bool()
    }

    /// `(seconds remaining, complete)` of the primary player.
    pub fn primary_remaining(&self) -> (f32, bool) {
        let Some(n) = self.program.primary_player else { return (f32::INFINITY, false) };
        match (&self.program.nodes[n].kind, &self.states[n]) {
            (NodeKind::SequencePlayer(cfg), NodeState::Seq(st)) => {
                if cfg.looping {
                    (f32::INFINITY, false)
                } else {
                    let remaining = (cfg.clip.duration as f64 - st.position).max(0.0) as f32;
                    (remaining / cfg.rate.max(1e-3), st.position >= cfg.clip.duration as f64 - 1e-6)
                }
            }
            (NodeKind::BlendSpacePlayer(cfg), NodeState::Space(st)) => {
                if cfg.looping {
                    (f32::INFINITY, false)
                } else {
                    let done = st.weights.iter().all(|w| {
                        let s = &cfg.space.samples[w.sample];
                        st.clocks[w.sample] >= s.clip.duration as f64 - 1e-6
                    });
                    (0.0, done)
                }
            }
            _ => (f32::INFINITY, false),
        }
    }

    /// Carry state over from a previous instance of an edited program: nodes
    /// with the same id and kind keep their clocks and fades.
    pub fn adopt(&mut self, old: GraphInstance) {
        self.tick = old.tick;
        let mut old_states: HashMap<String, (NodeState, NodeKind)> = HashMap::new();
        for (node, state) in old.program.nodes.iter().zip(old.states) {
            old_states.insert(node.id.clone(), (state, node.kind.clone()));
        }
        for (node, state) in self.program.nodes.iter().zip(self.states.iter_mut()) {
            let Some((prev, prev_kind)) = old_states.remove(&node.id) else { continue };
            match (&node.kind, state, prev) {
                (NodeKind::SequencePlayer(c), NodeState::Seq(new), NodeState::Seq(old)) => {
                    *new = old;
                    new.position = new.position.min(if c.looping { f64::MAX } else { c.clip.duration as f64 });
                }
                (NodeKind::BlendByBool { .. }, NodeState::Fade(new), NodeState::Fade(old)) if old.weights.len() == 2 => *new = old,
                (NodeKind::BlendByInt { count, .. }, NodeState::Fade(new), NodeState::Fade(old)) if old.weights.len() == *count => *new = old,
                (NodeKind::StateMachine(m), NodeState::Machine(new), NodeState::Machine(old)) => {
                    if let NodeKind::StateMachine(old_def) = &prev_kind {
                        new.adopt(m, old_def, *old)
                    }
                }
                (NodeKind::Layer { .. }, NodeState::Layer(new), NodeState::Layer(old)) => new.adopt(*old),
                _ => {}
            }
        }
    }

    pub fn kind(&self) -> GraphKind {
        self.program.kind
    }

    /// Debug names and weights of nested state machines, depth first.
    pub fn machine_trace(&self, out: &mut Vec<String>) {
        for (node, state) in self.program.nodes.iter().zip(&self.states) {
            match (state, &node.kind) {
                (NodeState::Machine(m), NodeKind::StateMachine(def)) => m.trace(def, out),
                (NodeState::Layer(i), _) => i.machine_trace(out),
                (NodeState::Linked(l), _) => {
                    if let Some(i) = &l.inst {
                        i.machine_trace(out)
                    }
                }
                _ => {}
            }
        }
    }

    /// Latest Pose Search query records of this graph and nested graphs.
    pub fn search_history(&self, out: &mut Vec<SearchRecord>) {
        for state in &self.states {
            match state {
                NodeState::Search(s) => out.extend(s.history.iter().cloned()),
                NodeState::Layer(i) => i.search_history(out),
                _ => {}
            }
        }
    }
}

// ---- helpers ----------------------------------------------------------------------

fn smooth(current: f32, target: f32, dt: f32, time: f32) -> f32 {
    if time <= 1e-6 {
        return target;
    }
    let k = 1.0 - (-3.0 * dt / time).exp();
    current + (target - current) * k
}

fn sample_loops(cfg: &SpaceCfg, sample: &super::super::blendspace::CompiledSample) -> bool {
    match sample.looping {
        crate::blend_space::SampleLoop::Loop => true,
        crate::blend_space::SampleLoop::Once => false,
        crate::blend_space::SampleLoop::Inherit => cfg.looping,
    }
}

fn record_span(env: &mut Env, scope: u64, clip: Arc<CompiledClip>, from: f64, to: f64, looping: bool, weight: f32, origin: SpanOrigin) {
    if env.quiet {
        return;
    }
    span_push(env.spans, scope, clip, from, to, looping, weight, origin);
}

/// An empty span (`from == to`) is kept: it says the playback is still live.
fn span_push(spans: &mut Vec<PlaybackSpan>, scope: u64, clip: Arc<CompiledClip>, from: f64, to: f64, looping: bool, weight: f32, origin: SpanOrigin) {
    spans.push(PlaybackSpan { scope, clip, from, to, looping, weight, origin });
}

/// A clip sampled at an unbounded position, with its root motion for the move
/// from `prev` to `position` and that motion removed from the pose.
fn sample_clip(skel: &RuntimeSkeleton, clip: &CompiledClip, prev: f64, position: f64, looping: bool, in_pose: bool) -> Pose {
    let (t, _) = wrap_position(position, clip.duration, looping);
    let mut pose = clip.sample(skel, t);
    if clip.root_motion.enabled && !in_pose {
        pose.root_motion = clip.root_motion_between(skel, prev, position, looping);
        clip.remove_root_motion(skel, &mut pose.bones, t);
    }
    pose
}

/// Blend `layer` over `acc` per bone: weight = mask weight * `w`.
fn blend_layer(skel: &RuntimeSkeleton, acc: &mut Pose, layer: &Pose, mask: Option<&Vec<f32>>, w: f32, mesh_space_rotation: bool) {
    let weight = |i: usize| (mask.map_or(1.0, |m| m.get(i).copied().unwrap_or(0.0)) * w).clamp(0.0, 1.0);
    if !mesh_space_rotation {
        for i in 0..acc.bones.len() {
            let bw = weight(i);
            if bw > 0.0 {
                acc.bones[i] = acc.bones[i].blend(&layer.bones[i], bw);
            }
        }
        return;
    }
    // rotations blend in mesh space so a layer does not inherit parent motion
    let acc_mesh = skel.to_mesh_space(&acc.bones);
    let layer_mesh = skel.to_mesh_space(&layer.bones);
    let mesh_rot = |m: &glam::Mat4| m.to_scale_rotation_translation().1;
    let mut new_rot: Vec<Quat> = Vec::with_capacity(acc.bones.len());
    for i in 0..acc.bones.len() {
        new_rot.push(super::super::pose::slerp_short(mesh_rot(&acc_mesh[i]), mesh_rot(&layer_mesh[i]), weight(i)));
    }
    let mut parent_rot_new = vec![Quat::IDENTITY; acc.bones.len()];
    for &i in &skel.order {
        let parent_rot = skel.parents[i].map_or(Quat::IDENTITY, |p| parent_rot_new[p]);
        let bw = weight(i);
        let mut b = acc.bones[i];
        b.translation = b.translation.lerp(layer.bones[i].translation, bw);
        b.scale = b.scale.lerp(layer.bones[i].scale, bw);
        b.rotation = (parent_rot.inverse() * new_rot[i]).normalize();
        parent_rot_new[i] = new_rot[i];
        acc.bones[i] = b;
    }
}

#[allow(dead_code)]
fn _unused(_: BoneTransform) {}

impl GraphInstance {
    /// Position of a Sequence Player's clock, in unbounded clip seconds.
    pub fn sequence_position(&self, n: usize) -> Option<f64> {
        match self.states.get(n)? {
            NodeState::Seq(st) => Some(st.position),
            _ => None,
        }
    }

    /// Move a Sequence Player's clock. Nothing is reported for the skipped time.
    pub fn seek_sequence(&mut self, n: usize, position: f64) {
        let tick = self.tick;
        if let Some(NodeState::Seq(st)) = self.states.get_mut(n) {
            st.position = position.max(0.0);
            st.prev = st.position;
            st.started = true;
            st.last_tick = tick;
        }
    }
}

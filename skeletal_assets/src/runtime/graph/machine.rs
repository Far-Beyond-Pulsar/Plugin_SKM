//! State machines: definition (immutable, shared) and per-instance runtime.
//!
//! A machine is always in one state, its *current* state. A transition whose
//! rule is true starts a timed blend from the weights the machine has right now
//! to the target state, so an interrupting transition blends from a mixture
//! rather than snapping. Every state that carries weight keeps a live graph
//! instance (its own clocks); a state with no weight is not updated.

use std::collections::HashMap;
use std::sync::Arc;

use crate::montage::BlendCurve;

use super::super::pose::{blend_weighted, Pose, RuntimeSkeleton};
use super::instance::{GraphInstance, GroupPhase};
use super::{Env, PlaybackSpan, Program, RuleContext, StateEvent, StateEventKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupt {
    None,
    Source,
    Target,
    Both,
}

#[derive(Clone, Debug)]
pub struct StateDef {
    pub name: String,
    pub node_id: String,
    pub program: Arc<Program>,
    pub reset_on_entry: bool,
}

#[derive(Clone, Debug)]
pub struct TransDef {
    pub id: String,
    pub from: usize,
    pub to: usize,
    pub rule: Arc<Program>,
    pub duration: f32,
    pub curve: BlendCurve,
    pub priority: i32,
    pub interrupt: Interrupt,
    pub reentry: bool,
}

#[derive(Clone, Debug)]
pub struct Machine {
    pub name: String,
    pub states: Vec<StateDef>,
    pub transitions: Vec<TransDef>,
    pub default: usize,
}

#[derive(Debug)]
struct StateInst {
    state: usize,
    graph: GraphInstance,
    /// Seconds since this instance was entered.
    time: f32,
}

#[derive(Debug)]
struct ActiveBlend {
    sources: Vec<(usize, f32)>,
    target: usize,
    elapsed: f32,
    duration: f32,
    curve: BlendCurve,
    interrupt: Interrupt,
    to_state: usize,
    trans: usize,
}

#[derive(Debug, Default)]
pub struct MachineState {
    insts: Vec<Option<StateInst>>,
    current: Option<usize>,
    blend: Option<ActiveBlend>,
    dormant: HashMap<usize, StateInst>,
    rules: Vec<Option<GraphInstance>>,
    last_tick: u64,
    started: bool,
    weights: Vec<(usize, f32)>,
    /// Name of the transition that fired most recently, for traces and tests.
    pub last_transition: Option<String>,
    start_override: Option<usize>,
}

fn emit(env: &mut Env, machine: &Machine, kind: StateEventKind, state: usize, from: usize, to: usize, transition: &str) {
    if env.quiet {
        return;
    }
    env.state_events.push(StateEvent {
        machine: machine.name.clone(),
        kind,
        state: machine.states[state].name.clone(),
        from_state: machine.states[from].name.clone(),
        to_state: machine.states[to].name.clone(),
        transition: transition.to_string(),
    });
}

impl MachineState {
    fn alloc(&mut self, inst: StateInst) -> usize {
        if let Some(i) = self.insts.iter().position(Option::is_none) {
            self.insts[i] = Some(inst);
            i
        } else {
            self.insts.push(Some(inst));
            self.insts.len() - 1
        }
    }

    fn new_inst(&mut self, machine: &Machine, state: usize, env: &mut Env) -> StateInst {
        if !machine.states[state].reset_on_entry {
            if let Some(mut d) = self.dormant.remove(&state) {
                d.time = 0.0;
                return d;
            }
        }
        StateInst { state, graph: GraphInstance::new(machine.states[state].program.clone(), env.next_uid), time: 0.0 }
    }

    fn reset(&mut self, machine: &Machine, env: &mut Env) {
        self.insts.clear();
        self.dormant.clear();
        self.blend = None;
        self.weights.clear();
        let adopted = self.start_override;
        let start = self.start_override.take().unwrap_or(machine.default);
        if adopted.is_none() {
            emit(env, machine, StateEventKind::Entered, start, start, start, "");
        }
        let inst = self.new_inst(machine, start, env);
        self.current = Some(self.alloc(inst));
        self.started = true;
    }

    /// The weight of every live instance right now.
    fn current_weights(&self) -> Vec<(usize, f32)> {
        let Some(current) = self.current else { return Vec::new() };
        let Some(b) = &self.blend else { return vec![(current, 1.0)] };
        let p = if b.duration <= 0.0 { 1.0 } else { (b.elapsed / b.duration).clamp(0.0, 1.0) };
        let s = b.curve.evaluate(p);
        let mut acc: Vec<(usize, f32)> = Vec::new();
        let mut add = |slot: usize, w: f32| {
            if let Some(e) = acc.iter_mut().find(|(s, _)| *s == slot) {
                e.1 += w;
            } else {
                acc.push((slot, w));
            }
        };
        for &(slot, w) in &b.sources {
            add(slot, w * (1.0 - s));
        }
        add(b.target, s);
        acc.retain(|(_, w)| *w > 1e-6);
        acc.sort_by_key(|(slot, _)| *slot);
        acc
    }

    fn finish_blend(&mut self, machine: &Machine, env: &mut Env) {
        let Some(b) = self.blend.take() else { return };
        let from = machine.transitions[b.trans].from;
        let id = machine.transitions[b.trans].id.clone();
        let mut exited: Vec<usize> = Vec::new();
        for slot in 0..self.insts.len() {
            if slot == b.target {
                continue;
            }
            if let Some(inst) = self.insts[slot].take() {
                if !exited.contains(&inst.state) {
                    exited.push(inst.state);
                }
                if !machine.states[inst.state].reset_on_entry {
                    self.dormant.insert(inst.state, inst);
                }
            }
        }
        for state in exited {
            emit(env, machine, StateEventKind::Exited, state, from, b.to_state, &id);
        }
        emit(env, machine, StateEventKind::TransitionCompleted, from, from, b.to_state, &id);
        self.current = Some(b.target);
    }

    pub fn update(&mut self, machine: &Machine, env: &mut Env, w: f32, tick: u64) {
        if !self.started || self.last_tick + 1 != tick {
            self.reset(machine, env);
        }
        self.last_tick = tick;
        if self.rules.len() != machine.transitions.len() {
            self.rules = machine.transitions.iter().map(|_| None).collect();
        }
        let dt = env.dt;
        // 1. advance the blend in flight
        if let Some(b) = &mut self.blend {
            b.elapsed += dt;
        }
        if self.blend.as_ref().is_some_and(|b| b.elapsed >= b.duration) {
            self.finish_blend(machine, env);
        }
        // 2. which transitions may fire
        let live = self.current_weights();
        let (allowed_from, current_state): (Vec<usize>, usize) = {
            let cur_slot = self.current.expect("started");
            let cur_state = self.insts[cur_slot].as_ref().map_or(machine.default, |i| i.state);
            match &self.blend {
                None => (vec![cur_state], cur_state),
                Some(b) => {
                    let mut from = Vec::new();
                    if matches!(b.interrupt, Interrupt::Source | Interrupt::Both) {
                        for &(slot, _) in &b.sources {
                            if let Some(i) = &self.insts[slot] {
                                from.push(i.state);
                            }
                        }
                    }
                    if matches!(b.interrupt, Interrupt::Target | Interrupt::Both) {
                        from.push(b.to_state);
                    }
                    (from, cur_state)
                }
            }
        };
        let _ = current_state;
        let mut candidates: Vec<usize> = (0..machine.transitions.len()).filter(|&t| allowed_from.contains(&machine.transitions[t].from)).collect();
        candidates.sort_by(|&a, &b| {
            let (ta, tb) = (&machine.transitions[a], &machine.transitions[b]);
            ta.priority.cmp(&tb.priority).then(ta.id.cmp(&tb.id))
        });
        // 3. first true rule fires
        let mut fire = None;
        for t in candidates {
            let def = &machine.transitions[t];
            // the instance of the rule's source state with the most weight
            let slot = live
                .iter()
                .filter(|(slot, _)| self.insts[*slot].as_ref().is_some_and(|i| i.state == def.from))
                .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
                .map(|(slot, _)| *slot);
            let Some(slot) = slot else { continue };
            let (time, remaining, complete) = {
                let inst = self.insts[slot].as_ref().expect("live");
                let (remaining, complete) = inst.graph.primary_remaining();
                (inst.time, remaining, complete)
            };
            if self.rules[t].is_none() {
                self.rules[t] = Some(GraphInstance::new(def.rule.clone(), env.next_uid));
            }
            let saved = env.rule.replace(RuleContext { state_time: time, time_remaining: remaining, complete });
            let fired = self.rules[t].as_mut().expect("created").rule(env);
            env.rule = saved;
            if fired {
                fire = Some(t);
                break;
            }
        }
        if let Some(t) = fire {
            self.start_transition(machine, t, env);
        }
        // 4. update every instance that carries weight
        self.weights = self.current_weights();
        for (slot, weight) in self.weights.clone() {
            if let Some(inst) = self.insts[slot].as_mut() {
                inst.time += dt;
                inst.graph.update(env, w * weight);
            }
        }
    }

    fn start_transition(&mut self, machine: &Machine, t: usize, env: &mut Env) {
        let def = &machine.transitions[t];
        let snapshot = self.current_weights();
        let reuse = if def.reentry && def.from == def.to {
            None
        } else {
            snapshot.iter().map(|(s, _)| *s).find(|s| self.insts[*s].as_ref().is_some_and(|i| i.state == def.to))
        };
        let target = match reuse {
            Some(slot) => slot,
            None => {
                let inst = self.new_inst(machine, def.to, env);
                self.alloc(inst)
            }
        };
        self.last_transition = Some(def.id.clone());
        emit(env, machine, StateEventKind::TransitionStarted, def.from, def.from, def.to, &def.id);
        emit(env, machine, StateEventKind::Entered, def.to, def.from, def.to, &def.id);
        if def.duration <= 0.0 {
            // immediate: everything but the target is dropped
            self.blend = Some(ActiveBlend { sources: Vec::new(), target, elapsed: 0.0, duration: 0.0, curve: def.curve, interrupt: def.interrupt, to_state: def.to, trans: t });
            self.current = Some(target);
            self.finish_blend(machine, env);
            return;
        }
        // snapshot sources: when the target is already fading out it keeps that weight as a source
        self.blend = Some(ActiveBlend { sources: snapshot, target, elapsed: 0.0, duration: def.duration, curve: def.curve, interrupt: def.interrupt, to_state: def.to, trans: t });
        self.current = Some(target);
    }

    pub fn evaluate(&mut self, machine: &Machine, env: &mut Env, skel: &Arc<RuntimeSkeleton>) -> Pose {
        let mut poses: Vec<(Arc<Pose>, f32)> = Vec::new();
        for (slot, weight) in self.weights.clone() {
            if let Some(inst) = self.insts[slot].as_mut() {
                poses.push((inst.graph.evaluate(env), weight));
            }
        }
        let refs: Vec<(&Pose, f32)> = poses.iter().map(|(p, w)| (&**p, *w)).collect();
        let _ = machine;
        blend_weighted(&refs).unwrap_or_else(|| skel.bind_pose())
    }

    pub fn apply_sync(&mut self, groups: &HashMap<String, GroupPhase>, dt: f32, spans: &mut Vec<PlaybackSpan>, quiet: bool) {
        for inst in self.insts.iter_mut().flatten() {
            inst.graph.apply_sync(groups, dt, spans, quiet);
        }
    }

    /// Resume in the state the previous instance was in, by name, when the edited
    /// machine still has it; otherwise start at the default state. A blend in flight
    /// is not carried: the edit may have rewired the states it mixed.
    pub fn adopt(&mut self, machine: &Machine, old_machine: &Machine, old: MachineState) {
        if let Some(name) = old.current_state(old_machine) {
            self.start_override = machine.states.iter().position(|s| s.name == name);
        }
    }

    pub fn trace(&self, machine: &Machine, out: &mut Vec<String>) {
        let parts: Vec<String> = self
            .weights
            .iter()
            .filter_map(|(slot, w)| self.insts[*slot].as_ref().map(|i| format!("{} {:.2}", machine.states[i.state].name, w)))
            .collect();
        let arrow = self.blend.as_ref().map(|b| format!(" via {}", machine.transitions[b.trans].id)).unwrap_or_default();
        out.push(format!("{}: {}{}", machine.name, parts.join(", "), arrow));
        for inst in self.insts.iter().flatten() {
            inst.graph.machine_trace(out);
        }
    }

    /// Name of the state the machine is in (the latest target).
    pub fn current_state<'a>(&self, machine: &'a Machine) -> Option<&'a str> {
        let slot = self.current?;
        self.insts[slot].as_ref().map(|i| machine.states[i.state].name.as_str())
    }

    /// Whether a transition blend is in flight.
    pub fn blending(&self) -> bool {
        self.blend.is_some()
    }
}

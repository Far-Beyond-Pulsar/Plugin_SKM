//! Headless tests for the graph compiler and evaluator, one section per node family.

use std::sync::Arc;

use glam::Vec3;
use serde_json::{json, Value as Json};

use crate::anim_graph::builder::GraphBuilder;
use crate::anim_graph::catalogue::{GraphKind, ParamType};
use crate::anim_graph::{AnimSubGraph, AnimationGraphAsset, GraphParameter};
use crate::animation::{BoneTrack, Keyframe};
use crate::assets::{AnimSeqAsset, SkeletonAsset};
use crate::blend_space::{BlendAxis, BlendSample, BlendSpaceAsset, SyncMode};
use crate::issue::Severity;
use crate::math::{Transform, Vec3 as AV3};
use crate::runtime::anim_instance::{AnimInstance, TimeStep, UpdateMode};
use crate::runtime::clip::tests::{clip_asset, translate_track};
use crate::runtime::graph::compile::{compile_graph, CompileOutput};
use crate::runtime::graph::{CompiledGraph, Value};
use crate::runtime::memory::MemorySource;
use crate::runtime::pose::tests::chain_skeleton;
use crate::runtime::pose::RuntimeSkeleton;

pub(crate) struct Fixture {
    pub src: MemorySource,
    pub skeleton_id: String,
    pub skel: Arc<RuntimeSkeleton>,
}

fn js(v: &str) -> Json {
    Json::from(v)
}

impl Fixture {
    pub fn new() -> Self {
        let skeleton = chain_skeleton(3);
        let asset = SkeletonAsset::new(skeleton);
        let id = asset.id.as_str().to_string();
        let skel = Arc::new(RuntimeSkeleton::new(id.clone(), &asset.skeleton()).unwrap());
        let mut src = MemorySource::default();
        src.skeletons.insert(id.clone(), Arc::new(asset));
        Self { src, skeleton_id: id, skel }
    }

    /// Register a sequence of `duration` seconds on the fixture skeleton.
    pub fn seq(&mut self, name: &str, duration: f32, tracks: Vec<BoneTrack>) -> String {
        let mut asset = clip_asset(name, duration, tracks);
        asset.skeleton = Some(crate::ids::SkeletonId::parse(&self.skeleton_id).unwrap());
        let id = asset.id.as_str().to_string();
        self.src.sequences.insert(id.clone(), Arc::new(asset));
        id
    }

    /// A sequence moving `b1` along x from `from` to `to` over `duration`.
    pub fn slide(&mut self, name: &str, duration: f32, from: f32, to: f32) -> String {
        self.seq(name, duration, vec![translate_track("b1", [from, 1.0, 0.0], [to, 1.0, 0.0], duration)])
    }

    pub fn asset(&self, params: &[(&str, ParamType)]) -> AnimationGraphAsset {
        let mut asset = AnimationGraphAsset::default();
        asset.skeleton = Some(crate::ids::SkeletonId::parse(&self.skeleton_id).unwrap().to_ref());
        for (name, ty) in params {
            asset.parameters.push(GraphParameter::new(*name, *ty));
        }
        asset
    }

    /// A main graph built by `build` (the Output Pose node is called `out`).
    pub fn graph(&self, params: &[(&str, ParamType)], build: impl FnOnce(&mut GraphBuilder)) -> AnimationGraphAsset {
        let mut asset = self.asset(params);
        let mut b = GraphBuilder::new("main", params);
        b.node("out", "anim.output_pose", &[]);
        build(&mut b);
        asset.main = b.into_graph();
        asset
    }

    pub fn try_compile(&self, asset: &AnimationGraphAsset) -> CompileOutput {
        compile_graph(asset, &self.src)
    }

    pub fn compile(&self, asset: &AnimationGraphAsset) -> Arc<CompiledGraph> {
        let out = self.try_compile(asset);
        out.graph.unwrap_or_else(|| panic!("graph did not compile: {:#?}", out.diagnostics))
    }

    pub fn instance(&self, asset: &AnimationGraphAsset) -> AnimInstance {
        let mut inst = AnimInstance::new(self.skel.clone());
        inst.set_graph(self.compile(asset)).unwrap();
        inst
    }
}

pub(crate) fn step(inst: &mut AnimInstance, dt: f32) -> Arc<crate::runtime::pose::Pose> {
    inst.advance(dt, TimeStep::Variable, UpdateMode::Play)
}

pub(crate) fn x1(inst: &AnimInstance) -> f32 {
    inst.pose().bones[1].translation.x
}

pub(crate) fn sub(id: &str, kind: GraphKind, graph: crate::anim_graph::builder::GraphBuilder) -> AnimSubGraph {
    AnimSubGraph { id: id.into(), name: id.into(), kind, graph: graph.into_graph() }
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

// ---- sources ---------------------------------------------------------------------------

#[test]
fn sequence_player_advances_loops_clamps_scales_rate_and_restarts_when_relevant() {
    let mut f = Fixture::new();
    let a = f.slide("A", 4.0, 0.0, 4.0);
    let asset = f.graph(&[("rate", ParamType::Float)], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&a)), ("start_position", json!(0.5))]);
        b.node("r", "anim.get_parameter", &[("parameter", js("rate"))]);
        b.wire("p", "out", "pose").wire("r", "p", "play_rate");
    });
    let mut inst = f.instance(&asset);
    inst.set_float("rate", 1.0).unwrap();
    step(&mut inst, 1.0);
    assert!(near(x1(&inst), 1.5), "start_position + 1s: {}", x1(&inst));
    inst.set_float("rate", 2.0).unwrap();
    step(&mut inst, 1.0);
    assert!(near(x1(&inst), 3.5), "rate 2: {}", x1(&inst));
    step(&mut inst, 1.0);
    assert!(near(x1(&inst), 1.5), "looped past the end (5.5 mod 4): {}", x1(&inst));

    let once = f.graph(&[], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&a)), ("looping", js("once"))]);
        b.wire("p", "out", "pose");
    });
    let mut inst = f.instance(&once);
    step(&mut inst, 10.0);
    assert!(near(x1(&inst), 4.0), "a non-looping player holds its last frame");
}

#[test]
fn a_branch_with_no_weight_does_not_advance_and_restarts_from_its_start_position_when_it_returns() {
    let mut f = Fixture::new();
    let a = f.slide("A", 10.0, 0.0, 10.0);
    let c = f.slide("C", 10.0, 100.0, 110.0);
    let asset = f.graph(&[("alpha", ParamType::Float)], |b| {
        b.node("a", "anim.sequence_player", &[("sequence", js(&a))]);
        b.node("c", "anim.sequence_player", &[("sequence", js(&c))]);
        b.node("al", "anim.get_parameter", &[("parameter", js("alpha"))]);
        b.node("blend", "anim.blend", &[]);
        b.wire("a", "blend", "a").wire("c", "blend", "b").wire("al", "blend", "alpha").wire("blend", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    inst.set_float("alpha", 0.0).unwrap();
    step(&mut inst, 2.0);
    assert!(near(x1(&inst), 2.0));
    // C has been irrelevant for two seconds; bringing it in starts it at 0, not at 2
    inst.set_float("alpha", 1.0).unwrap();
    step(&mut inst, 1.0);
    assert!(near(x1(&inst), 101.0), "{}", x1(&inst));
    inst.set_float("alpha", 0.5).unwrap();
    step(&mut inst, 1.0);
    // A was out for a tick, so it restarts at 0 and has run 1 s; C stayed relevant (2 s -> 102)
    assert!(near(x1(&inst), 0.5 * 1.0 + 0.5 * 102.0), "{}", x1(&inst));
}

#[test]
fn blend_multi_normalises_weights_and_all_zero_selects_the_first() {
    let mut f = Fixture::new();
    let a = f.slide("A", 10.0, 0.0, 0.0);
    let b2 = f.slide("B", 10.0, 10.0, 10.0);
    let c = f.slide("C", 10.0, 20.0, 20.0);
    let asset = f.graph(&[("w0", ParamType::Float), ("w1", ParamType::Float), ("w2", ParamType::Float)], |b| {
        b.node("a", "anim.sequence_player", &[("sequence", js(&a))]);
        b.node("b", "anim.sequence_player", &[("sequence", js(&b2))]);
        b.node("c", "anim.sequence_player", &[("sequence", js(&c))]);
        b.node("m", "anim.blend_multi", &[("count", json!(3))]);
        for i in 0..3 {
            b.node(&format!("w{i}"), "anim.get_parameter", &[("parameter", js(&format!("w{i}")))]);
            b.wire(&format!("w{i}"), "m", &format!("weight_{i}"));
        }
        b.wire("a", "m", "pose_0").wire("b", "m", "pose_1").wire("c", "m", "pose_2").wire("m", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    for (w, expect) in [([2.0, 1.0, 1.0], 0.5 * 0.0 + 0.25 * 10.0 + 0.25 * 20.0), ([0.0, 0.0, 0.0], 0.0), ([0.0, 0.0, 5.0], 20.0)] {
        for (i, v) in w.iter().enumerate() {
            inst.set_float(&format!("w{i}"), *v).unwrap();
        }
        step(&mut inst, 0.1);
        assert!(near(x1(&inst), expect), "{w:?}: {}", x1(&inst));
    }
}

#[test]
fn blend_by_bool_and_int_cross_fade_over_their_blend_time() {
    let mut f = Fixture::new();
    let a = f.slide("A", 10.0, 0.0, 0.0);
    let b2 = f.slide("B", 10.0, 10.0, 10.0);
    let asset = f.graph(&[("on", ParamType::Bool)], |b| {
        b.node("a", "anim.sequence_player", &[("sequence", js(&a))]);
        b.node("b", "anim.sequence_player", &[("sequence", js(&b2))]);
        b.node("c", "anim.get_parameter", &[("parameter", js("on"))]);
        b.node("sw", "anim.blend_by_bool", &[("blend_time", json!(1.0)), ("curve", js("linear"))]);
        b.wire("b", "sw", "when_true").wire("a", "sw", "when_false").wire("c", "sw", "condition").wire("sw", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    step(&mut inst, 0.1);
    assert!(near(x1(&inst), 0.0), "false selects A without a fade at start");
    inst.set_bool("on", true).unwrap();
    step(&mut inst, 0.5);
    assert!(near(x1(&inst), 5.0), "halfway through the fade: {}", x1(&inst));
    step(&mut inst, 0.5);
    assert!(near(x1(&inst), 10.0));
    inst.set_bool("on", false).unwrap();
    step(&mut inst, 0.25);
    assert!(near(x1(&inst), 7.5), "{}", x1(&inst));

    // by int: selector is clamped to the input range
    let asset = f.graph(&[("sel", ParamType::Int)], |b| {
        b.node("a", "anim.sequence_player", &[("sequence", js(&a))]);
        b.node("b", "anim.sequence_player", &[("sequence", js(&b2))]);
        b.node("s", "anim.get_parameter", &[("parameter", js("sel"))]);
        b.node("sw", "anim.blend_by_int", &[("count", json!(2)), ("blend_time", json!(0.0))]);
        b.wire("a", "sw", "pose_0").wire("b", "sw", "pose_1").wire("s", "sw", "selector").wire("sw", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    for (sel, expect) in [(0, 0.0), (1, 10.0), (9, 10.0), (-4, 0.0)] {
        inst.set_int("sel", sel).unwrap();
        step(&mut inst, 0.1);
        assert!(near(x1(&inst), expect), "selector {sel}: {}", x1(&inst));
    }
}

#[test]
fn a_cached_pose_is_evaluated_once_however_often_it_is_read() {
    let mut f = Fixture::new();
    let a = f.slide("A", 10.0, 0.0, 10.0);
    let asset = f.graph(&[], |b| {
        b.node("a", "anim.sequence_player", &[("sequence", js(&a))]);
        b.node("save", "anim.save_cached_pose", &[("name", js("Loco"))]);
        b.node("u1", "anim.use_cached_pose", &[("name", js("Loco"))]);
        b.node("u2", "anim.use_cached_pose", &[("name", js("Loco"))]);
        b.node("blend", "anim.blend", &[("alpha", json!(0.5))]);
        b.wire("a", "save", "pose").wire("u1", "blend", "a").wire("u2", "blend", "b").wire("blend", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    step(&mut inst, 1.0);
    assert!(near(x1(&inst), 1.0), "the player advanced once, not once per reader: {}", x1(&inst));
    assert_eq!(inst.report.spans.len(), 1);
}

// ---- blend spaces ----------------------------------------------------------------------

fn blend_space_asset(f: &mut Fixture, two_d: bool, sync: SyncMode, points: &[(f32, f32, f32, f32)]) -> String {
    // (x, y, clip x offset, clip duration)
    let mut asset = BlendSpaceAsset::default();
    asset.skeleton = Some(crate::ids::SkeletonId::parse(&f.skeleton_id).unwrap().to_ref());
    asset.x_axis = BlendAxis { name: "X".into(), min: 0.0, max: 10.0, ..BlendAxis::default() };
    if two_d {
        asset.y_axis = Some(BlendAxis { name: "Y".into(), min: 0.0, max: 10.0, ..BlendAxis::default() });
    }
    asset.sync.mode = sync;
    for (n, &(x, y, offset, duration)) in points.iter().enumerate() {
        let seq = f.slide(&format!("s{n}"), duration, offset, offset + duration);
        asset.samples.push(BlendSample::new(Some(pulsar_reflection::AssetRef::parse(&seq).unwrap()), x, two_d.then_some(y)));
    }
    let id = asset.id.as_str().to_string();
    f.src.blend_spaces.insert(id.clone(), Arc::new(asset));
    id
}

#[test]
fn blend_space_players_solve_weights_from_parameters_in_1d_and_2d() {
    let mut f = Fixture::new();
    let one_d = blend_space_asset(&mut f, false, SyncMode::None, &[(0.0, 0.0, 0.0, 100.0), (10.0, 0.0, 1000.0, 100.0)]);
    let asset = f.graph(&[("speed", ParamType::Float)], |b| {
        b.node("bs", "anim.blend_space_player", &[("blend_space", js(&one_d))]);
        b.node("sp", "anim.get_parameter", &[("parameter", js("speed"))]);
        b.wire("sp", "bs", "x").wire("bs", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    inst.set_float("speed", 2.5).unwrap();
    step(&mut inst, 1.0);
    // 0.75 * (0 + 1) + 0.25 * (1000 + 1) = 1 + 250
    assert!(near(x1(&inst), 251.0), "{}", x1(&inst));
    inst.set_float("speed", 999.0).unwrap();
    step(&mut inst, 1.0);
    assert!(near(x1(&inst), 1002.0), "clamped to the last sample: {}", x1(&inst));

    let two_d = blend_space_asset(&mut f, true, SyncMode::None, &[(0.0, 0.0, 0.0, 100.0), (10.0, 0.0, 100.0, 100.0), (0.0, 10.0, 200.0, 100.0)]);
    let asset = f.graph(&[("x", ParamType::Float), ("y", ParamType::Float)], |b| {
        b.node("bs", "anim.blend_space_player", &[("blend_space", js(&two_d))]);
        b.node("px", "anim.get_parameter", &[("parameter", js("x"))]);
        b.node("py", "anim.get_parameter", &[("parameter", js("y"))]);
        b.wire("px", "bs", "x").wire("py", "bs", "y").wire("bs", "out", "pose");
    });
    let mut a = f.instance(&asset);
    let mut b = f.instance(&asset);
    for inst in [&mut a, &mut b] {
        inst.set_float("x", 5.0).unwrap();
        inst.set_float("y", 5.0).unwrap();
        step(inst, 1.0);
    }
    // the point (5,5) lies on the hypotenuse: halfway between samples 1 and 2
    assert!(near(x1(&a), 0.5 * 101.0 + 0.5 * 201.0), "{}", x1(&a));
    assert_eq!(a.pose().bones, b.pose().bones, "deterministic");
}

#[test]
fn phase_synced_blend_space_samples_stay_aligned_despite_different_clip_lengths() {
    let mut f = Fixture::new();
    // slow clip: 4 s sliding 0..4 ; fast clip: 2 s sliding 100..102
    let space = blend_space_asset(&mut f, false, SyncMode::Phase, &[(0.0, 0.0, 0.0, 4.0), (10.0, 0.0, 100.0, 2.0)]);
    let asset = f.graph(&[("x", ParamType::Float)], |b| {
        b.node("bs", "anim.blend_space_player", &[("blend_space", js(&space))]);
        b.node("px", "anim.get_parameter", &[("parameter", js("x"))]);
        b.wire("px", "bs", "x").wire("bs", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    inst.set_float("x", 5.0).unwrap();
    // weighted cycle = 0.5*4 + 0.5*2 = 3 s; after 1.5 s both clips are at phase 0.5
    step(&mut inst, 1.5);
    let expect = 0.5 * (0.0 + 0.5 * 4.0) + 0.5 * (100.0 + 0.5 * 2.0);
    assert!(near(x1(&inst), expect), "{} vs {expect}", x1(&inst));
}

// ---- additive / layered ------------------------------------------------------------------

#[test]
fn additive_make_and_apply_round_trip_and_alpha_scales_the_difference() {
    let mut f = Fixture::new();
    let base = f.slide("base", 10.0, 0.0, 0.0);
    let lean = f.slide("lean", 10.0, 3.0, 3.0);
    let asset = f.graph(&[("alpha", ParamType::Float)], |b| {
        b.node("base", "anim.sequence_player", &[("sequence", js(&base))]);
        b.node("lean", "anim.sequence_player", &[("sequence", js(&lean))]);
        b.node("ref", "anim.reference_pose", &[]);
        b.node("mk", "anim.make_additive", &[]);
        b.node("ap", "anim.apply_additive", &[]);
        b.node("al", "anim.get_parameter", &[("parameter", js("alpha"))]);
        b.wire("lean", "mk", "source").wire("ref", "mk", "reference");
        b.wire("base", "ap", "base").wire("mk", "ap", "additive").wire("al", "ap", "alpha").wire("ap", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    inst.set_float("alpha", 1.0).unwrap();
    step(&mut inst, 0.1);
    // base x=0, additive = lean(3) - bind(0) = 3
    assert!(near(x1(&inst), 3.0), "{}", x1(&inst));
    inst.set_float("alpha", 0.5).unwrap();
    step(&mut inst, 0.1);
    assert!(near(x1(&inst), 1.5), "{}", x1(&inst));
    inst.set_float("alpha", 0.0).unwrap();
    step(&mut inst, 0.1);
    assert!(near(x1(&inst), 0.0));
}

#[test]
fn layered_blend_per_bone_applies_masks_weights_and_curve_ownership() {
    let mut f = Fixture::new();
    let mut base_tracks = vec![translate_track("b1", [1.0, 1.0, 0.0], [1.0, 1.0, 0.0], 10.0), translate_track("b2", [1.0, 1.0, 0.0], [1.0, 1.0, 0.0], 10.0)];
    let _ = &mut base_tracks;
    let base = f.seq("base", 10.0, base_tracks);
    let layer = f.seq("layer", 10.0, vec![translate_track("b1", [9.0, 1.0, 0.0], [9.0, 1.0, 0.0], 10.0), translate_track("b2", [9.0, 1.0, 0.0], [9.0, 1.0, 0.0], 10.0)]);
    let mask = json!([{ "branches": [{ "bone": "b2", "weight": 1.0 }], "default_weight": 0.0 }]);
    let asset = f.graph(&[("w", ParamType::Float)], |b| {
        b.node("base", "anim.sequence_player", &[("sequence", js(&base))]);
        b.node("layer", "anim.sequence_player", &[("sequence", js(&layer))]);
        b.node("w", "anim.get_parameter", &[("parameter", js("w"))]);
        b.node("lb", "anim.layered_blend_per_bone", &[("count", json!(1)), ("masks", mask)]);
        b.wire("base", "lb", "base").wire("layer", "lb", "layer_0").wire("w", "lb", "layer_weight_0").wire("lb", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    inst.set_float("w", 1.0).unwrap();
    step(&mut inst, 0.1);
    assert!(near(inst.pose().bones[1].translation.x, 1.0), "b1 is outside the mask");
    assert!(near(inst.pose().bones[2].translation.x, 9.0), "b2 is inside the mask");
    inst.set_float("w", 0.25).unwrap();
    step(&mut inst, 0.1);
    assert!(near(inst.pose().bones[2].translation.x, 1.0 + 0.25 * 8.0));
    inst.set_float("w", 0.0).unwrap();
    step(&mut inst, 0.1);
    assert!(near(inst.pose().bones[2].translation.x, 1.0), "weight 0 leaves the base");
}

// ---- curves ------------------------------------------------------------------------------

#[test]
fn curves_flow_through_get_and_modify_nodes() {
    let mut f = Fixture::new();
    let mut seq = clip_asset("c", 4.0, vec![]);
    seq.skeleton = Some(crate::ids::SkeletonId::parse(&f.skeleton_id).unwrap());
    let mut curve = crate::curves::AnimationCurve::new("speed".into(), false).unwrap();
    curve.set_key("k0".into(), 0.0, crate::curves::CurveValue::Float(0.0), crate::curves::CurveInterpolation::Linear, 4.0).unwrap();
    curve.set_key("k1".into(), 4.0, crate::curves::CurveValue::Float(8.0), crate::curves::CurveInterpolation::Linear, 4.0).unwrap();
    seq.features.curves.push(curve);
    let id = seq.id.as_str().to_string();
    f.src.sequences.insert(id.clone(), Arc::new(seq));
    let asset = f.graph(&[], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&id))]);
        b.node("mod", "anim.modify_curve", &[("curve", js("speed")), ("mode", js("scale")), ("value", json!(0.5))]);
        b.node("add", "anim.modify_curve", &[("curve", js("extra")), ("mode", js("set")), ("value", json!(7.0))]);
        b.wire("p", "mod", "pose").wire("mod", "add", "pose").wire("add", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    step(&mut inst, 2.0);
    assert!(near(inst.curves()["speed"], 2.0), "sampled 4, scaled by 0.5: {:?}", inst.curves());
    assert!(near(inst.curves()["extra"], 7.0));
}

// ---- state machines ------------------------------------------------------------------------

struct Trans<'a> {
    id: &'a str,
    from: &'a str,
    to: &'a str,
    /// A Bool parameter, or `"complete"` for the State Complete rule node.
    when: &'a str,
    props: Vec<(&'a str, Json)>,
}

fn t<'a>(id: &'a str, from: &'a str, to: &'a str, when: &'a str, props: Vec<(&'a str, Json)>) -> Trans<'a> {
    Trans { id, from, to, when, props }
}

/// Add a state machine `name` to `asset`; returns nothing, wires are created on `main`.
fn add_machine(asset: &mut AnimationGraphAsset, params: &[(&str, ParamType)], name: &str, states: &[(&str, &str, Vec<(&str, Json)>)], transitions: &[Trans]) {
    let mut m = GraphBuilder::new(name, params);
    m.node("entry", "sm.entry", &[]);
    for (i, (state, seq, props)) in states.iter().enumerate() {
        let sid = format!("s_{state}");
        let pose_id = format!("{name}_p_{state}");
        let mut props: Vec<(&str, Json)> = props.clone();
        props.push(("name", js(state)));
        props.push(("pose_graph", js(&pose_id)));
        m.node(&sid, "sm.state", &props);
        if i == 0 {
            m.wire_pin("entry", "start", &sid, "default");
        }
        let mut p = GraphBuilder::new(&pose_id, params);
        p.node("p", "anim.sequence_player", &[("sequence", js(seq))]);
        p.node("r", "anim.state_result", &[]);
        p.wire("p", "r", "pose");
        asset.subgraphs.push(sub(&pose_id, GraphKind::StatePose, p));
    }
    for tr in transitions {
        let rule_id = format!("{name}_r_{}", tr.id);
        let mut props: Vec<(&str, Json)> = tr.props.clone();
        props.push(("rule", js(&rule_id)));
        m.node(tr.id, "sm.transition", &props);
        m.wire_pin(&format!("s_{}", tr.from), "leave", tr.id, "from");
        m.wire_pin(&format!("s_{}", tr.to), "arrive", tr.id, "to");
        let mut r = GraphBuilder::new(&rule_id, params);
        r.node("res", "rule.result", &[]);
        if tr.when == "complete" {
            r.node("c", "rule.state_complete", &[]);
            r.wire("c", "res", "can_enter");
        } else {
            r.node("g", "anim.get_parameter", &[("parameter", js(tr.when))]);
            r.wire("g", "res", "can_enter");
        }
        asset.subgraphs.push(sub(&rule_id, GraphKind::TransitionRule, r));
    }
    asset.subgraphs.push(sub(name, GraphKind::StateMachine, m));
}

fn machine_asset(f: &Fixture, params: &[(&str, ParamType)], states: &[(&str, &str, Vec<(&str, Json)>)], transitions: &[Trans]) -> AnimationGraphAsset {
    let mut asset = f.graph(params, |b| {
        b.node("sm", "anim.state_machine", &[("machine", js("m"))]);
        b.wire("sm", "out", "pose");
    });
    add_machine(&mut asset, params, "m", states, transitions);
    asset
}

fn trace_has(inst: &AnimInstance, text: &str) -> bool {
    inst.report.trace.iter().any(|l| l.contains(text))
}

#[test]
fn a_transition_blends_over_its_duration_with_the_shaped_curve_and_then_settles() {
    let mut f = Fixture::new();
    let idle = f.slide("idle", 100.0, 0.0, 0.0);
    let walk = f.slide("walk", 100.0, 10.0, 10.0);
    let asset = machine_asset(
        &f,
        &[("go", ParamType::Bool)],
        &[("Idle", &idle, vec![]), ("Walk", &walk, vec![])],
        &[t("t1", "Idle", "Walk", "go", vec![("duration", json!(1.0)), ("curve", js("linear"))])],
    );
    let mut inst = f.instance(&asset);
    step(&mut inst, 0.5);
    assert!(near(x1(&inst), 0.0));
    assert!(trace_has(&inst, "Idle 1.00"), "{:?}", inst.report.trace);
    inst.set_bool("go", true).unwrap();
    step(&mut inst, 0.0);
    step(&mut inst, 0.5);
    assert!(near(x1(&inst), 5.0), "halfway: {}", x1(&inst));
    assert!(trace_has(&inst, "via t1"));
    step(&mut inst, 0.5);
    assert!(near(x1(&inst), 10.0), "{}", x1(&inst));
    step(&mut inst, 0.1);
    assert!(near(x1(&inst), 10.0));
    assert!(!trace_has(&inst, "via"), "the blend ended: {:?}", inst.report.trace);
    assert!(trace_has(&inst, "Walk 1.00"));
}

#[test]
fn the_lowest_priority_value_wins_and_a_rule_may_be_a_compare_of_a_parameter() {
    let mut f = Fixture::new();
    let idle = f.slide("idle", 100.0, 0.0, 0.0);
    let walk = f.slide("walk", 100.0, 10.0, 10.0);
    let run = f.slide("run", 100.0, 20.0, 20.0);
    let params = [("go", ParamType::Bool), ("speed", ParamType::Float)];
    let mut asset = machine_asset(
        &f,
        &params,
        &[("Idle", &idle, vec![]), ("Walk", &walk, vec![]), ("Run", &run, vec![])],
        &[
            t("to_walk", "Idle", "Walk", "go", vec![("duration", json!(0.0)), ("priority", json!(1))]),
            t("to_run", "Idle", "Run", "go", vec![("duration", json!(0.0)), ("priority", json!(0))]),
        ],
    );
    // swap the Idle -> Walk rule for `speed > 0.5`
    let rule = asset.subgraphs.iter_mut().find(|s| s.id == "m_r_to_walk").unwrap();
    let mut r = GraphBuilder::new("r", &params);
    r.node("res", "rule.result", &[]).node("g", "anim.get_parameter", &[("parameter", js("speed"))]).node("cmp", "math.compare", &[("op", js("gt")), ("b", json!(0.5))]);
    r.wire("g", "cmp", "a").wire("cmp", "res", "can_enter");
    rule.graph = r.into_graph();
    let mut inst = f.instance(&asset);
    inst.set_float("speed", 3.0).unwrap();
    inst.set_bool("go", true).unwrap();
    step(&mut inst, 0.1);
    step(&mut inst, 0.1);
    assert!(near(x1(&inst), 20.0), "both rules were true; priority 0 (Run) fires first: {}", x1(&inst));
}

#[test]
fn only_an_interruptible_transition_can_be_replaced_and_it_blends_from_the_current_mixture() {
    let mut f = Fixture::new();
    let a = f.slide("a", 100.0, 0.0, 0.0);
    let b2 = f.slide("b", 100.0, 10.0, 10.0);
    let c = f.slide("c", 100.0, 100.0, 100.0);
    for (interrupt, expect_interrupted) in [("none", false), ("target", true)] {
        let asset = machine_asset(
            &f,
            &[("ab", ParamType::Bool), ("bc", ParamType::Bool)],
            &[("A", &a, vec![]), ("B", &b2, vec![]), ("C", &c, vec![])],
            &[
                t("ab", "A", "B", "ab", vec![("duration", json!(1.0)), ("curve", js("linear")), ("interrupt", js(interrupt))]),
                t("bc", "B", "C", "bc", vec![("duration", json!(1.0)), ("curve", js("linear"))]),
            ],
        );
        let mut inst = f.instance(&asset);
        step(&mut inst, 0.1);
        inst.set_bool("ab", true).unwrap();
        step(&mut inst, 0.0);
        step(&mut inst, 0.5);
        assert!(near(x1(&inst), 5.0), "{interrupt}: halfway A->B: {}", x1(&inst));
        inst.set_bool("bc", true).unwrap();
        inst.set_bool("ab", false).unwrap();
        step(&mut inst, 0.0);
        step(&mut inst, 0.5);
        if expect_interrupted {
            // new blend from {A:0.5, B:0.5} to C, half done: 0.25 A, 0.25 B, 0.5 C
            assert!(near(x1(&inst), 0.25 * 0.0 + 0.25 * 10.0 + 0.5 * 100.0), "{interrupt}: {}", x1(&inst));
        } else {
            assert!(near(x1(&inst), 10.0), "{interrupt}: the first blend finishes untouched: {}", x1(&inst));
        }
    }
}

#[test]
fn re_entry_restarts_a_state_while_the_old_instance_fades() {
    let mut f = Fixture::new();
    let hit = f.slide("hit", 100.0, 0.0, 100.0);
    let idle = f.slide("idle", 100.0, 0.0, 0.0);
    let asset = machine_asset(
        &f,
        &[("again", ParamType::Bool)],
        &[("Hit", &hit, vec![]), ("Idle", &idle, vec![])],
        &[t("re", "Hit", "Hit", "again", vec![("duration", json!(0.0)), ("allow_reentry", json!(true))])],
    );
    let mut inst = f.instance(&asset);
    step(&mut inst, 3.0);
    assert!(near(x1(&inst), 3.0));
    inst.set_bool("again", true).unwrap();
    step(&mut inst, 0.5);
    assert!(near(x1(&inst), 0.5), "a fresh instance of the same state: {}", x1(&inst));
    let _ = &idle;
}

#[test]
fn state_complete_drives_a_transition_and_nested_machines_run_inside_a_state() {
    let mut f = Fixture::new();
    let mut once_props = Vec::new();
    once_props.push(("looping", js("once")));
    let a_id = f.slide("a", 1.0, 0.0, 1.0);
    let b2 = f.slide("b", 100.0, 50.0, 50.0);
    // outer machine: First (plays a, one second, once) -> Second when complete
    let params: [(&str, ParamType); 0] = [];
    let mut asset = f.graph(&params, |b| {
        b.node("sm", "anim.state_machine", &[("machine", js("outer"))]);
        b.wire("sm", "out", "pose");
    });
    // First's pose graph is a nested machine "inner" (one state playing `a` once)
    add_machine(&mut asset, &params, "inner", &[("Only", &a_id, vec![])], &[]);
    // make the inner player non-looping
    for s in asset.subgraphs.iter_mut().filter(|s| s.id == "inner_p_Only") {
        s.graph.nodes.get_mut("p").unwrap().properties.insert("looping".into(), js("once"));
    }
    let mut outer = GraphBuilder::new("outer", &params);
    outer.node("entry", "sm.entry", &[]);
    outer.node("s_First", "sm.state", &[("name", js("First")), ("pose_graph", js("outer_p_First"))]);
    outer.node("s_Second", "sm.state", &[("name", js("Second")), ("pose_graph", js("outer_p_Second"))]);
    outer.wire_pin("entry", "start", "s_First", "default");
    outer.node("t", "sm.transition", &[("rule", js("outer_r_t")), ("duration", json!(0.0))]);
    outer.wire_pin("s_First", "leave", "t", "from").wire_pin("s_Second", "arrive", "t", "to");
    let mut first = GraphBuilder::new("outer_p_First", &params);
    first.node("n", "anim.state_machine", &[("machine", js("inner"))]).node("r", "anim.state_result", &[]).wire("n", "r", "pose");
    let mut second = GraphBuilder::new("outer_p_Second", &params);
    second.node("p", "anim.sequence_player", &[("sequence", js(&b2))]).node("r", "anim.state_result", &[]).wire("p", "r", "pose");
    // rule: the *source state's primary player* has finished -- the nested machine's player
    let mut rule = GraphBuilder::new("outer_r_t", &params);
    rule.node("c", "rule.state_complete", &[]).node("res", "rule.result", &[]).wire("c", "res", "can_enter");
    asset.subgraphs.push(sub("outer_p_First", GraphKind::StatePose, first));
    asset.subgraphs.push(sub("outer_p_Second", GraphKind::StatePose, second));
    asset.subgraphs.push(sub("outer_r_t", GraphKind::TransitionRule, rule));
    asset.subgraphs.push(sub("outer", GraphKind::StateMachine, outer));
    let compiled = f.try_compile(&asset);
    assert!(compiled.diagnostics.iter().all(|d| d.severity != Severity::Error), "{:#?}", compiled.diagnostics);
    let mut inst = f.instance(&asset);
    step(&mut inst, 0.5);
    assert!(near(x1(&inst), 0.5), "the nested machine's state plays: {}", x1(&inst));
    assert!(trace_has(&inst, "Only 1.00"), "{:?}", inst.report.trace);
    // the outer rule only sees a primary *player* directly in First's graph; a nested machine
    // has none, so State Complete stays false and First is never left
    step(&mut inst, 5.0);
    assert!(near(x1(&inst), 1.0), "the inner once-player holds its end frame: {}", x1(&inst));
    let _ = once_props;
}

#[test]
fn state_complete_of_a_direct_player_ends_a_one_shot_state() {
    let mut f = Fixture::new();
    let shot = f.slide("shot", 1.0, 0.0, 1.0);
    let idle = f.slide("idle", 100.0, 50.0, 50.0);
    let mut asset = machine_asset(
        &f,
        &[],
        &[("Shot", &shot, vec![]), ("Idle", &idle, vec![])],
        &[t("done", "Shot", "Idle", "complete", vec![("duration", json!(0.0))])],
    );
    asset.subgraphs.iter_mut().find(|s| s.id == "m_p_Shot").unwrap().graph.nodes.get_mut("p").unwrap().properties.insert("looping".into(), js("once"));
    let mut inst = f.instance(&asset);
    step(&mut inst, 0.5);
    assert!(near(x1(&inst), 0.5));
    step(&mut inst, 0.6);
    step(&mut inst, 0.1);
    assert!(near(x1(&inst), 50.0), "{}", x1(&inst));
}

// ---- montage slot ------------------------------------------------------------------------------

#[test]
fn a_masked_montage_plays_over_locomotion_through_the_slot_in_one_pose_and_reports_its_result() {
    use crate::runtime::montage::{MontageEventKind, MontageResult};
    let mut f = Fixture::new();
    let loco = f.slide("loco", 100.0, 0.0, 100.0);
    let asset = f.graph(&[], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&loco))]);
        b.node("slot", "anim.slot", &[("slot", js("UpperBody")), ("group", js("DefaultGroup"))]);
        b.wire("p", "slot", "source").wire("slot", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    let wave = crate::runtime::montage::tests::montage(&f.skel, "wave", "UpperBody", "b2", Some("b2"));
    step(&mut inst, 1.0);
    assert!(near(x1(&inst), 1.0));
    let h = inst.play_montage(wave, 1.0, None).unwrap();
    step(&mut inst, 1.0);
    assert!(near(inst.pose().bones[1].translation.x, 2.0), "locomotion keeps advancing under the montage");
    assert!(near(inst.pose().bones[2].translation.x, 1.0), "the masked bone plays the montage: {}", inst.pose().bones[2].translation.x);
    assert!(inst.report.montage_events.iter().any(|e| e.handle == h && matches!(e.kind, MontageEventKind::Started)), "{:?}", inst.report.montage_events);
    step(&mut inst, 5.0);
    let ended: Vec<_> = inst.report.montage_events.iter().filter_map(|e| if let MontageEventKind::Ended(r) = e.kind { Some(r) } else { None }).collect();
    assert_eq!(ended, [MontageResult::Completed]);
    assert!(near(inst.pose().bones[2].translation.x, 0.0), "after the montage the slot passes the source through");
    // scrubbing never reports
    let h2 = inst.play_montage(crate::runtime::montage::tests::montage(&f.skel, "wave2", "UpperBody", "b2", None), 1.0, None).unwrap();
    inst.advance(5.0, TimeStep::Variable, UpdateMode::Scrub);
    assert!(inst.report.montage_events.is_empty() && inst.report.spans.is_empty());
    let _ = h2;
    inst.shutdown();
    assert!(inst.report.montage_events.iter().any(|e| matches!(e.kind, MontageEventKind::Ended(MontageResult::Cancelled))) || inst.montages.instances().is_empty());
}

// ---- IK / retarget ---------------------------------------------------------------------------------

fn tip(inst: &AnimInstance, bone: usize) -> Vec3 {
    inst.skeleton().bone_mesh_space(&inst.pose().bones, bone).to_scale_rotation_translation().2
}

#[test]
fn two_bone_ik_and_fabrik_reach_targets_built_from_parameters() {
    let mut f = Fixture::new();
    let rest = f.seq("rest", 10.0, vec![]);
    for node in ["anim.two_bone_ik", "anim.fabrik"] {
        let props: Vec<(&str, Json)> = if node == "anim.two_bone_ik" { vec![("end_bone", js("b2"))] } else { vec![("tip_bone", js("b2")), ("root_bone", js("b0")), ("max_iterations", json!(40)), ("precision", json!(0.0001))] };
        let asset = f.graph(&[("target", ParamType::Vector)], |b| {
            b.node("p", "anim.sequence_player", &[("sequence", js(&rest))]);
            b.node("t", "anim.get_parameter", &[("parameter", js("target"))]);
            b.node("mk", "anim.make_transform", &[]);
            b.node("ik", node, &props);
            b.wire("t", "mk", "location").wire("p", "ik", "pose").wire("mk", "ik", "effector").wire("ik", "out", "pose");
        });
        let mut inst = f.instance(&asset);
        inst.set_vector("target", [1.0, 1.0, 0.0]).unwrap();
        step(&mut inst, 0.1);
        assert!((tip(&inst, 2) - Vec3::new(1.0, 1.0, 0.0)).length() < 5e-3, "{node}: {}", tip(&inst, 2));
        // bone lengths are kept
        assert!(((tip(&inst, 2) - tip(&inst, 1)).length() - 1.0).abs() < 5e-3);
        // an unreachable target extends toward it
        inst.set_vector("target", [0.0, 10.0, 0.0]).unwrap();
        step(&mut inst, 0.1);
        assert!((tip(&inst, 2) - Vec3::new(0.0, 2.0, 0.0)).length() < 5e-2, "{node}: {}", tip(&inst, 2));
    }
}

#[test]
fn ik_effectors_can_follow_another_posed_bone_and_alpha_blends_the_solve() {
    let mut f = Fixture::new();
    let rest = f.seq("rest", 10.0, vec![]);
    // a target pose that holds b2 at (0.8, 1.2, 0) in its own local space under b1
    let moved = f.seq("moved", 10.0, vec![BoneTrack { bone_id: "b2".into(), keyframes: vec![Keyframe::linear(0.0, Transform { translation: AV3::new(0.8, 0.2, 0.0), ..Transform::IDENTITY })] }]);
    let asset = f.graph(&[("a", ParamType::Float)], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&rest))]);
        b.node("q", "anim.sequence_player", &[("sequence", js(&moved))]);
        b.node("bt", "anim.get_bone_transform", &[("bone", js("b2")), ("space", js("mesh"))]);
        b.node("a", "anim.get_parameter", &[("parameter", js("a"))]);
        b.node("ik", "anim.two_bone_ik", &[("end_bone", js("b2"))]);
        b.wire("q", "bt", "pose").wire("p", "ik", "pose").wire("bt", "ik", "effector").wire("a", "ik", "alpha").wire("ik", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    inst.set_float("a", 1.0).unwrap();
    step(&mut inst, 0.1);
    let want = Vec3::new(0.8, 1.2, 0.0);
    assert!((tip(&inst, 2) - want).length() < 5e-3, "{} vs {want}", tip(&inst, 2));
    inst.set_float("a", 0.0).unwrap();
    step(&mut inst, 0.1);
    assert!((tip(&inst, 2) - Vec3::new(0.0, 2.0, 0.0)).length() < 1e-4, "alpha 0 keeps the input pose");
}

#[test]
fn an_ik_rig_node_runs_the_rig_stack_with_goals_from_the_graph() {
    use crate::ik_rig::{IkChain, IkGoal, IkRigAsset, IkSolver, SolverKind};
    let mut f = Fixture::new();
    let rest = f.seq("rest", 10.0, vec![]);
    let mut rig = IkRigAsset::new("rig", Some(crate::ids::SkeletonId::parse(&f.skeleton_id).unwrap().to_ref()));
    rig.chains.push(IkChain { id: "c".into(), name: "arm".into(), start_bone: "b0".into(), end_bone: "b2".into(), goal: Some("g".into()) });
    rig.goals.push(IkGoal { id: "g".into(), name: "hand".into(), bone: "b2".into(), position_alpha: 1.0, rotation_alpha: 1.0, default_offset: [0.0; 3] });
    rig.solvers.push(IkSolver { id: "s".into(), name: "reach".into(), enabled: true, kind: SolverKind::Fabrik { chain: "c".into(), goal: "g".into(), precision: 1e-4, max_iterations: 40, respect_joint_limits: false, take_goal_rotation: false } });
    let rig_id = rig.id.as_str().to_string();
    f.src.ik_rigs.insert(rig_id.clone(), Arc::new(rig));
    let asset = f.graph(&[("target", ParamType::Vector)], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&rest))]);
        b.node("t", "anim.get_parameter", &[("parameter", js("target"))]);
        b.node("mk", "anim.make_transform", &[]);
        b.node("ik", "anim.ik_rig", &[("rig", js(&rig_id)), ("goals", json!(["hand"]))]);
        b.wire("t", "mk", "location").wire("p", "ik", "pose").wire("mk", "ik", "goal_hand").wire("ik", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    inst.set_vector("target", [1.0, 1.0, 0.0]).unwrap();
    step(&mut inst, 0.1);
    assert!((tip(&inst, 2) - Vec3::new(1.0, 1.0, 0.0)).length() < 5e-3, "{}", tip(&inst, 2));
    assert!(inst.report.trace.iter().any(|l| l.contains("reach")), "{:?}", inst.report.trace);
    // a missing goal name is a compile error that names the goal
    let bad = f.graph(&[], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&rest))]);
        b.node("ik", "anim.ik_rig", &[("rig", js(&rig_id)), ("goals", json!(["foot"]))]);
        b.wire("p", "ik", "pose").wire("ik", "out", "pose");
    });
    assert!(f.try_compile(&bad).diagnostics.iter().any(|d| d.code == "graph.ik_goal"));
}

fn source_skeleton() -> (SkeletonAsset, Arc<RuntimeSkeleton>) {
    let sk = crate::skeleton::Skeleton {
        bones: (0..3)
            .map(|i| crate::skeleton::Bone {
                id: format!("s{i}"),
                name: format!("s{i}"),
                parent: (i > 0).then(|| format!("s{}", i - 1)),
                bind_transform: Transform { translation: AV3::new(0.0, if i == 0 { 0.0 } else { 0.5 }, 0.0), ..Transform::IDENTITY },
            })
            .collect(),
    };
    let asset = SkeletonAsset::new(sk);
    let rt = Arc::new(RuntimeSkeleton::new(asset.id.as_str(), &asset.skeleton()).unwrap());
    (asset, rt)
}

#[test]
fn a_retarget_node_maps_a_source_skeleton_subtree_onto_the_graph_skeleton() {
    use crate::retarget::{BoneMapping, RetargetProfileAsset, RootPolicy, RotationMode, TranslationMode};
    let mut f = Fixture::new();
    let (src_asset, _src_rt) = source_skeleton();
    let src_id = src_asset.id.as_str().to_string();
    f.src.skeletons.insert(src_id.clone(), Arc::new(src_asset));
    // a clip on the *source* skeleton: s1 rotated 30 degrees about z
    let mut clip = clip_asset("src", 10.0, vec![BoneTrack { bone_id: "s1".into(), keyframes: vec![Keyframe::linear(0.0, Transform { translation: AV3::new(0.0, 0.5, 0.0), rotation: AV3::new(0.0, 0.0, 30.0), ..Transform::IDENTITY })] }]);
    clip.skeleton = Some(crate::ids::SkeletonId::parse(&src_id).unwrap());
    let seq_id = clip.id.as_str().to_string();
    f.src.sequences.insert(seq_id.clone(), Arc::new(clip));
    let mut profile = RetargetProfileAsset::new("p", Some(crate::ids::SkeletonId::parse(&src_id).unwrap().to_ref()), Some(crate::ids::SkeletonId::parse(&f.skeleton_id).unwrap().to_ref()));
    for (s, t) in [("s0", "b0"), ("s1", "b1"), ("s2", "b2")] {
        profile.bone_map.push(BoneMapping { id: format!("m{t}"), source_bone: s.into(), target_bone: t.into(), rotation: RotationMode::OneToOne, translation: TranslationMode::None });
    }
    profile.root = RootPolicy { source_root: Some("s0".into()), target_root: Some("b0".into()), ..RootPolicy::default() };
    let profile_id = profile.id.as_str().to_string();
    f.src.profiles.insert(profile_id.clone(), Arc::new(profile));
    let asset = f.graph(&[], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&seq_id))]);
        b.node("rt", "anim.retarget", &[("profile", js(&profile_id))]);
        b.wire("p", "rt", "source").wire("rt", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    step(&mut inst, 0.1);
    let want = glam::Quat::from_rotation_z(30f32.to_radians());
    assert!(inst.pose().bones[1].rotation.dot(want).abs() > 0.9999, "{:?}", inst.pose().bones[1].rotation);
    assert_eq!(inst.pose().bones.len(), 3);
    // the target keeps its own bone lengths (1.0, not the source's 0.5)
    assert!((inst.pose().bones[1].translation - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-4);

    // a source subtree that also feeds the target pose is refused
    let bad = f.graph(&[], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&seq_id))]);
        b.node("rt", "anim.retarget", &[("profile", js(&profile_id))]);
        b.node("mix", "anim.blend", &[]);
        b.wire("p", "rt", "source").wire("rt", "mix", "a").wire("p", "mix", "b").wire("mix", "out", "pose");
    });
    assert!(f.try_compile(&bad).diagnostics.iter().any(|d| d.code == "graph.mixed_skeleton"));
}

// ---- pose search -------------------------------------------------------------------------------------

fn pose_db(f: &mut Fixture) -> (String, String, String) {
    use crate::pose_db::{ChannelKind, FeatureChannel, Normalization, PoseDbAsset, PoseDbSource, PoseSource};
    // walk: the root moves forward at 2 m/s while b1 slides; idle: stands still
    let mut walk = clip_asset("walk", 4.0, vec![translate_track("b0", [0.0; 3], [0.0, 0.0, 8.0], 4.0)]);
    walk.skeleton = Some(crate::ids::SkeletonId::parse(&f.skeleton_id).unwrap());
    walk.features.root_motion.enabled = true;
    let walk_id = walk.id.as_str().to_string();
    f.src.sequences.insert(walk_id.clone(), Arc::new(walk));
    let idle_id = f.seq("idle", 4.0, vec![]);
    let mut db = PoseDbAsset::new("db", Some(crate::ids::SkeletonId::parse(&f.skeleton_id).unwrap().to_ref()));
    db.sampling.interval = 0.25;
    db.sampling.end_margin = 0.5;
    for (n, id) in [("walk", &walk_id), ("idle", &idle_id)] {
        db.sources.push(PoseDbSource { id: n.into(), source: PoseSource::Sequence { sequence: pulsar_reflection::AssetRef::parse(id).unwrap() }, enabled: true, mirrored: false, looping: true, exclusions: vec![], cost_bias: 0.0 });
    }
    db.channels.push(FeatureChannel { id: "t".into(), name: "traj".into(), weight: 1.0, normalization: Normalization::Standardize, kind: ChannelKind::Trajectory { offsets: vec![0.5], position: true, facing: false, velocity: true } });
    let db_id = db.id.as_str().to_string();
    f.src.pose_dbs.insert(db_id.clone(), Arc::new(db));
    (db_id, walk_id, idle_id)
}

#[test]
fn pose_search_picks_the_matching_clip_reports_its_cost_and_plays_the_fallback_without_a_database() {
    let mut f = Fixture::new();
    let (db_id, _walk, _idle) = pose_db(&mut f);
    let fallback = f.slide("fallback", 10.0, 77.0, 77.0);
    let build = |f: &Fixture, db: &str| {
        f.graph(&[("speed", ParamType::Float)], |b| {
            b.node("ps", "anim.pose_search", &[("database", js(db)), ("search_interval", json!(0.0)), ("query_debug", json!(true)), ("blend_time", json!(0.0))]);
            b.node("sp", "anim.get_parameter", &[("parameter", js("speed"))]);
            b.node("dir", "math.constant_vector", &[("value", json!([0.0, 0.0, 1.0]))]);
            b.node("fb", "anim.sequence_player", &[("sequence", js(&fallback))]);
            b.wire("sp", "ps", "trajectory_speed").wire("dir", "ps", "trajectory_direction").wire("fb", "ps", "fallback").wire("ps", "out", "pose");
        })
    };
    let asset = build(&f, &db_id);
    let out = f.try_compile(&asset);
    assert!(out.graph.is_some(), "{:#?}", out.diagnostics);
    assert_eq!(f.src.index_builds(), 1, "the derived index is stored once");
    let mut inst = f.instance(&asset);
    assert_eq!(f.src.index_builds(), 1, "a second compile reuses the cached index");
    inst.set_float("speed", 2.0).unwrap();
    step(&mut inst, 0.1);
    let walking = inst.report.searches.last().and_then(|s| s.result.clone()).expect("a query ran");
    assert_eq!(walking.clip, 0, "walking matches the walk clip");
    assert!(!walking.breakdown.is_empty() && walking.cost.is_finite());
    inst.set_float("speed", 0.0).unwrap();
    step(&mut inst, 0.1);
    let idle = inst.report.searches.last().and_then(|s| s.result.clone()).unwrap();
    assert_eq!(idle.clip, 1, "standing still matches the idle clip");
    // the same inputs always pick the same entry
    let mut again = f.instance(&asset);
    again.set_float("speed", 2.0).unwrap();
    step(&mut again, 0.1);
    assert_eq!(again.report.searches.last().and_then(|s| s.result.clone()).unwrap().entry, walking.entry);

    // a missing database plays the connected fallback
    let broken = build(&f, "00000000-0000-4000-8000-000000000000");
    let out = f.try_compile(&broken);
    assert!(out.diagnostics.iter().any(|d| d.code == "graph.pose_search_fallback"), "{:#?}", out.diagnostics);
    let mut inst = f.instance(&broken);
    step(&mut inst, 0.5);
    assert!(near(x1(&inst), 77.0), "{}", x1(&inst));
    // without a fallback it is an error, never a silent default pose
    let no_fallback = f.graph(&[], |b| {
        b.node("ps", "anim.pose_search", &[("database", js("00000000-0000-4000-8000-000000000000"))]);
        b.wire("ps", "out", "pose");
    });
    assert!(f.try_compile(&no_fallback).diagnostics.iter().any(|d| d.code == "graph.pose_search_unavailable"));
}

// ---- sync groups ----------------------------------------------------------------------------------------

#[test]
fn sync_group_followers_track_the_leaders_normalised_phase() {
    let mut f = Fixture::new();
    let slow = f.slide("slow", 4.0, 0.0, 4.0);
    let fast = f.slide("fast", 2.0, 0.0, 2.0);
    let asset = f.graph(&[], |b| {
        b.node("a", "anim.sequence_player", &[("sequence", js(&slow)), ("sync_group", js("gait")), ("sync_role", js("leader"))]);
        b.node("b", "anim.sequence_player", &[("sequence", js(&fast)), ("sync_group", js("gait")), ("sync_role", js("follower"))]);
        b.node("blend", "anim.blend", &[("alpha", json!(0.5))]);
        b.wire("a", "blend", "a").wire("b", "blend", "b").wire("blend", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    for _ in 0..5 {
        step(&mut inst, 0.7);
    }
    let a = inst.report.spans.iter().find(|s| matches!(&s.origin, crate::runtime::graph::SpanOrigin::SequencePlayer { node } if node == "a")).unwrap().to;
    let b = inst.report.spans.iter().find(|s| matches!(&s.origin, crate::runtime::graph::SpanOrigin::SequencePlayer { node } if node == "b")).unwrap().to;
    assert!(((a / 4.0).rem_euclid(1.0) - (b / 2.0).rem_euclid(1.0)).abs() < 1e-4, "phases {a}/4 vs {b}/2");
    assert!(a > 3.0, "the leader advanced at its own rate");
}

// ---- root motion, stepping, scrubbing, reload -------------------------------------------------------------------

#[test]
fn root_motion_is_extracted_removed_from_the_pose_and_survives_loop_seams_and_fixed_steps() {
    let mut f = Fixture::new();
    let mut walk = clip_asset("walk", 1.0, vec![translate_track("b0", [0.0; 3], [0.0, 0.0, 4.0], 1.0)]);
    walk.skeleton = Some(crate::ids::SkeletonId::parse(&f.skeleton_id).unwrap());
    walk.features.root_motion.enabled = true;
    let id = walk.id.as_str().to_string();
    f.src.sequences.insert(id.clone(), Arc::new(walk));
    let asset = f.graph(&[], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&id))]);
        b.wire("p", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    step(&mut inst, 0.75);
    step(&mut inst, 0.75);
    assert!(near(inst.report.root_motion.translation.z, 3.0), "{:?}", inst.report.root_motion.translation);
    assert!(inst.pose().bones[0].translation.z.abs() < 1e-3, "the pose itself no longer carries the travel");

    // fixed stepping: the same total motion regardless of how time is sliced
    let mut fixed = f.instance(&asset);
    let mut total = crate::runtime::pose::RootMotion::IDENTITY;
    let mut substeps = 0;
    for dt in [0.04, 0.08, 0.5, 0.88] {
        fixed.advance(dt, TimeStep::Fixed { step: 0.1, max_substeps: 16 }, UpdateMode::Play);
        total = total.then(&fixed.report.root_motion);
        substeps += fixed.report.substeps;
    }
    assert_eq!(substeps, 15, "1.5 s of time at 0.1 s steps");
    assert!(near(total.translation.z, 6.0 * 0.0 + 4.0 * 1.5), "{:?}", total.translation);
    // the spiral guard drops time beyond max_substeps instead of running away
    let mut guard = f.instance(&asset);
    guard.advance(100.0, TimeStep::Fixed { step: 0.1, max_substeps: 4 }, UpdateMode::Play);
    assert_eq!(guard.report.substeps, 4);
}

#[test]
fn pausing_scrubbing_and_the_single_sequence_player_never_raise_gameplay_data() {
    let mut f = Fixture::new();
    let a = f.slide("A", 4.0, 0.0, 4.0);
    let clip = Arc::new(crate::runtime::clip::CompiledClip::compile(&f.src.sequences[&a], &f.skel));
    let mut inst = AnimInstance::new(f.skel.clone());
    inst.set_graph(crate::runtime::anim_instance::single_sequence_graph(f.skel.clone(), clip, true, false)).unwrap();
    step(&mut inst, 1.0);
    assert!(near(x1(&inst), 1.0));
    assert_eq!(inst.report.spans.len(), 1, "playback is reported as a span");
    // pause: a zero delta keeps the pose
    step(&mut inst, 0.0);
    assert!(near(x1(&inst), 1.0));
    assert!(inst.report.spans.iter().all(|s| s.from == s.to), "a paused player is live but does not move");
    // seek + refresh: the pose moves, nothing is reported
    assert!(inst.seek_single_sequence(2.5));
    inst.refresh();
    assert!(near(x1(&inst), 2.5));
    assert!(inst.report.spans.iter().all(|s| s.from == s.to), "refresh reports nothing new");
    assert_eq!(inst.single_sequence_position(), Some(2.5));
    // the previous pose is the one before the latest advance
    step(&mut inst, 0.5);
    assert!(near(inst.previous_pose().bones[1].translation.x, 2.5));
    assert!(near(x1(&inst), 3.0));
    // non-finite and negative deltas are harmless
    step(&mut inst, f32::NAN);
    step(&mut inst, -3.0);
    assert!(near(x1(&inst), 3.0));
}

#[test]
fn hot_reload_keeps_surviving_node_state_and_parameter_values() {
    let mut f = Fixture::new();
    let a = f.slide("A", 100.0, 0.0, 100.0);
    let build = |gain: f32| {
        f.graph(&[("k", ParamType::Float)], |b| {
            b.node("p", "anim.sequence_player", &[("sequence", js(&a))]);
            b.node("m", "anim.modify_curve", &[("curve", js("gain")), ("value", json!(gain))]);
            b.wire("p", "m", "pose").wire("m", "out", "pose");
        })
    };
    let mut inst = f.instance(&build(1.0));
    inst.set_float("k", 4.0).unwrap();
    step(&mut inst, 3.0);
    assert!(near(inst.curves()["gain"], 1.0));
    inst.set_graph(f.compile(&build(2.0))).unwrap();
    step(&mut inst, 1.0);
    assert!(near(x1(&inst), 4.0), "the player kept its clock across the edit: {}", x1(&inst));
    assert!(near(inst.curves()["gain"], 2.0), "and picked up the edit");
    assert_eq!(inst.parameter("k"), Some(&Value::Float(4.0)), "parameters keep their values");
    // a graph for another skeleton is refused
    let (other_asset, other) = source_skeleton();
    let _ = other_asset;
    let mut foreign = AnimInstance::new(other);
    assert!(foreign.set_graph(f.compile(&build(1.0))).is_err());
}

// ---- layers and linked graphs ----------------------------------------------------------------------------------------

#[test]
fn layers_receive_their_input_pose_and_linked_graphs_share_same_named_parameters() {
    let mut f = Fixture::new();
    let a = f.slide("A", 100.0, 0.0, 100.0);
    let c = f.slide("C", 100.0, 1000.0, 1000.0);
    // linked asset: blends A and C by its parameter `mix`
    let mut linked = f.graph(&[("mix", ParamType::Float)], |b| {
        b.node("a", "anim.sequence_player", &[("sequence", js(&a))]);
        b.node("c", "anim.sequence_player", &[("sequence", js(&c))]);
        b.node("m", "anim.get_parameter", &[("parameter", js("mix"))]);
        b.node("bl", "anim.blend", &[]);
        b.wire("a", "bl", "a").wire("c", "bl", "b").wire("m", "bl", "alpha").wire("bl", "out", "pose");
    });
    linked.name = "linked".into();
    let linked_id = linked.id.as_str().to_string();
    f.src.graphs.insert(linked_id.clone(), Arc::new(linked));
    let mut asset = f.graph(&[("mix", ParamType::Float)], |b| {
        b.node("lg", "anim.linked_graph", &[("graph", js(&linked_id))]);
        b.node("ly", "anim.layer", &[("layer", js("tag"))]);
        b.wire("lg", "ly", "input").wire("ly", "out", "pose");
    });
    let mut layer = GraphBuilder::new("tag", &[]);
    layer.node("in", "anim.layer_input", &[]).node("mod", "anim.modify_curve", &[("curve", js("tagged")), ("value", json!(5.0))]).node("outp", "anim.layer_output", &[]);
    layer.wire("in", "mod", "pose").wire("mod", "outp", "pose");
    asset.subgraphs.push(sub("tag", GraphKind::Layer, layer));
    let mut inst = f.instance(&asset);
    inst.set_float("mix", 0.0).unwrap();
    step(&mut inst, 1.0);
    assert!(near(x1(&inst), 1.0), "{}", x1(&inst));
    assert!(near(inst.curves()["tagged"], 5.0), "the layer ran on the linked graph's output");
    inst.set_float("mix", 1.0).unwrap();
    step(&mut inst, 1.0);
    assert!(near(x1(&inst), 1000.0), "the parent's parameter reached the linked graph: {}", x1(&inst));
    assert!(inst.graph().unwrap().dependencies.iter().any(|(k, id)| k == "anim_graph" && *id == linked_id));
}

// ---- diagnostics and sharing --------------------------------------------------------------------------------------------

#[test]
fn compile_failures_name_the_node_and_a_repair_and_no_graph_is_produced() {
    let mut f = Fixture::new();
    let a = f.slide("A", 4.0, 0.0, 4.0);
    // missing sequence
    let missing = f.graph(&[], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js("11111111-1111-4111-8111-111111111111"))]);
        b.wire("p", "out", "pose");
    });
    let out = f.try_compile(&missing);
    assert!(out.graph.is_none());
    let d = out.diagnostics.iter().find(|d| d.code == "graph.sequence_unavailable").expect("reported");
    assert!(d.location.as_deref().is_some_and(|l| l.contains("p")) && d.repair.is_some(), "{d:?}");
    // a sequence for another skeleton
    let (other, _) = source_skeleton();
    let other_id = other.id.as_str().to_string();
    f.src.skeletons.insert(other_id.clone(), Arc::new(other));
    let mut foreign = clip_asset("foreign", 1.0, vec![]);
    foreign.skeleton = Some(crate::ids::SkeletonId::parse(&other_id).unwrap());
    let foreign_id = foreign.id.as_str().to_string();
    f.src.sequences.insert(foreign_id.clone(), Arc::new(foreign));
    let mismatch = f.graph(&[], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&foreign_id))]);
        b.wire("p", "out", "pose");
    });
    assert!(f.try_compile(&mismatch).diagnostics.iter().any(|d| d.code == "graph.skeleton_mismatch"));
    // validator errors stop compilation
    let unwired = f.graph(&[], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&a))]);
    });
    let out = f.try_compile(&unwired);
    assert!(out.graph.is_none() && out.diagnostics.iter().any(|d| d.code == "graph.unconnected_input"));
    // a graph that links itself
    let mut selfish = f.graph(&[], |b| {
        b.node("lg", "anim.linked_graph", &[]);
        b.wire("lg", "out", "pose");
    });
    let id = selfish.id.as_str().to_string();
    selfish.main.nodes.get_mut("lg").unwrap().properties.insert("graph".into(), js(&id));
    f.src.graphs.insert(id.clone(), Arc::new(selfish.clone()));
    assert!(f.try_compile(&selfish).diagnostics.iter().any(|d| d.code == "graph.self_link"));
    // two graphs that link each other are caught by the compiler
    let mut g1 = f.graph(&[], |b| {
        b.node("lg", "anim.linked_graph", &[]);
        b.wire("lg", "out", "pose");
    });
    let mut g2 = g1.clone();
    g2.id = pulsar_reflection::AssetRef::new();
    let (id1, id2) = (g1.id.as_str().to_string(), g2.id.as_str().to_string());
    g1.main.nodes.get_mut("lg").unwrap().properties.insert("graph".into(), js(&id2));
    g2.main.nodes.get_mut("lg").unwrap().properties.insert("graph".into(), js(&id1));
    f.src.graphs.insert(id1, Arc::new(g1.clone()));
    f.src.graphs.insert(id2, Arc::new(g2));
    assert!(f.try_compile(&g1).diagnostics.iter().any(|d| d.code == "graph.linked_cycle"), "{:#?}", f.try_compile(&g1).diagnostics);
}

#[test]
fn the_typed_parameter_api_validates_names_types_and_values() {
    use crate::runtime::graph::ParamError;
    let mut f = Fixture::new();
    let a = f.slide("A", 4.0, 0.0, 4.0);
    let asset = f.graph(&[("speed", ParamType::Float), ("grounded", ParamType::Bool), ("n", ParamType::Int), ("dir", ParamType::Vector)], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&a))]);
        b.wire("p", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    assert_eq!(inst.set_float("nope", 1.0), Err(ParamError::Unknown("nope".into())));
    assert!(matches!(inst.set_bool("speed", true), Err(ParamError::WrongType { .. })));
    assert!(matches!(inst.set_int("speed", 1), Err(ParamError::WrongType { .. })));
    assert_eq!(inst.set_float("speed", f32::NAN), Err(ParamError::NotFinite("speed".into())));
    assert_eq!(inst.set_vector("dir", [0.0, f32::INFINITY, 0.0]), Err(ParamError::NotFinite("dir".into())));
    inst.set_float("speed", 3.0).unwrap();
    inst.set_bool("grounded", true).unwrap();
    inst.set_int("n", 4).unwrap();
    inst.set_vector("dir", [1.0, 2.0, 3.0]).unwrap();
    assert_eq!(inst.parameter("speed"), Some(&Value::Float(3.0)));
    let listed: Vec<_> = inst.parameters().map(|(n, t, _)| (n.to_string(), t)).collect();
    assert_eq!(listed.len(), 4);
    assert!(listed.contains(&("n".to_string(), ParamType::Int)));
}

#[test]
fn two_components_share_compiled_data_and_keep_independent_state() {
    let mut f = Fixture::new();
    let a = f.slide("A", 100.0, 0.0, 100.0);
    let asset = f.graph(&[("rate", ParamType::Float)], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&a))]);
        b.node("r", "anim.get_parameter", &[("parameter", js("rate"))]);
        b.wire("r", "p", "play_rate").wire("p", "out", "pose");
    });
    let graph = f.compile(&asset);
    let mut one = AnimInstance::new(f.skel.clone());
    let mut two = AnimInstance::new(f.skel.clone());
    one.set_graph(graph.clone()).unwrap();
    two.set_graph(graph.clone()).unwrap();
    assert!(Arc::ptr_eq(one.graph().unwrap(), two.graph().unwrap()), "one compiled graph for both");
    one.set_float("rate", 1.0).unwrap();
    two.set_float("rate", 3.0).unwrap();
    for _ in 0..3 {
        step(&mut one, 1.0);
        step(&mut two, 1.0);
    }
    assert!(near(x1(&one), 3.0) && near(x1(&two), 9.0), "{} {}", x1(&one), x1(&two));
    // montages are per instance too
    let m = crate::runtime::montage::tests::montage(&f.skel, "m", "S", "b2", None);
    one.play_montage(m, 1.0, None).unwrap();
    assert_eq!(one.montages.instances().len(), 1);
    assert!(two.montages.instances().is_empty());
}

#[test]
fn every_catalogue_node_has_an_evaluator() {
    // Lowering has no arm for a type the catalogue lists: compile a one-node graph of each
    // evaluable value node, and check the evaluator names every pose node type.
    let src = include_str!("compile.rs");
    for def in crate::anim_graph::catalogue::CATALOGUE {
        if def.type_id.starts_with("sm.") {
            continue; // lowered by the state machine compiler, not as program nodes
        }
        assert!(src.contains(&format!("\"{}\"", def.type_id)), "no evaluator for {}", def.type_id);
    }
}

// ---- value nodes and determinism ------------------------------------------------------------

#[test]
fn math_logic_and_vector_value_nodes_compute_what_the_catalogue_says() {
    let mut f = Fixture::new();
    let a = f.slide("A", 10.0, 0.0, 0.0);
    let c = f.slide("C", 10.0, 100.0, 100.0);
    let params = [("x", ParamType::Float), ("n", ParamType::Int), ("p", ParamType::Bool), ("q", ParamType::Bool)];
    // alpha = clamp(x * 2 + int_to_float(n), 0, 1)
    let asset = f.graph(&params, |b| {
        b.node("a", "anim.sequence_player", &[("sequence", js(&a))]);
        b.node("c", "anim.sequence_player", &[("sequence", js(&c))]);
        b.node("x", "anim.get_parameter", &[("parameter", js("x"))]);
        b.node("n", "anim.get_parameter", &[("parameter", js("n"))]);
        b.node("mul", "math.float_op", &[("op", js("mul")), ("b", json!(2.0))]);
        b.node("itf", "math.int_to_float", &[]);
        b.node("sum", "math.float_op", &[("op", js("add"))]);
        b.node("clamp", "math.clamp", &[("min", json!(0.0)), ("max", json!(1.0))]);
        b.node("blend", "anim.blend", &[("curve", js("linear"))]);
        b.wire("x", "mul", "a").wire("n", "itf", "value").wire("mul", "sum", "a").wire("itf", "sum", "b").wire("sum", "clamp", "value");
        b.wire("a", "blend", "a").wire("c", "blend", "b").wire("clamp", "blend", "alpha").wire("blend", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    for (x, n, expect) in [(0.25, 0, 50.0), (0.25, 1, 100.0), (-3.0, 0, 0.0), (0.1, 0, 20.0)] {
        inst.set_float("x", x).unwrap();
        inst.set_int("n", n).unwrap();
        step(&mut inst, 0.01);
        assert!(near(x1(&inst), expect), "x={x} n={n}: {}", x1(&inst));
    }
    // condition = (p or q) and not q  ==  p and not q
    let asset = f.graph(&params, |b| {
        b.node("a", "anim.sequence_player", &[("sequence", js(&a))]);
        b.node("c", "anim.sequence_player", &[("sequence", js(&c))]);
        for n in ["p", "q", "q2"] {
            b.node(n, "anim.get_parameter", &[("parameter", js(if n == "q2" { "q" } else { n }))]);
        }
        b.node("or", "logic.or", &[]).node("not", "logic.not", &[]).node("and", "logic.and", &[]);
        b.node("sw", "anim.blend_by_bool", &[("blend_time", json!(0.0))]);
        b.wire("p", "or", "a").wire("q", "or", "b").wire("q2", "not", "value").wire("or", "and", "a").wire("not", "and", "b");
        b.wire("c", "sw", "when_true").wire("a", "sw", "when_false").wire("and", "sw", "condition").wire("sw", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    for (p, q, expect) in [(false, false, 0.0), (true, false, 100.0), (true, true, 0.0), (false, true, 0.0)] {
        inst.set_bool("p", p).unwrap();
        inst.set_bool("q", q).unwrap();
        step(&mut inst, 0.01);
        assert!(near(x1(&inst), expect), "p={p} q={q}: {}", x1(&inst));
    }
    // vectors and transforms: a constant vector and make_vector feed an IK target
    let rest = f.seq("rest", 10.0, vec![]);
    let asset = f.graph(&[("h", ParamType::Float)], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&rest))]);
        b.node("h", "anim.get_parameter", &[("parameter", js("h"))]);
        b.node("v", "math.make_vector", &[("x", json!(1.0)), ("z", json!(0.0))]);
        b.node("mk", "anim.make_transform", &[]);
        b.node("ik", "anim.two_bone_ik", &[("end_bone", js("b2"))]);
        b.wire("h", "v", "y").wire("v", "mk", "location").wire("p", "ik", "pose").wire("mk", "ik", "effector").wire("ik", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    inst.set_float("h", 1.0).unwrap();
    step(&mut inst, 0.01);
    let want = Vec3::new(1.0, 1.0, 0.0);
    assert!((tip(&inst, 2) - want).length() < 5e-3, "{}", tip(&inst, 2));
}

/// Run a state machine, blend space, IK and Montage graph twice with identical
/// inputs: every pose of every update is bit-for-bit equal.
#[test]
fn a_whole_graph_is_deterministic_update_by_update() {
    let mut f = Fixture::new();
    let idle = f.slide("idle", 3.0, 0.0, 1.0);
    let walk = f.slide("walk", 2.0, 10.0, 14.0);
    let space = blend_space_asset(&mut f, true, SyncMode::Phase, &[(0.0, 0.0, 0.0, 4.0), (10.0, 0.0, 100.0, 2.0), (0.0, 10.0, 200.0, 3.0)]);
    let params = [("go", ParamType::Bool), ("x", ParamType::Float), ("y", ParamType::Float), ("t", ParamType::Vector)];
    let mut asset = machine_asset(
        &f,
        &params,
        &[("Idle", &idle, vec![]), ("Walk", &walk, vec![])],
        &[t("go", "Idle", "Walk", "go", vec![("duration", json!(0.7)), ("interrupt", js("both"))])],
    );
    // main: machine -> two bone IK (target from `t`) ; blend space -> layered over it
    let mut main = GraphBuilder::new("main", &params);
    main.node("out", "anim.output_pose", &[]);
    main.node("sm", "anim.state_machine", &[("machine", js("m"))]);
    main.node("bs", "anim.blend_space_player", &[("blend_space", js(&space))]);
    main.node("px", "anim.get_parameter", &[("parameter", js("x"))]).node("py", "anim.get_parameter", &[("parameter", js("y"))]);
    main.node("tv", "anim.get_parameter", &[("parameter", js("t"))]).node("mk", "anim.make_transform", &[]);
    main.node("ik", "anim.two_bone_ik", &[("end_bone", js("b2"))]);
    main.node("lb", "anim.layered_blend_per_bone", &[("count", json!(1)), ("masks", json!([{ "branches": [{ "bone": "b2", "weight": 1.0 }], "default_weight": 0.0 }]))]);
    main.node("half", "math.constant_float", &[("value", json!(0.5))]);
    main.wire("px", "bs", "x").wire("py", "bs", "y").wire("tv", "mk", "location");
    main.wire("sm", "ik", "pose").wire("mk", "ik", "effector");
    main.wire("ik", "lb", "base").wire("bs", "lb", "layer_0").wire("half", "lb", "layer_weight_0").wire("lb", "out", "pose");
    asset.main = main.into_graph();
    let run = || {
        let mut inst = f.instance(&asset);
        let mut poses = Vec::new();
        for i in 0..40 {
            inst.set_bool("go", (10..25).contains(&i)).unwrap();
            inst.set_float("x", (i as f32 * 0.37).sin().abs() * 10.0).unwrap();
            inst.set_float("y", (i as f32 * 0.21).cos().abs() * 10.0).unwrap();
            inst.set_vector("t", [1.0, (i as f32 * 0.1).sin() + 1.0, 0.0]).unwrap();
            step(&mut inst, 0.05 + (i % 3) as f32 * 0.01);
            poses.push(((**inst.pose()).clone(), inst.report.root_motion));
        }
        poses
    };
    let (a, b) = (run(), run());
    assert_eq!(a.len(), b.len());
    for (i, (pa, pb)) in a.iter().zip(&b).enumerate() {
        assert_eq!(pa.0.bones, pb.0.bones, "update {i}");
        assert_eq!(pa.0.curves, pb.0.curves, "update {i}");
        assert_eq!(pa.1, pb.1, "update {i}");
        assert!(pa.0.is_finite(), "update {i}");
    }
}

// ---- Phase 3: reverse playback, notifies of graph players, root motion modes -----------------

fn reverse_player_graph(f: &Fixture, seq: &str, looping: &str, start: f32) -> AnimationGraphAsset {
    f.graph(&[], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(seq)), ("play_rate", json!(-1.0)), ("looping", js(looping)), ("start_position", json!(start))]);
        b.wire("p", "out", "pose");
    })
}

#[test]
fn a_sequence_player_with_a_negative_rate_plays_backwards_loops_and_holds_at_the_start() {
    let mut f = Fixture::new();
    let a = f.slide("A", 4.0, 0.0, 4.0);
    let mut inst = f.instance(&reverse_player_graph(&f, &a, "loop", 3.0));
    step(&mut inst, 1.0);
    assert!(near(x1(&inst), 2.0), "{}", x1(&inst));
    step(&mut inst, 1.5);
    assert!(near(x1(&inst), 0.5), "{}", x1(&inst));
    step(&mut inst, 1.0);
    assert!(near(x1(&inst), 3.5), "wrapped below zero: {}", x1(&inst));

    let mut once = f.instance(&reverse_player_graph(&f, &a, "once", 3.0));
    step(&mut once, 10.0);
    assert!(near(x1(&once), 0.0), "a one-shot holds its first frame going backwards: {}", x1(&once));
}

#[test]
fn reverse_root_motion_is_the_inverse_of_forward_across_loop_seams() {
    let mut f = Fixture::new();
    let mut walk = clip_asset("walk", 1.0, vec![translate_track("b0", [0.0; 3], [0.0, 0.0, 4.0], 1.0)]);
    walk.skeleton = Some(crate::ids::SkeletonId::parse(&f.skeleton_id).unwrap());
    walk.features.root_motion.enabled = true;
    let id = walk.id.as_str().to_string();
    f.src.sequences.insert(id.clone(), Arc::new(walk));
    let mut inst = f.instance(&reverse_player_graph(&f, &id, "loop", 0.5));
    let mut total = crate::runtime::pose::RootMotion::IDENTITY;
    for _ in 0..3 {
        step(&mut inst, 0.75);
        total = total.then(&inst.report.root_motion);
    }
    assert!(near(total.translation.z, -4.0 * 2.25), "2.25 s backwards over two seams: {:?}", total.translation);
}

#[test]
fn a_blend_space_with_a_negative_rate_plays_its_samples_backwards() {
    let mut f = Fixture::new();
    let space = blend_space_asset(&mut f, false, SyncMode::None, &[(0.0, 0.0, 0.0, 4.0), (10.0, 0.0, 100.0, 4.0)]);
    let asset = f.graph(&[], |b| {
        b.node("bs", "anim.blend_space_player", &[("blend_space", js(&space)), ("play_rate", json!(-1.0)), ("looping", js("loop"))]);
        b.wire("bs", "out", "pose");
    });
    let mut inst = f.instance(&asset);
    step(&mut inst, 1.0);
    // x defaults to 0: only the first sample, 1 s before its start wraps to 3 s
    assert!(near(x1(&inst), 3.0), "{}", x1(&inst));
}

#[test]
fn graph_players_raise_notifies_and_states_in_either_direction_once() {
    use crate::notifies::{AnimationNotify, NotifyTarget};
    use crate::notify_schema::AnimationNotifyState;
    use crate::runtime::playback::{NotifyPhase, NotifyTracker};
    let mut f = Fixture::new();
    let mut asset = clip_asset("A", 4.0, vec![translate_track("b1", [0.0, 1.0, 0.0], [4.0, 1.0, 0.0], 4.0)]);
    asset.skeleton = Some(crate::ids::SkeletonId::parse(&f.skeleton_id).unwrap());
    asset.features.notifies.push(AnimationNotify { id: "n".into(), name: "n".into(), time: 3.0, target: NotifyTarget::Self_, fields: vec![], schema: None });
    asset.features.notify_states.push(AnimationNotifyState {
        id: "s".into(),
        name: "s".into(),
        start: 1.0,
        end: 2.0,
        target: NotifyTarget::Self_,
        schema: None,
        fields: vec![],
        tick_interval: None,
        disabled: false,
    });
    let id = asset.id.as_str().to_string();
    f.src.sequences.insert(id.clone(), Arc::new(asset));
    let mut inst = f.instance(&reverse_player_graph(&f, &id, "once", 3.5));
    let mut tracker = NotifyTracker::new();
    let mut out = Vec::new();
    for _ in 0..8 {
        inst.advance(0.5, TimeStep::Variable, UpdateMode::Play);
        tracker.observe_report(&inst.report, &mut out).unwrap();
    }
    let order: Vec<String> = out.iter().filter(|e| e.phase != NotifyPhase::Tick).map(|e| format!("{}:{}", e.id(), e.phase.name())).collect();
    assert_eq!(order, ["n:notify", "s:begin", "s:end"]);
    assert_eq!(tracker.active_states(), 0);
    assert_eq!(out[0].owner.kind(), "animseq");
}

#[test]
fn no_root_motion_extraction_leaves_the_travel_in_the_pose() {
    let mut f = Fixture::new();
    let mut walk = clip_asset("walk", 1.0, vec![translate_track("b0", [0.0; 3], [0.0, 0.0, 4.0], 1.0)]);
    walk.skeleton = Some(crate::ids::SkeletonId::parse(&f.skeleton_id).unwrap());
    walk.features.root_motion.enabled = true;
    let id = walk.id.as_str().to_string();
    f.src.sequences.insert(id.clone(), Arc::new(walk));
    let asset = f.graph(&[], |b| {
        b.node("p", "anim.sequence_player", &[("sequence", js(&id))]);
        b.wire("p", "out", "pose");
    });
    let mut extracting = f.instance(&asset);
    step(&mut extracting, 0.5);
    assert!(near(extracting.report.root_motion.translation.z, 2.0));
    assert!(extracting.pose().bones[0].translation.z.abs() < 1e-3);
    let mut in_pose = f.instance(&asset);
    in_pose.set_root_motion_extraction(false);
    step(&mut in_pose, 0.5);
    assert!(in_pose.report.root_motion.is_identity(), "nothing extracted");
    assert!(near(in_pose.pose().bones[0].translation.z, 2.0), "the root bone carries the travel itself: {}", in_pose.pose().bones[0].translation.z);
}

// ---- named skeleton masks ----------------------------------------------------------------

fn layered_with_masks(f: &mut Fixture, base: &str, layer: &str, masks: Json) -> AnimationGraphAsset {
    f.graph(&[("w", ParamType::Float)], |b| {
        b.node("base", "anim.sequence_player", &[("sequence", js(base))]);
        b.node("layer", "anim.sequence_player", &[("sequence", js(layer))]);
        b.node("w", "anim.get_parameter", &[("parameter", js("w"))]);
        b.node("lb", "anim.layered_blend_per_bone", &[("count", json!(1)), ("masks", masks)]);
        b.wire("base", "lb", "base").wire("layer", "lb", "layer_0").wire("w", "lb", "layer_weight_0").wire("lb", "out", "pose");
    })
}

fn add_library_mask(f: &mut Fixture, name: &str, bone: &str) {
    use crate::bone_mask::{BoneMaskSpec, MaskBranch, NamedBoneMask};
    let mut asset = (*f.src.skeletons[&f.skeleton_id]).clone();
    asset.masks.push(NamedBoneMask { name: name.into(), spec: BoneMaskSpec { branches: vec![MaskBranch { bone: bone.into(), depth: None, weight: 1.0 }], ..Default::default() } });
    f.src.skeletons.insert(f.skeleton_id.clone(), Arc::new(asset));
}

#[test]
fn a_layered_blend_layer_can_follow_a_named_skeleton_mask() {
    let mut f = Fixture::new();
    let base = f.seq("base", 10.0, vec![translate_track("b1", [1.0, 1.0, 0.0], [1.0, 1.0, 0.0], 10.0), translate_track("b2", [1.0, 1.0, 0.0], [1.0, 1.0, 0.0], 10.0)]);
    let layer = f.seq("layer", 10.0, vec![translate_track("b1", [9.0, 1.0, 0.0], [9.0, 1.0, 0.0], 10.0), translate_track("b2", [9.0, 1.0, 0.0], [9.0, 1.0, 0.0], 10.0)]);
    add_library_mask(&mut f, "Tip", "b2");
    let asset = layered_with_masks(&mut f, &base, &layer, json!([{ "profile": "Tip" }]));
    let mut inst = f.instance(&asset);
    inst.set_float("w", 1.0).unwrap();
    step(&mut inst, 0.1);
    assert!(near(inst.pose().bones[1].translation.x, 1.0), "b1 is outside the named mask");
    assert!(near(inst.pose().bones[2].translation.x, 9.0), "b2 is inside the named mask");
}

#[test]
fn an_unknown_named_mask_is_a_compile_error_with_a_repair() {
    let mut f = Fixture::new();
    let base = f.slide("base", 2.0, 0.0, 1.0);
    let asset = layered_with_masks(&mut f, &base, &base, json!([{ "profile": "Missing" }]));
    let out = f.try_compile(&asset);
    let d = out.diagnostics.iter().find(|d| d.code == "graph.mask_profile").expect("diagnostic");
    assert!(d.message.contains("Missing"));
}

#[test]
fn promoting_inline_layer_masks_keeps_the_pose_and_moves_the_rule_to_the_skeleton() {
    let mut f = Fixture::new();
    let base = f.seq("base", 10.0, vec![translate_track("b1", [1.0, 1.0, 0.0], [1.0, 1.0, 0.0], 10.0), translate_track("b2", [1.0, 1.0, 0.0], [1.0, 1.0, 0.0], 10.0)]);
    let layer = f.seq("layer", 10.0, vec![translate_track("b1", [9.0, 1.0, 0.0], [9.0, 1.0, 0.0], 10.0), translate_track("b2", [9.0, 1.0, 0.0], [9.0, 1.0, 0.0], 10.0)]);
    let mut asset = layered_with_masks(&mut f, &base, &layer, json!([{ "branches": [{ "bone": "b2", "weight": 1.0 }], "default_weight": 0.0 }]));
    let mut library = Vec::new();
    let used = crate::mask_library::promote_graph_masks(&mut asset, &mut library);
    assert_eq!(used.len(), 1);
    assert_eq!(library.len(), 1);
    assert_eq!(asset.main.nodes["lb"].properties["masks"][0]["profile"], json!(used[0]));
    let mut skeleton = (*f.src.skeletons[&f.skeleton_id]).clone();
    skeleton.masks = library;
    f.src.skeletons.insert(f.skeleton_id.clone(), Arc::new(skeleton));
    let mut inst = f.instance(&asset);
    inst.set_float("w", 1.0).unwrap();
    step(&mut inst, 0.1);
    assert!(near(inst.pose().bones[1].translation.x, 1.0));
    assert!(near(inst.pose().bones[2].translation.x, 9.0));
}

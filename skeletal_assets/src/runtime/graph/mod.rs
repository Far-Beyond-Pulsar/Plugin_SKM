//! Compiled animation graphs and their per-instance state.
//!
//! [`compile::compile_graph`] validates an `.animgraph`, resolves every asset
//! it names and lowers each Graphy document into an immutable [`Program`]
//! (nodes in a fixed order with resolved input links and typed configuration).
//! A [`CompiledGraph`] is shared by every component that plays it. All mutable
//! state lives in [`instance::GraphInstance`] values owned by the component.

pub mod compile;
pub mod instance;
pub mod machine;

use std::sync::Arc;

use glam::Vec3;

use crate::diagnostics::Diagnostic;
use crate::montage::BlendCurve;
use crate::anim_graph::catalogue::{GraphKind, ParamType};

use super::blendspace::CompiledBlendSpace;
use super::clip::CompiledClip;
use super::ik::{Effector, TwoBoneParams};
use super::ikrig::CompiledIkRig;
use super::montage::MontageSet;
use super::posesearch::{MirrorTable, PoseIndex, SearchClip};
use super::pose::RuntimeSkeleton;
use super::retarget::RetargetPlan;

/// A typed value on a data wire.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Float(f32),
    Bool(bool),
    Int(i32),
    Vector(Vec3),
    Transform(Effector),
}

impl Value {
    /// A vector value from plain numbers.
    pub fn vector(v: [f32; 3]) -> Self {
        Self::Vector(Vec3::from_array(v))
    }
    pub fn as_f32(&self) -> f32 {
        match self {
            Self::Float(v) => *v,
            Self::Int(v) => *v as f32,
            Self::Bool(b) => *b as u8 as f32,
            _ => 0.0,
        }
    }
    pub fn as_bool(&self) -> bool {
        matches!(self, Self::Bool(true))
    }
    pub fn as_i32(&self) -> i32 {
        match self {
            Self::Int(v) => *v,
            Self::Float(v) => *v as i32,
            _ => 0,
        }
    }
    pub fn as_vec3(&self) -> Vec3 {
        match self {
            Self::Vector(v) => *v,
            _ => Vec3::ZERO,
        }
    }
    pub fn as_effector(&self) -> Option<Effector> {
        match self {
            Self::Transform(e) => Some(*e),
            _ => None,
        }
    }
}

/// Why a parameter write was refused.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamError {
    Unknown(String),
    WrongType { name: String, expected: ParamType },
    NotFinite(String),
}

impl std::fmt::Display for ParamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(n) => write!(f, "the graph has no parameter named `{n}`"),
            Self::WrongType { name, expected } => write!(f, "parameter `{name}` is {expected:?}"),
            Self::NotFinite(n) => write!(f, "parameter `{n}` must be finite"),
        }
    }
}

impl std::error::Error for ParamError {}

#[derive(Clone, Debug)]
pub struct ParamDef {
    pub name: String,
    pub ty: ParamType,
    pub default: Value,
}

/// Current parameter values of one instance, indexed like the graph's `params`.
#[derive(Clone, Debug, Default)]
pub struct ParamValues {
    pub values: Vec<Value>,
}

impl ParamValues {
    pub fn from_defs(defs: &[ParamDef]) -> Self {
        Self { values: defs.iter().map(|d| d.default.clone()).collect() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncRole {
    Auto,
    Leader,
    Follower,
}

#[derive(Clone, Debug)]
pub struct SyncCfg {
    pub group: String,
    pub role: SyncRole,
}

#[derive(Clone, Debug)]
pub struct SeqCfg {
    pub clip: Arc<CompiledClip>,
    pub rate: f32,
    pub start: f32,
    pub looping: bool,
    pub sync: Option<SyncCfg>,
}

#[derive(Clone, Debug)]
pub struct SpaceCfg {
    pub space: Arc<CompiledBlendSpace>,
    pub x: f32,
    pub y: f32,
    pub rate: f32,
    pub looping: bool,
    pub sync: Option<SyncCfg>,
}

/// A pose database ready to query.
#[derive(Debug)]
pub struct CompiledPoseDb {
    pub asset: crate::pose_db::PoseDbAsset,
    pub clips: Vec<SearchClip>,
    pub index: PoseIndex,
    pub mirror: Option<MirrorTable>,
    /// Whether the index came from the derived cache instead of a build.
    pub from_cache: bool,
}

#[derive(Clone, Debug)]
pub struct SearchCfg {
    pub db: Option<Arc<CompiledPoseDb>>,
    pub interval: f32,
    pub blend_time: f32,
    pub continuing_bias: f32,
    pub debug: bool,
    pub speed: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectorSpace {
    Mesh,
    Bone,
}

#[derive(Clone, Debug)]
pub struct TwoBoneCfg {
    pub end: usize,
    pub alpha: f32,
    pub space: EffectorSpace,
    pub params: TwoBoneParams,
}

#[derive(Clone, Debug)]
pub struct FabrikCfg {
    pub chain: Vec<usize>,
    pub alpha: f32,
    pub precision: f32,
    pub max_iterations: u32,
}

#[derive(Clone, Debug)]
pub struct IkRigCfg {
    pub rig: Arc<CompiledIkRig>,
    /// `(goal index in the rig, input index on the node)`.
    pub goal_inputs: Vec<(usize, usize)>,
    pub alpha: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveBlendMode {
    Base,
    Layer,
    Max,
    Sum,
}

#[derive(Clone, Debug)]
pub struct LayeredCfg {
    pub layers: usize,
    pub masks: Vec<Option<Vec<f32>>>,
    pub mesh_space_rotation: bool,
    pub curves: CurveBlendMode,
    pub root_motion_from_layer0: bool,
}

#[derive(Clone, Debug)]
pub struct SlotCfg {
    pub slot: String,
    pub group: String,
    pub always_update_source: bool,
}

#[derive(Clone, Debug)]
pub struct LinkedCfg {
    pub graph: Arc<CompiledGraph>,
    /// `(index in the linked graph, index in this graph)`.
    pub param_map: Vec<(usize, usize)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    AbsA,
    Pow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareOp {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
    Ne,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveEditMode {
    Set,
    Add,
    Scale,
}

#[derive(Clone, Debug)]
pub enum NodeKind {
    SequencePlayer(SeqCfg),
    BlendSpacePlayer(SpaceCfg),
    PoseSearch(SearchCfg),
    ReferencePose,
    Blend { alpha: f32, curve: BlendCurve },
    BlendMulti { count: usize },
    BlendByBool { condition: bool, blend_time: f32, curve: BlendCurve },
    BlendByInt { count: usize, selector: i32, blend_time: f32 },
    MakeAdditive { mesh_space: bool },
    ApplyAdditive { alpha: f32 },
    LayeredBlend(LayeredCfg),
    Slot(SlotCfg),
    SaveCachedPose,
    UseCachedPose { save: usize },
    GetCurve { curve: String },
    ModifyCurve { curve: String, value: f32, mode: CurveEditMode },
    TwoBoneIk(TwoBoneCfg),
    Fabrik(FabrikCfg),
    IkRig(IkRigCfg),
    Retarget { plan: Arc<RetargetPlan>, alpha: f32 },
    StateMachine(Arc<machine::Machine>),
    Layer { program: Arc<Program> },
    LinkedGraph(LinkedCfg),
    OutputPose,
    LayerInput,
    LayerOutput,
    StateResult,
    GetParameter { index: usize },
    ConstFloat(f32),
    ConstBool(bool),
    ConstVector(Vec3),
    MakeVector { x: f32, y: f32, z: f32 },
    MakeTransform { location: Vec3, rotation: Vec3 },
    GetBoneTransform { bone: usize, mesh_space: bool },
    FloatOp { op: FloatOp, a: f32, b: f32 },
    Clamp { min: f32, max: f32 },
    IntToFloat,
    Compare { op: CompareOp, b: f32, tolerance: f32 },
    And,
    Or,
    Not,
    StateTime,
    TimeRemaining,
    StateComplete,
    RuleResult,
}

#[derive(Clone, Debug)]
pub struct CNode {
    /// The Graphy node id, for traces and diagnostics.
    pub id: String,
    pub kind: NodeKind,
    /// Source node of each input pin, in the catalogue's pin order for the
    /// node's properties. `None` for an unconnected optional input.
    pub inputs: Vec<Option<usize>>,
    /// Skeleton the node's poses are over.
    pub skeleton: Arc<RuntimeSkeleton>,
}

#[derive(Clone, Debug)]
pub struct Program {
    pub kind: GraphKind,
    pub label: String,
    pub nodes: Vec<CNode>,
    pub terminal: usize,
    /// The player whose clock answers "time remaining" for transition rules.
    pub primary_player: Option<usize>,
}

/// An immutable, shareable graph.
#[derive(Clone, Debug)]
pub struct CompiledGraph {
    pub id: String,
    pub name: String,
    pub skeleton: Arc<RuntimeSkeleton>,
    pub main: Arc<Program>,
    pub params: Vec<ParamDef>,
    /// Warnings and notes from compilation.
    pub diagnostics: Vec<Diagnostic>,
    /// `(asset kind, asset id)` of every asset compiled in, for hot reload.
    pub dependencies: Vec<(String, String)>,
}

impl CompiledGraph {
    pub fn param_index(&self, name: &str) -> Option<usize> {
        self.params.iter().position(|p| p.name == name)
    }
}

/// One advance of a sequence clock, reported for notify and event consumers. A span with
/// `from == to` means the playback is live but did not move.

#[derive(Clone, Debug)]
pub struct PlaybackSpan {
    /// The graph instance (or Montage instance) the playing node belongs to: two
    /// instances of the same graph node never share a playback.
    pub scope: u64,
    pub clip: Arc<CompiledClip>,
    /// Play positions in unbounded clip seconds.
    pub from: f64,
    pub to: f64,
    pub looping: bool,
    /// Contribution weight of the producing node.
    pub weight: f32,
    pub origin: SpanOrigin,
    /// For a Montage segment that continues across its loop seam: the `(clip_start, clip_end)`
    /// window it loops over. A span that starts at the window start right after one that ended at
    /// its end is the same run, not a jump.
    pub seam: Option<(f64, f64)>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpanOrigin {
    SequencePlayer { node: String },
    BlendSpaceSample { node: String, sample: String },
    PoseSearch { node: String },
    Montage { montage: String, segment: String },
}

/// Cost breakdown of one Pose Search query, kept for the debugger.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchRecord {
    pub node: String,
    pub result: Option<super::posesearch::SearchResult>,
    pub used_fallback: bool,
}

/// What a transition rule can ask about its source state.
#[derive(Clone, Copy, Debug, Default)]
pub struct RuleContext {
    pub state_time: f32,
    pub time_remaining: f32,
    pub complete: bool,
}

/// What a state machine reports to its owner. Delivery is the owner's business (the component
/// publishes these on itself, never globally); the graph only records them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StateEventKind {
    /// A state became the machine's current state (at the start of the transition into it, or when the machine starts).
    Entered,
    /// A state stopped carrying weight: its transition blend finished.
    Exited,
    TransitionStarted,
    /// A transition's blend ran to the end. A transition replaced by another one in flight is not completed.
    TransitionCompleted,
}

impl StateEventKind {
    pub const ALL: [StateEventKind; 4] = [Self::Entered, Self::Exited, Self::TransitionStarted, Self::TransitionCompleted];

    /// The suffix used in canonical event names (`state_entered`, ...).
    pub fn name(self) -> &'static str {
        match self {
            Self::Entered => "entered",
            Self::Exited => "exited",
            Self::TransitionStarted => "transition_started",
            Self::TransitionCompleted => "transition_completed",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StateEvent {
    pub machine: String,
    pub kind: StateEventKind,
    /// The state entered or exited; for transitions, the state transitioned from.
    pub state: String,
    pub from_state: String,
    pub to_state: String,
    /// Id of the transition (empty for a machine starting up).
    pub transition: String,
}

/// Everything an update or evaluation reads from outside the graph.
pub struct Env<'a> {
    pub dt: f32,
    pub params: &'a ParamValues,
    pub montages: &'a MontageSet,
    pub sync: &'a mut instance::SyncRegistry,
    pub rule: Option<RuleContext>,
    pub spans: &'a mut Vec<PlaybackSpan>,
    pub searches: &'a mut Vec<SearchRecord>,
    /// State machine events of this update (empty when `quiet`).
    pub state_events: &'a mut Vec<StateEvent>,
    pub trace: &'a mut Vec<String>,
    /// Next instance id for nested instances created during the update.
    pub next_uid: &'a mut u64,
    /// When set, no playback spans are recorded (scrubbing never raises events).
    pub quiet: bool,
    /// `NoRootMotionExtraction`: the root bone's motion stays in the pose.
    pub root_motion_in_pose: bool,
}

#[cfg(test)]
mod tests;

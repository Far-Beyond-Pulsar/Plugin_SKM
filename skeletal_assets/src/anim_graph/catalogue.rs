//! The animation node and pin type catalogue.
//!
//! This is the data model of the animation Graphy profile: which node types
//! exist, their pins, their authored properties and their evaluation contract.
//! The profile (`skm_editor`) renders and edits from it; the compiler/evaluator
//! (Phase 2) lowers a validated document using the same definitions, so a node
//! cannot exist in one and not the other. Nothing here draws or evaluates.
//!
//! Pose flows on data wires. Every pin type maps to a Graphy `TypeInfo` string
//! (`anim::Pose`, `f32`, ...), so Graphy's own connection check applies.

use graphy_blueprint::{DataType, TypeInfo};
use pulsar_reflection::AssetKind;
use serde_json::Value;

/// A value that can travel on an animation wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnimPinType {
    /// A full local-space skeleton pose with curves and root motion.
    Pose,
    /// A pose that is a difference from a reference, to be applied on a base.
    AdditivePose,
    Float,
    Bool,
    Int,
    Vector,
    /// A translation/rotation/scale target, e.g. an IK effector.
    Transform,
    Name,
    /// Connects states and transitions inside a state machine graph.
    StateFlow,
}

impl AnimPinType {
    pub const fn type_name(self) -> &'static str {
        match self {
            Self::Pose => "anim::Pose",
            Self::AdditivePose => "anim::AdditivePose",
            Self::Float => "f32",
            Self::Bool => "bool",
            Self::Int => "i32",
            Self::Vector => "Vec3",
            Self::Transform => "anim::Transform",
            Self::Name => "Name",
            Self::StateFlow => "anim::StateFlow",
        }
    }

    pub fn from_type_name(name: &str) -> Option<Self> {
        [
            Self::Pose,
            Self::AdditivePose,
            Self::Float,
            Self::Bool,
            Self::Int,
            Self::Vector,
            Self::Transform,
            Self::Name,
            Self::StateFlow,
        ]
        .into_iter()
        .find(|t| t.type_name() == name)
    }

    pub fn data_type(self) -> DataType {
        DataType::Data(TypeInfo::parse(self.type_name()))
    }

    /// Whether a wire from `self` may end at an input of type `to`. Types are
    /// exact: no implicit numeric widening, and an additive pose is never a
    /// pose until an Apply Additive node says how strongly.
    pub fn connects_to(self, to: Self) -> bool {
        self == to
    }

    pub fn is_pose(self) -> bool {
        matches!(self, Self::Pose | Self::AdditivePose)
    }
}

/// The kind of value a graph parameter holds. A subset of pin types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamType {
    Float,
    Bool,
    Int,
    Vector,
}

impl ParamType {
    pub const fn pin_type(self) -> AnimPinType {
        match self {
            Self::Float => AnimPinType::Float,
            Self::Bool => AnimPinType::Bool,
            Self::Int => AnimPinType::Int,
            Self::Vector => AnimPinType::Vector,
        }
    }

    /// Whether `value` is a legal default for this type.
    pub fn accepts(self, value: &Value) -> bool {
        match self {
            Self::Float => value.as_f64().is_some_and(f64::is_finite),
            Self::Int => value.as_i64().is_some(),
            Self::Bool => value.is_boolean(),
            Self::Vector => value
                .as_array()
                .is_some_and(|a| a.len() == 3 && a.iter().all(|v| v.as_f64().is_some_and(f64::is_finite))),
        }
    }

    pub fn default_value(self) -> Value {
        match self {
            Self::Float => Value::from(0.0),
            Self::Int => Value::from(0),
            Self::Bool => Value::from(false),
            Self::Vector => serde_json::json!([0.0, 0.0, 0.0]),
        }
    }
}

/// The graph a node may be placed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphKind {
    /// The asset's main graph: ends in an Output Pose.
    Main,
    /// A reusable layer: Layer Input (optional) to Layer Output.
    Layer,
    /// A state machine: Entry, states, transitions.
    StateMachine,
    /// The pose a state produces: ends in State Result.
    StatePose,
    /// A transition rule: ends in Rule Result (Bool).
    TransitionRule,
}

impl GraphKind {
    /// Graphs whose nodes produce poses.
    pub fn is_pose_graph(self) -> bool {
        matches!(self, Self::Main | Self::Layer | Self::StatePose)
    }

    /// The node type that must appear exactly once to terminate this graph.
    pub fn terminal(self) -> &'static str {
        match self {
            Self::Main => "anim.output_pose",
            Self::Layer => "anim.layer_output",
            Self::StateMachine => "sm.entry",
            Self::StatePose => "anim.state_result",
            Self::TransitionRule => "rule.result",
        }
    }
}

const POSE: &[GraphKind] = &[GraphKind::Main, GraphKind::Layer, GraphKind::StatePose];
/// Pure value nodes usable in a pose graph and in a transition rule.
const VALUE: &[GraphKind] = &[
    GraphKind::Main,
    GraphKind::Layer,
    GraphKind::StatePose,
    GraphKind::TransitionRule,
];

#[derive(Clone, Copy, Debug)]
pub struct PinDef {
    pub name: &'static str,
    pub ty: AnimPinType,
    /// A required input must be connected. Optional data inputs fall back to
    /// the node property of the same name.
    pub required: bool,
    pub doc: &'static str,
}

const fn req(name: &'static str, ty: AnimPinType, doc: &'static str) -> PinDef {
    PinDef { name, ty, required: true, doc }
}
const fn opt(name: &'static str, ty: AnimPinType, doc: &'static str) -> PinDef {
    PinDef { name, ty, required: false, doc }
}

/// Which declared sub-graph a property selects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PropKind {
    Float { min: f32, max: f32 },
    Int { min: i64, max: i64 },
    Bool,
    Text,
    Enum(&'static [&'static str]),
    /// A bone id of the graph's skeleton.
    Bone,
    /// An animation curve name.
    Curve,
    /// A graph parameter name of the stated type.
    Parameter(AnimPinType),
    /// A slot name.
    Slot,
    /// A cached-pose name.
    CachedPose,
    /// A sync group name.
    SyncGroup,
    /// A typed asset reference; the string is the `AssetKind::KIND`.
    Asset(&'static str),
    /// An inline sub-graph of the given kind, by id.
    SubGraph(GraphKind),
    /// A `BoneMaskSpec` object.
    BoneMask,
    /// An array of bone ids.
    BoneList,
    /// An array of identifier names (IK goals, ...).
    NameList,
    Vector,
}

#[derive(Clone, Copy, Debug)]
pub struct PropDef {
    pub name: &'static str,
    pub kind: PropKind,
    pub required: bool,
    /// JSON text of the default value written into new nodes; `""` for none.
    pub default: &'static str,
    pub doc: &'static str,
}

const fn prop(name: &'static str, kind: PropKind, default: &'static str, doc: &'static str) -> PropDef {
    PropDef { name, kind, required: false, default, doc }
}
const fn need(name: &'static str, kind: PropKind, doc: &'static str) -> PropDef {
    PropDef { name, kind, required: true, default: "", doc }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootMotionFlow {
    None,
    /// Takes root motion from the first pose input unchanged.
    FromFirstInput,
    /// Weighted blend of the inputs' root motion by the blend weights.
    Blend,
    /// The node's own source supplies it (sequence, blend space, montage).
    FromSource,
    /// Discarded.
    Discard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveFlow {
    None,
    Passthrough,
    /// Curves blend by the same weights as the pose; absent curves count as 0.
    Blend,
    /// The node outputs the curves of its source asset.
    FromSource,
    /// The node reads or writes named curves.
    Modifies,
}

/// What a node does when evaluated, precisely enough that the Phase 2
/// evaluator has no semantic choices left.
#[derive(Clone, Copy, Debug)]
pub struct EvalContract {
    pub summary: &'static str,
    pub root_motion: RootMotionFlow,
    pub curves: CurveFlow,
    /// Contributes the notifies of its source asset(s) to the event stream.
    pub notifies: bool,
    /// Holds per-instance state (a clock, a state, a solver cache).
    pub stateful: bool,
    /// Needs the graph's skeleton to be set to evaluate.
    pub needs_skeleton: bool,
}

const fn eval(summary: &'static str, root_motion: RootMotionFlow, curves: CurveFlow) -> EvalContract {
    EvalContract { summary, root_motion, curves, notifies: false, stateful: false, needs_skeleton: false }
}
const fn stateful(mut c: EvalContract) -> EvalContract {
    c.stateful = true;
    c
}
const fn needs_skel(mut c: EvalContract) -> EvalContract {
    c.needs_skeleton = true;
    c
}

/// How a node's pins grow with a property.
#[derive(Clone, Copy, Debug)]
pub enum DynamicPins {
    None,
    /// `count` copies of the `(pose, weight)` pairs named `<stem>_<i>` /
    /// `<stem>_weight_<i>` after the fixed pins, `i` from 0.
    PoseWeightPairs { count_prop: &'static str, stem: &'static str, weight_stem: Option<&'static str> },
    /// One `Transform` input per name in a `NameList` property, named `goal_<name>`.
    GoalTransforms { list_prop: &'static str },
    /// The sole output has the type of the parameter named by `param_prop`.
    OutputOfParameter { param_prop: &'static str },
}

#[derive(Clone, Copy, Debug)]
pub struct NodeDef {
    pub type_id: &'static str,
    pub title: &'static str,
    pub category: &'static str,
    pub doc: &'static str,
    pub graphs: &'static [GraphKind],
    pub inputs: &'static [PinDef],
    pub outputs: &'static [PinDef],
    pub props: &'static [PropDef],
    pub dynamic: DynamicPins,
    pub eval: EvalContract,
    /// Per graph, at most this many of the node (0 = unlimited).
    pub max_per_graph: u32,
}

use AnimPinType as T;
use PropKind as P;
use RootMotionFlow as RM;
use CurveFlow as CF;

const SEQUENCE_KIND: &str = pulsar_reflection::AnimationSequenceAssetKind::KIND;
const BLEND_SPACE_KIND: &str = pulsar_reflection::BlendSpaceAssetKind::KIND;
const GRAPH_KIND: &str = pulsar_reflection::AnimationGraphAssetKind::KIND;
const POSEDB_KIND: &str = pulsar_reflection::PoseDatabaseAssetKind::KIND;
const IKRIG_KIND: &str = pulsar_reflection::IkRigAssetKind::KIND;
const RETARGET_KIND: &str = pulsar_reflection::RetargetProfileAssetKind::KIND;
const IKRETARGET_KIND: &str = pulsar_reflection::IkRetargeterAssetKind::KIND;

const LOOP_MODES: &[&str] = &["inherit", "loop", "once"];
const SYNC_ROLES: &[&str] = &["auto", "leader", "follower"];
const BLEND_CURVES: &[&str] = &["linear", "ease_in", "ease_out", "ease_in_out"];
const CURVE_BLEND: &[&str] = &["base", "layer", "max", "sum"];

const fn node(
    type_id: &'static str,
    title: &'static str,
    category: &'static str,
    doc: &'static str,
    graphs: &'static [GraphKind],
    inputs: &'static [PinDef],
    outputs: &'static [PinDef],
    props: &'static [PropDef],
    eval: EvalContract,
) -> NodeDef {
    NodeDef { type_id, title, category, doc, graphs, inputs, outputs, props, dynamic: DynamicPins::None, eval, max_per_graph: 0 }
}
const fn dynamic(mut n: NodeDef, d: DynamicPins) -> NodeDef {
    n.dynamic = d;
    n
}
const fn once(mut n: NodeDef) -> NodeDef {
    n.max_per_graph = 1;
    n
}

const POSE_OUT: &[PinDef] = &[req("pose", T::Pose, "The resulting pose")];
const NO_PINS: &[PinDef] = &[];

/// Every node type, in palette order.
pub static CATALOGUE: &[NodeDef] = &[
    // ---- sources ----
    notifying(stateful_node(node(
        "anim.sequence_player", "Sequence Player", "Sources",
        "Plays one animation sequence with its own clock.",
        POSE, &[opt("play_rate", T::Float, "Overrides the rate property"), opt("start_position", T::Float, "Seconds; read when the node becomes relevant")],
        POSE_OUT,
        &[
            need("sequence", P::Asset(SEQUENCE_KIND), "The sequence to play"),
            prop("play_rate", P::Float { min: 0.0, max: 16.0 }, "1.0", "Rate multiplier; the sign of reverse play is not supported"),
            prop("start_position", P::Float { min: 0.0, max: 1.0e6 }, "0.0", "Start time in seconds"),
            prop("looping", P::Enum(LOOP_MODES), "\"inherit\"", "Loop behaviour"),
            prop("sync_group", P::SyncGroup, "", "Sync group name, empty for none"),
            prop("sync_role", P::Enum(SYNC_ROLES), "\"auto\"", "Leader or follower inside the sync group"),
        ],
        eval("Samples the sequence at its clock, advancing by delta*rate; outputs its curves, root motion and notifies.", RM::FromSource, CF::FromSource),
    ))),
    notifying(stateful_node(node(
        "anim.blend_space_player", "Blend Space Player", "Sources",
        "Plays a blend space at the given x/y input.",
        POSE, &[opt("x", T::Float, "Falls back to the x property"), opt("y", T::Float, "2D only; falls back to the y property")],
        POSE_OUT,
        &[
            need("blend_space", P::Asset(BLEND_SPACE_KIND), "The blend space to play"),
            prop("x", P::Float { min: -1.0e6, max: 1.0e6 }, "0.0", "Default x input"),
            prop("y", P::Float { min: -1.0e6, max: 1.0e6 }, "0.0", "Default y input"),
            prop("play_rate", P::Float { min: 0.0, max: 16.0 }, "1.0", "Rate multiplier"),
            prop("looping", P::Enum(LOOP_MODES), "\"inherit\"", "Loop behaviour"),
            prop("sync_group", P::SyncGroup, "", "Sync group name, empty for none"),
            prop("sync_role", P::Enum(SYNC_ROLES), "\"auto\"", "Leader or follower inside the sync group"),
        ],
        eval("Solves normalised sample weights for the input, advances contributing samples (synced per the space), blends their poses, curves and root motion by weight.", RM::FromSource, CF::FromSource),
    ))),
    skeletal(stateful_node(node(
        "anim.pose_search", "Pose Search", "Sources",
        "Motion matching: picks the best pose in a pose database for the query and plays it.",
        POSE,
        &[opt("fallback", T::Pose, "Played when the database cannot be used"), opt("trajectory_speed", T::Float, "Desired speed for trajectory channels"), opt("trajectory_direction", T::Vector, "Desired direction for trajectory channels")],
        POSE_OUT,
        &[
            need("database", P::Asset(POSEDB_KIND), "The pose database"),
            prop("search_interval", P::Float { min: 0.0, max: 5.0 }, "0.1", "Seconds between searches; 0 searches every update"),
            prop("blend_time", P::Float { min: 0.0, max: 5.0 }, "0.2", "Seconds to blend to a newly chosen pose"),
            prop("continuing_bias", P::Float { min: 0.0, max: 1.0 }, "0.1", "Cost discount for staying in the current clip"),
            prop("query_debug", P::Bool, "false", "Record the cost breakdown for the debugger"),
            prop("trajectory_speed", P::Float { min: 0.0, max: 100.0 }, "0.0", "Default desired speed for trajectory channels"),
        ],
        eval("Every search interval builds a query from the current pose and trajectory inputs, finds the minimum-cost database pose, and blends to it; plays the chosen clip between searches; outputs the fallback pose if the database is missing or empty.", RM::FromSource, CF::FromSource),
    ))),
    node(
        "anim.reference_pose", "Reference Pose", "Sources",
        "The skeleton's bind pose.",
        POSE, NO_PINS, POSE_OUT, &[],
        needs_skel(eval("Outputs the bind pose with no curves and no root motion.", RM::None, CF::None)),
    ),
    // ---- blending ----
    node(
        "anim.blend", "Blend", "Blend",
        "Blends two poses by alpha.",
        POSE, &[req("a", T::Pose, "Pose at alpha 0"), req("b", T::Pose, "Pose at alpha 1"), opt("alpha", T::Float, "Falls back to the alpha property")],
        POSE_OUT,
        &[
            prop("alpha", P::Float { min: 0.0, max: 1.0 }, "0.5", "Default blend factor"),
            prop("curve", P::Enum(BLEND_CURVES), "\"linear\"", "Shaping of alpha"),
        ],
        eval("Per-bone lerp/slerp from a to b by the shaped alpha; curves and root motion blend by the same weight.", RM::Blend, CF::Blend),
    ),
    dynamic(node(
        "anim.blend_multi", "Blend Multi", "Blend",
        "Blends N poses by normalised weights.",
        POSE, NO_PINS, POSE_OUT,
        &[prop("count", P::Int { min: 2, max: 16 }, "3", "Number of inputs")],
        eval("Weights are normalised to sum to 1 (all-zero selects input 0); poses blend pairwise in input order.", RM::Blend, CF::Blend),
    ), DynamicPins::PoseWeightPairs { count_prop: "count", stem: "pose", weight_stem: Some("weight") }),
    node(
        "anim.blend_by_bool", "Blend by Bool", "Blend",
        "Chooses one of two poses, blending over a duration when the condition changes.",
        POSE, &[req("when_true", T::Pose, "Pose while the condition is true"), req("when_false", T::Pose, "Pose while the condition is false"), opt("condition", T::Bool, "Falls back to the condition property")],
        POSE_OUT,
        &[
            prop("condition", P::Bool, "false", "Default condition"),
            prop("blend_time", P::Float { min: 0.0, max: 5.0 }, "0.2", "Seconds to cross-fade"),
            prop("curve", P::Enum(BLEND_CURVES), "\"ease_in_out\"", "Cross-fade shaping"),
        ],
        stateful(eval("Cross-fades between the two inputs over blend_time when the condition changes; only the contributing inputs are evaluated.", RM::Blend, CF::Blend)),
    ),
    dynamic(node(
        "anim.blend_by_int", "Blend by Int", "Blend",
        "Chooses one of N poses by an integer, cross-fading when it changes.",
        POSE, &[opt("selector", T::Int, "Falls back to the selector property")], POSE_OUT,
        &[
            prop("count", P::Int { min: 2, max: 16 }, "3", "Number of inputs"),
            prop("selector", P::Int { min: 0, max: 15 }, "0", "Default selector"),
            prop("blend_time", P::Float { min: 0.0, max: 5.0 }, "0.2", "Seconds to cross-fade"),
        ],
        stateful(eval("Selector is clamped to the input range; cross-fades from the previous selection over blend_time.", RM::Blend, CF::Blend)),
    ), DynamicPins::PoseWeightPairs { count_prop: "count", stem: "pose", weight_stem: None }),
    node(
        "anim.make_additive", "Make Additive", "Additive",
        "Turns a pose into an additive pose relative to a reference.",
        POSE, &[req("source", T::Pose, "The pose to difference"), req("reference", T::Pose, "The pose it is measured from")],
        &[req("additive", T::AdditivePose, "The difference")],
        &[prop("space", P::Enum(&["local", "mesh"]), "\"local\"", "Space the difference is taken in")],
        needs_skel(eval("Per-bone delta of source from reference in the chosen space; curves are subtracted; root motion is discarded.", RM::Discard, CF::Modifies)),
    ),
    node(
        "anim.apply_additive", "Apply Additive", "Additive",
        "Adds an additive pose onto a base pose.",
        POSE, &[req("base", T::Pose, "The base pose"), req("additive", T::AdditivePose, "The additive pose"), opt("alpha", T::Float, "Falls back to the alpha property")],
        POSE_OUT,
        &[prop("alpha", P::Float { min: 0.0, max: 1.0 }, "1.0", "Strength of the additive")],
        needs_skel(eval("Per-bone base * lerp(identity, additive, alpha) in the additive's space; curves add scaled by alpha; root motion keeps the base's.", RM::FromFirstInput, CF::Blend)),
    ),
    dynamic(node(
        "anim.layered_blend_per_bone", "Layered Blend per Bone", "Blend",
        "Blends layer poses over a base pose through per-bone masks.",
        POSE, &[req("base", T::Pose, "The base pose")], POSE_OUT,
        &[
            prop("count", P::Int { min: 1, max: 8 }, "1", "Number of layers"),
            prop("masks", P::BoneMask, "", "One mask per layer, layer 0 first, as an array of BoneMaskSpec"),
            prop("mesh_space_rotation", P::Bool, "false", "Blend rotations in mesh space so layers do not inherit parent motion"),
            prop("curve_blend", P::Enum(CURVE_BLEND), "\"base\"", "Which curves win where layers overlap"),
            prop("root_motion_from", P::Enum(&["base", "layer0"]), "\"base\"", "Root motion owner"),
        ],
        needs_skel(eval("For each layer in order, bone weight = mask weight * layer weight; bones lerp/slerp from the accumulated pose toward the layer by that weight; curves follow curve_blend; root motion follows root_motion_from.", RM::FromFirstInput, CF::Blend)),
    ), DynamicPins::PoseWeightPairs { count_prop: "count", stem: "layer", weight_stem: Some("layer_weight") }),
    // ---- montage / cache ----
    node(
        "anim.slot", "Slot", "Montage",
        "The place montages play on this graph's pose.",
        POSE, &[req("source", T::Pose, "The pose when no montage is playing")], POSE_OUT,
        &[
            need("slot", P::Slot, "Slot name; montages with a track of this slot play here"),
            prop("group", P::Text, "\"DefaultGroup\"", "Slot group"),
            prop("always_update_source", P::Bool, "false", "Keep evaluating the source while a montage covers it fully"),
        ],
        stateful(eval("Blends the source with each playing montage track of this slot (in priority order) by track weight and mask; root motion follows the montage's root-motion setting.", RM::Blend, CF::Blend)),
    ),
    node(
        "anim.save_cached_pose", "Save Cached Pose", "Cache",
        "Evaluates its input once per update and stores it under a name.",
        POSE, &[req("pose", T::Pose, "The pose to store")], NO_PINS,
        &[need("name", P::CachedPose, "Cache name, unique in the graph")],
        eval("Evaluated at most once per update no matter how many Use Cached Pose nodes read it; the stored value is the full pose with curves and root motion.", RM::FromFirstInput, CF::Passthrough),
    ),
    node(
        "anim.use_cached_pose", "Use Cached Pose", "Cache",
        "Reads a pose stored by Save Cached Pose.",
        POSE, NO_PINS, POSE_OUT,
        &[need("name", P::CachedPose, "Cache name")],
        eval("Returns a copy of the saved pose from the current update.", RM::FromFirstInput, CF::Passthrough),
    ),
    // ---- curves ----
    node(
        "anim.get_curve", "Get Curve Value", "Curves",
        "Reads a named curve from a pose.",
        VALUE, &[req("pose", T::Pose, "The pose to read")], &[req("value", T::Float, "The curve value, 0 when absent")],
        &[need("curve", P::Curve, "Curve name")],
        eval("Returns the pose's curve value, or 0 when the pose carries none.", RM::None, CF::Modifies),
    ),
    node(
        "anim.modify_curve", "Modify Curve", "Curves",
        "Sets, adds to or scales a named curve on a pose.",
        POSE, &[req("pose", T::Pose, "The pose to modify"), opt("value", T::Float, "Falls back to the value property")], POSE_OUT,
        &[
            need("curve", P::Curve, "Curve name"),
            prop("value", P::Float { min: -1.0e6, max: 1.0e6 }, "0.0", "Default value"),
            prop("mode", P::Enum(&["set", "add", "scale"]), "\"set\"", "How the value is combined"),
        ],
        eval("Passes the pose through with the named curve combined by mode; bones and root motion are unchanged.", RM::FromFirstInput, CF::Modifies),
    ),
    // ---- IK / retarget ----
    skeletal(node(
        "anim.two_bone_ik", "Two Bone IK", "IK",
        "Analytic two-bone IK (limbs): reaches the effector, bending toward the pole.",
        POSE, &[req("pose", T::Pose, "Input pose"), req("effector", T::Transform, "Where the end bone should go"), opt("pole", T::Vector, "Joint pole direction, mesh space"), opt("alpha", T::Float, "Falls back to the alpha property")], POSE_OUT,
        &[
            need("end_bone", P::Bone, "The tip bone; its parent and grandparent form the limb"),
            prop("alpha", P::Float { min: 0.0, max: 1.0 }, "1.0", "IK strength"),
            prop("effector_space", P::Enum(&["mesh", "component", "bone"]), "\"mesh\"", "Space of the effector transform"),
            prop("allow_stretch", P::Bool, "false", "Scale the limb when the target is out of reach"),
            prop("max_stretch", P::Float { min: 1.0, max: 4.0 }, "1.25", "Largest stretch factor"),
            prop("take_rotation", P::Bool, "false", "Also rotate the end bone to the effector rotation"),
        ],
        eval("Solves the two-bone triangle for the end bone's chain to reach the effector, bending toward the pole; blends the solved chain with the input by alpha. Other bones, curves and root motion pass through.", RM::FromFirstInput, CF::Passthrough),
    )),
    skeletal(stateful_node(node(
        "anim.fabrik", "FABRIK", "IK",
        "Iterative chain IK for spines, tails and arms.",
        POSE, &[req("pose", T::Pose, "Input pose"), req("effector", T::Transform, "Where the tip bone should go"), opt("alpha", T::Float, "Falls back to the alpha property")], POSE_OUT,
        &[
            need("tip_bone", P::Bone, "Last bone of the chain"),
            need("root_bone", P::Bone, "First bone of the chain; must be an ancestor of the tip"),
            prop("alpha", P::Float { min: 0.0, max: 1.0 }, "1.0", "IK strength"),
            prop("precision", P::Float { min: 1.0e-5, max: 1.0 }, "0.001", "Stop when the tip is this close"),
            prop("max_iterations", P::Int { min: 1, max: 64 }, "10", "Iteration cap"),
            prop("effector_space", P::Enum(&["mesh", "component"]), "\"mesh\"", "Space of the effector transform"),
        ],
        eval("Forward/backward reaching over the root..tip chain until within precision or max_iterations, preserving bone lengths; the solved chain blends with the input by alpha; deterministic for equal inputs.", RM::FromFirstInput, CF::Passthrough),
    ))),
    skeletal(dynamic(node(
        "anim.ik_rig", "IK Rig", "IK",
        "Runs an IK Rig's solver stack with goals from the graph.",
        POSE, &[req("pose", T::Pose, "Input pose"), opt("alpha", T::Float, "Falls back to the alpha property")], POSE_OUT,
        &[
            need("rig", P::Asset(IKRIG_KIND), "The IK Rig"),
            prop("goals", P::NameList, "[]", "Goal names of the rig exposed as inputs"),
            prop("alpha", P::Float { min: 0.0, max: 1.0 }, "1.0", "Overall IK strength"),
        ],
        eval("Applies the rig's enabled solvers in stack order against the goal transforms (an unconnected goal keeps its authored default), blending the result with the input by alpha.", RM::FromFirstInput, CF::Passthrough),
    ), DynamicPins::GoalTransforms { list_prop: "goals" })),
    skeletal(node(
        "anim.retarget", "Retarget Pose", "Retarget",
        "Retargets a pose from a source skeleton to this graph's skeleton.",
        POSE, &[req("source", T::Pose, "A pose on the source skeleton")], POSE_OUT,
        &[
            prop("profile", P::Asset(RETARGET_KIND), "", "Retarget profile (.animretarget); exactly one of profile / ik_retargeter"),
            prop("ik_retargeter", P::Asset(IKRETARGET_KIND), "", "IK retargeter (.ikretarget); exactly one of profile / ik_retargeter"),
            prop("alpha", P::Float { min: 0.0, max: 1.0 }, "1.0", "Blend toward the retargeted pose"),
        ],
        eval("Maps the source pose onto the target skeleton through the profile's chain mapping, retarget pose and root policy (plus IK correction for an IK retargeter); curves pass through by name; root motion is scaled by the root policy.", RM::FromFirstInput, CF::Passthrough),
    )),
    // ---- structure ----
    stateful_node(node(
        "anim.state_machine", "State Machine", "Structure",
        "Runs a state machine and outputs the pose of the active state(s).",
        POSE, NO_PINS, POSE_OUT,
        &[need("machine", P::SubGraph(GraphKind::StateMachine), "The state machine graph")],
        eval("Evaluates the active state's pose graph; during a transition evaluates source and target and blends by the transition's shaped progress; curves and root motion blend likewise.", RM::Blend, CF::Blend),
    )),
    stateful_node(node(
        "anim.layer", "Layer", "Structure",
        "Runs a reusable layer graph in this asset.",
        POSE, &[opt("input", T::Pose, "Passed to the layer's Layer Input node")], POSE_OUT,
        &[need("layer", P::SubGraph(GraphKind::Layer), "The layer graph")],
        eval("Evaluates the layer graph with this node's input as its Layer Input; the layer shares this asset's parameters.", RM::FromFirstInput, CF::Passthrough),
    )),
    stateful_node(node(
        "anim.linked_graph", "Linked Graph", "Structure",
        "Runs another animation graph asset as a layer.",
        POSE, &[opt("input", T::Pose, "Passed to the linked graph's Layer Input node")], POSE_OUT,
        &[need("graph", P::Asset(GRAPH_KIND), "The linked animation graph; its skeleton must match")],
        eval("Evaluates the linked graph's main graph with parameters of equal name and type shared from this graph; unmatched parameters keep the linked graph's defaults.", RM::FromFirstInput, CF::Passthrough),
    )),
    once(node(
        "anim.output_pose", "Output Pose", "Structure",
        "The final pose of the graph.",
        &[GraphKind::Main], &[req("pose", T::Pose, "The final pose")], NO_PINS, &[],
        eval("Delivers the pose, its curves and root motion to the component.", RM::FromFirstInput, CF::Passthrough),
    )),
    once(node(
        "anim.layer_input", "Layer Input", "Structure",
        "The pose passed in by the node that runs this layer.",
        &[GraphKind::Layer, GraphKind::Main], NO_PINS, POSE_OUT, &[],
        eval("Outputs the caller's input pose, or the reference pose when the caller connected none.", RM::FromFirstInput, CF::Passthrough),
    )),
    once(node(
        "anim.layer_output", "Layer Output", "Structure",
        "The pose this layer returns.",
        &[GraphKind::Layer], &[req("pose", T::Pose, "The layer's result")], NO_PINS, &[],
        eval("Returns the pose to the node that ran the layer.", RM::FromFirstInput, CF::Passthrough),
    )),
    once(node(
        "anim.state_result", "State Result", "Structure",
        "The pose of a state.",
        &[GraphKind::StatePose], &[req("pose", T::Pose, "The state's pose")], NO_PINS, &[],
        eval("Returns the pose to the state machine.", RM::FromFirstInput, CF::Passthrough),
    )),
    // ---- parameters and values ----
    dynamic(node(
        "anim.get_parameter", "Get Parameter", "Values",
        "Reads a graph parameter.",
        VALUE, NO_PINS, &[req("value", T::Float, "The parameter value (type follows the parameter)")],
        &[need("parameter", P::Parameter(T::Float), "Parameter name")],
        eval("Reads the component's current value of the named parameter.", RM::None, CF::None),
    ), DynamicPins::OutputOfParameter { param_prop: "parameter" }),
    node(
        "math.constant_float", "Float", "Values", "A constant number.",
        VALUE, NO_PINS, &[req("value", T::Float, "")], &[prop("value", P::Float { min: -1.0e9, max: 1.0e9 }, "0.0", "The number")],
        eval("Outputs the property value.", RM::None, CF::None),
    ),
    node(
        "math.constant_vector", "Vector", "Values", "A constant vector.",
        VALUE, NO_PINS, &[req("value", T::Vector, "")], &[prop("value", P::Vector, "[0.0,0.0,0.0]", "The vector")],
        eval("Outputs the property value.", RM::None, CF::None),
    ),
    node(
        "math.make_vector", "Make Vector", "Values", "Builds a vector from three numbers.",
        VALUE, &[opt("x", T::Float, "Falls back to the x property"), opt("y", T::Float, "Falls back to the y property"), opt("z", T::Float, "Falls back to the z property")], &[req("result", T::Vector, "")],
        &[
            prop("x", P::Float { min: -1.0e9, max: 1.0e9 }, "0.0", "Default x"),
            prop("y", P::Float { min: -1.0e9, max: 1.0e9 }, "0.0", "Default y"),
            prop("z", P::Float { min: -1.0e9, max: 1.0e9 }, "0.0", "Default z"),
        ],
        eval("Outputs (x, y, z).", RM::None, CF::None),
    ),
    node(
        "anim.make_transform", "Make Transform", "Values", "Builds an IK target from a location and an Euler rotation.",
        VALUE, &[opt("location", T::Vector, "Falls back to the location property"), opt("rotation", T::Vector, "Euler degrees XYZ; falls back to the rotation property")], &[req("result", T::Transform, "")],
        &[
            prop("location", P::Vector, "[0.0,0.0,0.0]", "Default location, mesh space"),
            prop("rotation", P::Vector, "[0.0,0.0,0.0]", "Default Euler rotation in degrees"),
        ],
        eval("Outputs a translation + rotation target; rotation is Euler degrees applied X, then Y, then Z.", RM::None, CF::None),
    ),
    node(
        "anim.get_bone_transform", "Get Bone Transform", "Values", "Reads a bone's transform from a pose.",
        VALUE, &[req("pose", T::Pose, "The pose to read")], &[req("result", T::Transform, "")],
        &[
            need("bone", P::Bone, "The bone to read"),
            prop("space", P::Enum(&["mesh", "local"]), "\"mesh\"", "Mesh space or relative to the parent"),
        ],
        needs_skel(eval("Returns the bone's transform in the chosen space from the evaluated input pose (the previous update's value while weights are decided).", RM::None, CF::Modifies)),
    ),
    node(
        "math.constant_bool", "Bool", "Values", "A constant boolean.",
        VALUE, NO_PINS, &[req("value", T::Bool, "")], &[prop("value", P::Bool, "false", "The value")],
        eval("Outputs the property value.", RM::None, CF::None),
    ),
    node(
        "math.float_op", "Float Operation", "Values", "Arithmetic on two numbers.",
        VALUE, &[opt("a", T::Float, "Falls back to the a property"), opt("b", T::Float, "Falls back to the b property")], &[req("result", T::Float, "")],
        &[
            prop("op", P::Enum(&["add", "sub", "mul", "div", "min", "max", "abs_a", "pow"]), "\"add\"", "The operation"),
            prop("a", P::Float { min: -1.0e9, max: 1.0e9 }, "0.0", "Default a"),
            prop("b", P::Float { min: -1.0e9, max: 1.0e9 }, "0.0", "Default b"),
        ],
        eval("Computes a op b; division by zero yields 0; pow of a negative base yields 0.", RM::None, CF::None),
    ),
    node(
        "math.clamp", "Clamp", "Values", "Clamps a number to a range.",
        VALUE, &[req("value", T::Float, "")], &[req("result", T::Float, "")],
        &[prop("min", P::Float { min: -1.0e9, max: 1.0e9 }, "0.0", "Lower bound"), prop("max", P::Float { min: -1.0e9, max: 1.0e9 }, "1.0", "Upper bound")],
        eval("Returns value clamped to [min, max].", RM::None, CF::None),
    ),
    node(
        "math.int_to_float", "Int to Float", "Values", "Converts an integer.",
        VALUE, &[req("value", T::Int, "")], &[req("result", T::Float, "")], &[],
        eval("Exact conversion.", RM::None, CF::None),
    ),
    node(
        "math.compare", "Compare", "Values", "Compares two numbers.",
        VALUE, &[req("a", T::Float, ""), opt("b", T::Float, "Falls back to the b property")], &[req("result", T::Bool, "")],
        &[
            prop("op", P::Enum(&["gt", "ge", "lt", "le", "eq", "ne"]), "\"gt\"", "The comparison"),
            prop("b", P::Float { min: -1.0e9, max: 1.0e9 }, "0.0", "Default b"),
            prop("tolerance", P::Float { min: 0.0, max: 1.0 }, "0.0001", "Equality tolerance for eq/ne"),
        ],
        eval("Evaluates a op b; eq/ne use tolerance.", RM::None, CF::None),
    ),
    node(
        "logic.and", "And", "Values", "Both inputs are true.",
        VALUE, &[req("a", T::Bool, ""), req("b", T::Bool, "")], &[req("result", T::Bool, "")], &[],
        eval("Logical and; both inputs are always evaluated.", RM::None, CF::None),
    ),
    node(
        "logic.or", "Or", "Values", "Either input is true.",
        VALUE, &[req("a", T::Bool, ""), req("b", T::Bool, "")], &[req("result", T::Bool, "")], &[],
        eval("Logical or; both inputs are always evaluated.", RM::None, CF::None),
    ),
    node(
        "logic.not", "Not", "Values", "Inverts a boolean.",
        VALUE, &[req("value", T::Bool, "")], &[req("result", T::Bool, "")], &[],
        eval("Logical not.", RM::None, CF::None),
    ),
    // ---- transition rule context ----
    node(
        "rule.state_time", "State Time", "Rule", "Seconds the source state has been active.",
        &[GraphKind::TransitionRule], NO_PINS, &[req("seconds", T::Float, "")], &[],
        stateful(eval("Time since the source state was entered, in seconds.", RM::None, CF::None)),
    ),
    node(
        "rule.time_remaining", "State Time Remaining", "Rule", "Seconds left in the source state's main asset.",
        &[GraphKind::TransitionRule], NO_PINS, &[req("seconds", T::Float, "")], &[],
        stateful(eval("Remaining time of the source state's primary player (looping players report infinity).", RM::None, CF::None)),
    ),
    node(
        "rule.state_complete", "State Complete", "Rule", "The source state's main asset reached its end.",
        &[GraphKind::TransitionRule], NO_PINS, &[req("complete", T::Bool, "")], &[],
        stateful(eval("True once the source state's primary non-looping player has finished.", RM::None, CF::None)),
    ),
    once(node(
        "rule.result", "Rule Result", "Rule", "Whether the transition may fire.",
        &[GraphKind::TransitionRule], &[req("can_enter", T::Bool, "True to take the transition")], NO_PINS, &[],
        eval("Evaluated each update while the source state is active.", RM::None, CF::None),
    )),
    // ---- state machine graph ----
    once(node(
        "sm.entry", "Entry", "State Machine", "Where the machine starts.",
        &[GraphKind::StateMachine], NO_PINS, &[req("start", T::StateFlow, "Connect to the default state's `default` pin")], &[],
        eval("The state it connects to is active when the machine is first evaluated.", RM::None, CF::None),
    )),
    node(
        "sm.state", "State", "State Machine", "A state with its own pose graph.",
        &[GraphKind::StateMachine],
        &[opt("default", T::StateFlow, "Connect Entry here to make this the default state")],
        &[opt("leave", T::StateFlow, "To the `from` pin of each transition that leaves this state"), opt("arrive", T::StateFlow, "To the `to` pin of each transition that enters this state")],
        &[
            need("name", P::Text, "State name, unique in the machine"),
            need("pose_graph", P::SubGraph(GraphKind::StatePose), "The graph that produces the state's pose"),
            prop("reset_on_entry", P::Bool, "true", "Restart stateful nodes (clocks) whenever the state is entered"),
        ],
        stateful(eval("Evaluates the state's pose graph while active or blending.", RM::FromSource, CF::FromSource)),
    ),
    node(
        "sm.transition", "Transition", "State Machine", "A rule-gated blend between two states.",
        &[GraphKind::StateMachine], &[req("from", T::StateFlow, "The source state's `leave` pin"), req("to", T::StateFlow, "The target state's `arrive` pin")], NO_PINS,
        &[
            need("rule", P::SubGraph(GraphKind::TransitionRule), "The rule graph"),
            prop("duration", P::Float { min: 0.0, max: 30.0 }, "0.2", "Blend time in seconds"),
            prop("curve", P::Enum(BLEND_CURVES), "\"ease_in_out\"", "Blend shaping"),
            prop("priority", P::Int { min: 0, max: 1000 }, "0", "Lower fires first when several rules are true"),
            prop("interrupt", P::Enum(&["none", "source", "target", "both"]), "\"none\"", "Which side of an in-flight transition may be interrupted"),
            prop("allow_reentry", P::Bool, "false", "Required for a transition from a state to itself"),
        ],
        stateful(eval("Each update the lowest-priority-value true rule from the active state starts blending toward its target over duration; an interruptible in-flight transition may be replaced.", RM::Blend, CF::Blend)),
    ),
];

const fn stateful_node(mut n: NodeDef) -> NodeDef {
    n.eval.stateful = true;
    n
}
const fn skeletal(mut n: NodeDef) -> NodeDef {
    n.eval.needs_skeleton = true;
    n
}
const fn notifying(mut n: NodeDef) -> NodeDef {
    n.eval.notifies = true;
    n
}

pub fn node_def(type_id: &str) -> Option<&'static NodeDef> {
    CATALOGUE.iter().find(|n| n.type_id == type_id)
}

/// A pin as it exists on a particular node instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinSpec {
    pub name: String,
    pub ty: AnimPinType,
    pub required: bool,
}

/// The input and output pins `def` has given the property values `props`.
/// `param_type` resolves a parameter name to its type for
/// [`DynamicPins::OutputOfParameter`].
pub fn resolve_pins(
    def: &NodeDef,
    props: &std::collections::HashMap<String, Value>,
    param_type: impl Fn(&str) -> Option<ParamType>,
) -> (Vec<PinSpec>, Vec<PinSpec>) {
    let spec = |p: &PinDef| PinSpec { name: p.name.into(), ty: p.ty, required: p.required };
    let mut inputs: Vec<PinSpec> = def.inputs.iter().map(spec).collect();
    let mut outputs: Vec<PinSpec> = def.outputs.iter().map(spec).collect();
    match def.dynamic {
        DynamicPins::None => {}
        DynamicPins::PoseWeightPairs { count_prop, stem, weight_stem } => {
            let count = props.get(count_prop).and_then(|v| v.as_i64()).unwrap_or(0).clamp(0, 16);
            for i in 0..count {
                inputs.push(PinSpec { name: format!("{stem}_{i}"), ty: T::Pose, required: true });
                if let Some(w) = weight_stem {
                    inputs.push(PinSpec { name: format!("{w}_{i}"), ty: T::Float, required: false });
                }
            }
        }
        DynamicPins::GoalTransforms { list_prop } => {
            if let Some(Value::Array(names)) = props.get(list_prop) {
                for name in names.iter().filter_map(Value::as_str) {
                    inputs.push(PinSpec { name: format!("goal_{name}"), ty: T::Transform, required: false });
                }
            }
        }
        DynamicPins::OutputOfParameter { param_prop } => {
            let ty = props
                .get(param_prop)
                .and_then(Value::as_str)
                .and_then(param_type)
                .map_or(T::Float, ParamType::pin_type);
            outputs = vec![PinSpec { name: "value".into(), ty, required: true }];
        }
    }
    (inputs, outputs)
}

/// Default property values for a new node of `def`.
pub fn default_properties(def: &NodeDef) -> std::collections::HashMap<String, Value> {
    def.props
        .iter()
        .filter(|p| !p.default.is_empty())
        .filter_map(|p| Some((p.name.to_string(), serde_json::from_str(p.default).ok()?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_is_self_consistent() {
        let mut seen = std::collections::HashSet::new();
        for def in CATALOGUE {
            assert!(seen.insert(def.type_id), "duplicate type {}", def.type_id);
            assert!(!def.graphs.is_empty(), "{} is placeable nowhere", def.type_id);
            assert!(!def.eval.summary.is_empty(), "{} has no evaluation contract", def.type_id);
            let mut pins = std::collections::HashSet::new();
            for p in def.inputs.iter().chain(def.outputs) {
                // input and output may share a name (blend has none); within a direction names are unique
                let _ = p;
            }
            for p in def.inputs {
                assert!(pins.insert(("in", p.name)), "{}: duplicate input {}", def.type_id, p.name);
            }
            for p in def.outputs {
                assert!(pins.insert(("out", p.name)), "{}: duplicate output {}", def.type_id, p.name);
            }
            let mut names = std::collections::HashSet::new();
            for p in def.props {
                assert!(names.insert(p.name), "{}: duplicate property {}", def.type_id, p.name);
                if !p.default.is_empty() {
                    serde_json::from_str::<Value>(p.default)
                        .unwrap_or_else(|e| panic!("{}.{} default is not JSON: {e}", def.type_id, p.name));
                }
            }
            // Every optional data input has a same-named property fallback.
            for p in def.inputs.iter().filter(|p| !p.required) {
                if matches!(p.ty, T::Float | T::Bool | T::Int) {
                    assert!(
                        def.props.iter().any(|q| q.name == p.name),
                        "{}: optional input {} has no property fallback",
                        def.type_id,
                        p.name
                    );
                }
            }
        }
    }

    #[test]
    fn every_graph_kind_has_exactly_one_terminal_definition() {
        for kind in [GraphKind::Main, GraphKind::Layer, GraphKind::StateMachine, GraphKind::StatePose, GraphKind::TransitionRule] {
            let def = node_def(kind.terminal()).expect("terminal exists");
            assert_eq!(def.max_per_graph, 1);
            assert!(def.graphs.contains(&kind));
        }
    }

    #[test]
    fn the_planned_node_families_exist() {
        for id in [
            "anim.sequence_player", "anim.blend_space_player", "anim.slot", "anim.state_machine",
            "sm.transition", "rule.result", "anim.save_cached_pose", "anim.use_cached_pose",
            "anim.layered_blend_per_bone", "anim.apply_additive", "anim.make_additive",
            "anim.two_bone_ik", "anim.fabrik", "anim.ik_rig", "anim.retarget", "anim.pose_search",
            "anim.get_curve", "anim.modify_curve", "anim.linked_graph", "anim.layer",
        ] {
            assert!(node_def(id).is_some(), "missing {id}");
        }
    }

    #[test]
    fn dynamic_pins_follow_properties() {
        let def = node_def("anim.layered_blend_per_bone").unwrap();
        let mut props = default_properties(def);
        props.insert("count".into(), Value::from(2));
        let (inputs, _) = resolve_pins(def, &props, |_| None);
        let names: Vec<_> = inputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["base", "layer_0", "layer_weight_0", "layer_1", "layer_weight_1"]);

        let get = node_def("anim.get_parameter").unwrap();
        let mut props = default_properties(get);
        props.insert("parameter".into(), Value::from("Grounded"));
        let (_, outputs) = resolve_pins(get, &props, |n| (n == "Grounded").then_some(ParamType::Bool));
        assert_eq!(outputs[0].ty, AnimPinType::Bool);
    }

    #[test]
    fn additive_pose_does_not_connect_to_pose() {
        assert!(!AnimPinType::AdditivePose.connects_to(AnimPinType::Pose));
        assert!(AnimPinType::Pose.connects_to(AnimPinType::Pose));
        for t in [AnimPinType::Pose, AnimPinType::Float, AnimPinType::StateFlow] {
            assert_eq!(AnimPinType::from_type_name(t.type_name()), Some(t));
        }
    }
}

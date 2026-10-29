//! The `.animgraph` asset: a Graphy graph document with an animation profile.
//!
//! Version 2 stores the pose graph as Graphy's own `GraphDescription` (the one
//! mutable graph model shared with Blueprint, Render Graph and Shader), plus
//! the animation-owned parts Graphy has no concept of: typed parameters and the
//! sub-graphs that state machines, states, transition rules and layers refer
//! to. Node and pin semantics live in [`catalogue`]; [`validate`] reports
//! problems; [`migrate`] reads the version 1 Sequence/BlendSpace/Output schema.
//!
//! Camera, selection, active-state highlighting and preview inputs are editor
//! state and never stored here.

pub mod builder;
pub mod catalogue;
pub mod migrate;
pub mod validate;

use std::collections::HashMap;

use graphy_blueprint::{
    Connection, ConnectionType, GraphDescription, NodeInstance, Pin, PinInstance, PinType, Position,
};
use pulsar_reflection::{AnimationGraphAssetKind, AssetRef, SkeletonAssetKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::blend_space::Migration;
use crate::codec::{self, ensure_unique, from_value, read_envelope, CodecError, Extra, VersionPolicy};
use catalogue::{default_properties, node_def, resolve_pins, GraphKind, ParamType, PinSpec};

pub const ANIMATION_GRAPH_EXTENSION: &str = "animgraph";
pub const ANIM_GRAPH_VERSION: u32 = 2;
const POLICY: VersionPolicy = VersionPolicy {
    oldest: 1,
    current: ANIM_GRAPH_VERSION,
    unversioned_as: Some(1),
};

/// A named, typed value the component sets and nodes read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphParameter {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ParamType,
    pub default: Value,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

impl GraphParameter {
    pub fn new(name: impl Into<String>, ty: ParamType) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            ty,
            default: ty.default_value(),
            description: String::new(),
        }
    }
}

/// A graph nested in the asset: a state machine, a state's pose, a transition
/// rule or a reusable layer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnimSubGraph {
    pub id: String,
    pub name: String,
    pub kind: GraphKind,
    pub graph: GraphDescription,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnimationGraphAsset {
    pub version: u32,
    pub id: AssetRef<AnimationGraphAssetKind>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeleton: Option<AssetRef<SkeletonAssetKind>>,
    #[serde(default)]
    pub parameters: Vec<GraphParameter>,
    pub main: GraphDescription,
    #[serde(default)]
    pub subgraphs: Vec<AnimSubGraph>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for AnimationGraphAsset {
    fn default() -> Self {
        let mut main = GraphDescription::new("AnimGraph");
        main.add_node(instantiate_node("output", "anim.output_pose", (400.0, 0.0), &|_| None).expect("catalogue node"))
            .expect("fresh graph");
        Self {
            version: ANIM_GRAPH_VERSION,
            id: AssetRef::new(),
            name: "New Animation Graph".into(),
            skeleton: None,
            parameters: Vec::new(),
            main,
            subgraphs: Vec::new(),
            extra: Extra::new(),
        }
    }
}

/// A new node of catalogue type `type_id`, with its default properties and the
/// pins those properties imply. `None` if the type is unknown.
pub fn instantiate_node(
    id: &str,
    type_id: &str,
    position: (f32, f32),
    param_type: &dyn Fn(&str) -> Option<ParamType>,
) -> Option<NodeInstance> {
    let def = node_def(type_id)?;
    let properties = default_properties(def);
    let (inputs, outputs) = resolve_pins(def, &properties, param_type);
    Some(NodeInstance {
        id: id.to_string(),
        node_type: type_id.to_string(),
        position: Position { x: position.0, y: position.1 },
        properties,
        inputs: inputs.iter().map(|p| pin_instance(p, PinType::Input)).collect(),
        outputs: outputs.iter().map(|p| pin_instance(p, PinType::Output)).collect(),
    })
}

fn pin_instance(spec: &PinSpec, direction: PinType) -> PinInstance {
    PinInstance {
        id: spec.name.clone(),
        pin: Pin {
            name: spec.name.clone(),
            pin_type: direction,
            data_type: spec.ty.data_type(),
            connected_to: Vec::new(),
        },
    }
}

/// Bring `node`'s pins in line with its properties (after a count, goal-list or
/// parameter change), keeping every pin whose name survives. Connections to
/// pins that disappeared are returned so the caller can remove them.
pub fn refresh_node_pins(
    node: &mut NodeInstance,
    param_type: &dyn Fn(&str) -> Option<ParamType>,
) -> Vec<String> {
    let Some(def) = node_def(&node.node_type) else { return Vec::new() };
    let (inputs, outputs) = resolve_pins(def, &node.properties, param_type);
    let mut lost = Vec::new();
    let mut rebuild = |old: &mut Vec<PinInstance>, wanted: &[PinSpec], direction: PinType| {
        let mut next = Vec::with_capacity(wanted.len());
        for spec in wanted {
            match old.iter().position(|p| p.id == spec.name && p.pin.data_type == spec.ty.data_type()) {
                Some(i) => next.push(old.remove(i)),
                None => next.push(pin_instance(spec, direction.clone())),
            }
        }
        for gone in old.drain(..) {
            lost.extend(gone.pin.connected_to);
        }
        *old = next;
    };
    rebuild(&mut node.inputs, &inputs, PinType::Input);
    rebuild(&mut node.outputs, &outputs, PinType::Output);
    lost
}

/// A data connection between two pins, with a deterministic id.
pub fn data_connection(source: &str, source_pin: &str, target: &str, target_pin: &str) -> Connection {
    Connection {
        id: format!("{source}.{source_pin}->{target}.{target_pin}"),
        source_node: source.into(),
        source_pin: source_pin.into(),
        target_node: target.into(),
        target_pin: target_pin.into(),
        connection_type: ConnectionType::Data,
    }
}

impl AnimationGraphAsset {
    pub fn subgraph(&self, id: &str) -> Option<&AnimSubGraph> {
        self.subgraphs.iter().find(|s| s.id == id)
    }

    pub fn parameter(&self, name: &str) -> Option<&GraphParameter> {
        self.parameters.iter().find(|p| p.name == name)
    }

    /// Every graph with its kind: the main graph, then each sub-graph.
    pub fn graphs(&self) -> impl Iterator<Item = (Option<&AnimSubGraph>, GraphKind, &GraphDescription)> {
        std::iter::once((None, GraphKind::Main, &self.main))
            .chain(self.subgraphs.iter().map(|s| (Some(s), s.kind, &s.graph)))
    }

    pub fn parse(text: &str) -> Result<Self, CodecError> {
        Self::parse_migrating(text).map(|(asset, _)| asset)
    }

    pub fn parse_migrating(text: &str) -> Result<(Self, Migration), CodecError> {
        let (value, from) = read_envelope(text, POLICY)?;
        if from < 2 {
            let (asset, migration) = migrate::from_v1(value)?;
            asset.validate_structure().map_err(CodecError::Invalid)?;
            return Ok((asset, migration));
        }
        if value.get("id").is_none() {
            return Err(CodecError::MissingId);
        }
        let mut asset: Self = from_value(value)?;
        asset.version = ANIM_GRAPH_VERSION;
        asset.validate_structure().map_err(CodecError::Invalid)?;
        Ok((asset, Migration { from_version: from, ..Default::default() }))
    }

    pub fn to_text(&self) -> anyhow::Result<String> {
        self.validate_structure().map_err(|e| anyhow::anyhow!(e))?;
        let mut asset = self.clone();
        asset.version = ANIM_GRAPH_VERSION;
        Ok(crate::codec::to_stable_pretty(&asset)?)
    }

    pub fn peek_id(text: &str) -> Option<String> {
        codec::peek_uuid(text)
    }

    /// Identity rules a file must satisfy to be read at all. Semantic
    /// problems (unconnected pins, wrong types) are [`validate::validate`].
    pub fn validate_structure(&self) -> Result<(), String> {
        ensure_unique("parameter", self.parameters.iter().map(|p| p.id.as_str()))?;
        ensure_unique("parameter name", self.parameters.iter().map(|p| p.name.as_str()))?;
        ensure_unique("sub-graph", self.subgraphs.iter().map(|s| s.id.as_str()))?;
        for parameter in &self.parameters {
            if !parameter.ty.accepts(&parameter.default) {
                return Err(format!("Parameter `{}` default does not match its type", parameter.name));
            }
        }
        for (_, _, graph) in self.graphs() {
            // GraphDescription keys nodes by id; a mismatch is a duplicate-id
            // symptom of hand edits or a bad merge.
            for (key, node) in &graph.nodes {
                if key != &node.id {
                    return Err(format!("Node key `{key}` does not match node id `{}`", node.id));
                }
            }
            let ids: Vec<&str> = graph.connections.iter().map(|c| c.id.as_str()).collect();
            ensure_unique("connection", ids)?;
        }
        Ok(())
    }

    /// A parameter-name to type lookup for pin resolution.
    pub fn param_types(&self) -> HashMap<String, ParamType> {
        self.parameters.iter().map(|p| (p.name.clone(), p.ty)).collect()
    }
}

crate::asset_file_io!(AnimationGraphAsset, load_animation_graph, save_animation_graph);

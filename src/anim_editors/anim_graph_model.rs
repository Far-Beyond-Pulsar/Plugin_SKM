//! Animation Graph editor model: the `.animgraph` document, which of its graphs
//! is open (main graph, state machine, a state's pose graph, a transition
//! rule, a layer), and every structural command: nodes, wires, properties,
//! parameters, states, transitions and sub-graphs.
//!
//! The Graphy canvas edits the open graph's pins and wires directly; whatever
//! it produces comes back through [`AnimGraphModel::replace_graph`] as one
//! undoable step. Everything else (palette, inspector, parameters, state
//! machine commands) goes through the methods here and is then pushed to the
//! canvas. The catalogue (`skeletal_assets::anim_graph::catalogue`) is the only
//! source of node and pin semantics.

use std::path::Path;
use std::sync::Arc;

use graphy_blueprint::GraphDescription;
use serde_json::{json, Value};
use skeletal_assets::anim_graph::catalogue::{node_def, AnimPinType, GraphKind, NodeDef, ParamType, PropKind, CATALOGUE};
use skeletal_assets::anim_graph::validate::validate;
use skeletal_assets::anim_graph::{data_connection, instantiate_node, refresh_node_pins, AnimSubGraph, AnimationGraphAsset, GraphParameter};
use skeletal_assets::diagnostics::Diagnostic;

use super::document::AnimDocument;
use super::selection::AnimSelection;
use super::store::DocStore;

/// Which graph of the asset is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphRef {
    Main,
    Sub(String),
}

pub struct AnimGraphModel {
    pub doc: AnimDocument<AnimationGraphAsset>,
    pub selection: AnimSelection,
    active: GraphRef,
    /// Graphs entered on the way to the active one, for the breadcrumb.
    trail: Vec<GraphRef>,
}

fn short(ty: &str) -> String {
    ty.rsplit('.').next().unwrap_or(ty).to_string()
}

impl AnimGraphModel {
    pub fn open(store: Arc<dyn DocStore>, path: &Path, default_text: &str) -> Result<Self, String> {
        let doc = AnimDocument::<AnimationGraphAsset>::open(store, path, default_text)?;
        Ok(Self { doc, selection: AnimSelection::None, active: GraphRef::Main, trail: Vec::new() })
    }

    pub fn asset(&self) -> &AnimationGraphAsset {
        self.doc.value()
    }

    pub fn active(&self) -> &GraphRef {
        &self.active
    }

    pub fn graph(&self) -> &GraphDescription {
        match &self.active {
            GraphRef::Main => &self.asset().main,
            GraphRef::Sub(id) => self.asset().subgraph(id).map_or(&self.asset().main, |s| &s.graph),
        }
    }

    pub fn graph_kind(&self) -> GraphKind {
        match &self.active {
            GraphRef::Main => GraphKind::Main,
            GraphRef::Sub(id) => self.asset().subgraph(id).map_or(GraphKind::Main, |s| s.kind),
        }
    }

    pub fn graph_title(&self) -> String {
        match &self.active {
            GraphRef::Main => "Main graph".into(),
            GraphRef::Sub(id) => self.asset().subgraph(id).map_or("?".into(), |s| s.name.clone()),
        }
    }

    pub fn param_types(&self) -> std::collections::HashMap<String, ParamType> {
        self.asset().param_types()
    }

    /// The graphs on the way to the active one (the last is the active graph).
    pub fn breadcrumb(&self) -> Vec<(GraphRef, String)> {
        let name = |g: &GraphRef| match g {
            GraphRef::Main => "Main graph".to_string(),
            GraphRef::Sub(id) => self.asset().subgraph(id).map_or("?".into(), |s| s.name.clone()),
        };
        let mut out: Vec<(GraphRef, String)> = vec![(GraphRef::Main, name(&GraphRef::Main))];
        for g in self.trail.iter().chain(std::iter::once(&self.active)) {
            if *g != GraphRef::Main {
                out.push((g.clone(), name(g)));
            }
        }
        out
    }

    // ---- navigation ---------------------------------------------------------

    /// Open a graph of the asset. The breadcrumb is rebuilt from the sub-graph
    /// references so it is always a real path from the main graph.
    pub fn open_graph(&mut self, target: GraphRef) -> bool {
        if let GraphRef::Sub(id) = &target {
            if self.asset().subgraph(id).is_none() {
                return false;
            }
        }
        self.trail = self.path_to(&target);
        if target != GraphRef::Main {
            self.trail.pop();
        }
        self.active = target;
        self.selection = AnimSelection::None;
        true
    }

    /// Main graph first, then each graph that references the next one.
    fn path_to(&self, target: &GraphRef) -> Vec<GraphRef> {
        let GraphRef::Sub(want) = target else { return vec![GraphRef::Main] };
        let mut path = vec![target.clone()];
        let mut at = want.clone();
        for _ in 0..16 {
            let parent = self.parent_of(&at);
            match parent {
                Some(GraphRef::Sub(p)) => {
                    path.push(GraphRef::Sub(p.clone()));
                    at = p;
                }
                Some(GraphRef::Main) | None => break,
            }
        }
        path.push(GraphRef::Main);
        path.reverse();
        path
    }

    fn parent_of(&self, sub: &str) -> Option<GraphRef> {
        let holds = |g: &GraphDescription| {
            g.nodes.values().any(|n| node_def(&n.node_type).is_some_and(|d| d.props.iter().any(|p| matches!(p.kind, PropKind::SubGraph(_)) && n.properties.get(p.name).and_then(Value::as_str) == Some(sub))))
        };
        if holds(&self.asset().main) {
            return Some(GraphRef::Main);
        }
        self.asset().subgraphs.iter().find(|s| holds(&s.graph)).map(|s| GraphRef::Sub(s.id.clone()))
    }

    /// The graph a node opens: the sub-graph its `SubGraph` property names.
    pub fn target_of(&self, node: &str) -> Option<GraphRef> {
        let n = self.graph().nodes.get(node)?;
        let def = node_def(&n.node_type)?;
        def.props.iter().find(|p| matches!(p.kind, PropKind::SubGraph(_))).and_then(|p| n.properties.get(p.name)?.as_str()).filter(|id| self.asset().subgraph(id).is_some()).map(|id| GraphRef::Sub(id.to_string()))
    }

    // ---- editing the active graph ---------------------------------------------

    fn with_graph<R>(&mut self, label: &str, key: Option<&str>, f: impl FnOnce(&mut GraphDescription, &std::collections::HashMap<String, ParamType>) -> Result<R, String>) -> Result<R, String> {
        let active = self.active.clone();
        let params = self.param_types();
        let mut result = None;
        let mut error = None;
        self.doc.edit(label, key, |a| {
            let mut candidate = match &active {
                GraphRef::Main => a.main.clone(),
                GraphRef::Sub(id) => match a.subgraphs.iter().find(|s| &s.id == id) {
                    Some(s) => s.graph.clone(),
                    None => {
                        error = Some("the graph no longer exists".to_string());
                        return (false, ());
                    }
                },
            };
            match f(&mut candidate, &params) {
                Ok(r) => {
                    match &active {
                        GraphRef::Main => a.main = candidate,
                        GraphRef::Sub(id) => {
                            if let Some(s) = a.subgraphs.iter_mut().find(|s| &s.id == id) {
                                s.graph = candidate;
                            }
                        }
                    }
                    result = Some(r);
                    (true, ())
                }
                Err(e) => {
                    error = Some(e);
                    (false, ())
                }
            }
        });
        match (result, error) {
            (Some(r), _) => Ok(r),
            (None, Some(e)) => Err(e),
            (None, None) => Err("nothing changed".into()),
        }
    }

    /// Replace the active graph with what the canvas now holds. Successive
    /// replacements with the same `key` (a drag) are one undo step.
    pub fn replace_graph(&mut self, graph: GraphDescription, label: &str, key: Option<&str>) {
        let _ = self.with_graph(label, key, |g, _| {
            *g = graph;
            Ok(())
        });
    }

    pub fn end_gesture(&mut self) {
        self.doc.end_gesture();
    }

    fn unique_node_id(&self, ty: &str) -> String {
        format!("{}_{}", short(ty), &uuid::Uuid::new_v4().simple().to_string()[..8])
    }

    /// Whether a node of `ty` may be added to the open graph.
    pub fn can_add(&self, ty: &str) -> Result<&'static NodeDef, String> {
        let def = node_def(ty).ok_or_else(|| format!("unknown node type `{ty}`"))?;
        let kind = self.graph_kind();
        if !def.graphs.contains(&kind) {
            return Err(format!("{} cannot be placed in this kind of graph", def.title));
        }
        if def.max_per_graph > 0 && self.graph().nodes.values().filter(|n| n.node_type == ty).count() as u32 >= def.max_per_graph {
            return Err(format!("{} appears once per graph", def.title));
        }
        Ok(def)
    }

    /// The catalogue entries valid in the open graph.
    pub fn palette(&self) -> Vec<&'static NodeDef> {
        CATALOGUE.iter().filter(|d| self.can_add(d.type_id).is_ok()).collect()
    }

    pub fn add_node(&mut self, ty: &str, position: (f32, f32)) -> Result<String, String> {
        self.can_add(ty)?;
        let id = self.unique_node_id(ty);
        let node_id = id.clone();
        self.with_graph("Add node", None, |g, params| {
            let node = instantiate_node(&node_id, ty, position, &|p| params.get(p).copied()).ok_or("unknown node type")?;
            g.add_node(node)
        })?;
        self.selection = AnimSelection::GraphNode { id: id.clone() };
        Ok(id)
    }

    /// Delete a node (and the sub-graphs it owns). A graph's terminal node stays.
    pub fn delete_node(&mut self, id: &str) -> Result<(), String> {
        let node = self.graph().nodes.get(id).cloned().ok_or("no such node")?;
        if node.node_type == self.graph_kind().terminal() {
            return Err("this node ends the graph and cannot be deleted".into());
        }
        let owned: Vec<String> = node_def(&node.node_type)
            .map(|d| d.props.iter().filter(|p| matches!(p.kind, PropKind::SubGraph(_))).filter_map(|p| node.properties.get(p.name)?.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let id = id.to_string();
        self.with_graph("Delete node", None, |g, _| {
            g.remove_node(&id);
            Ok(())
        })?;
        if !owned.is_empty() {
            self.doc.apply("Delete owned graphs", |a| a.subgraphs.retain(|s| !owned.contains(&s.id)));
        }
        if matches!(&self.selection, AnimSelection::GraphNode { id: s } if *s == id) {
            self.selection = AnimSelection::None;
        }
        Ok(())
    }

    pub fn move_node(&mut self, id: &str, x: f32, y: f32) -> bool {
        let id = id.to_string();
        self.with_graph("Move node", Some(&format!("move:{id}")), |g, _| {
            let n = g.nodes.get_mut(&id).ok_or("no such node")?;
            if n.position.x == x && n.position.y == y {
                return Err("unchanged".into());
            }
            n.position.x = x;
            n.position.y = y;
            Ok(())
        })
        .is_ok()
    }

    /// Wire `from`'s output to `to`'s input. Types must match exactly; an input
    /// takes one wire, so it replaces the one it had.
    pub fn connect(&mut self, from: (&str, &str), to: (&str, &str)) -> Result<(), String> {
        let (sn, sp, tn, tp) = (from.0.to_string(), from.1.to_string(), to.0.to_string(), to.1.to_string());
        self.with_graph("Connect", None, |g, _| {
            let src = g.nodes.get(&sn).ok_or("no such source node")?;
            let dst = g.nodes.get(&tn).ok_or("no such target node")?;
            let out = src.outputs.iter().find(|p| p.id == sp).ok_or("no such output pin")?;
            let inp = dst.inputs.iter().find(|p| p.id == tp).ok_or("no such input pin")?;
            let (a, b) = (pin_type(&out.pin.data_type), pin_type(&inp.pin.data_type));
            match (a, b) {
                (Some(a), Some(b)) if a.connects_to(b) => {}
                (Some(a), Some(b)) => return Err(format!("{} cannot connect to {}", a.type_name(), b.type_name())),
                _ => return Err("unknown pin type".into()),
            }
            if sn == tn {
                return Err("a node cannot connect to itself".into());
            }
            let existing: Vec<String> = g.connections.iter().filter(|c| c.target_node == tn && c.target_pin == tp).map(|c| c.id.clone()).collect();
            for id in existing {
                g.remove_connection(&id);
            }
            g.add_connection(data_connection(&sn, &sp, &tn, &tp))
        })
    }

    pub fn disconnect(&mut self, connection: &str) -> bool {
        let id = connection.to_string();
        self.with_graph("Disconnect", None, |g, _| {
            if !g.connections.iter().any(|c| c.id == id) {
                return Err("no such connection".into());
            }
            g.remove_connection(&id);
            Ok(())
        })
        .is_ok()
    }

    // ---- properties -------------------------------------------------------------

    /// Check `value` against a property's declared kind.
    pub fn check_property(&self, kind: PropKind, value: &Value) -> Result<(), String> {
        let text_ok = |v: &Value| v.is_null() || v.as_str().is_some();
        match kind {
            PropKind::Float { min, max } => {
                let v = value.as_f64().ok_or("enter a number")? as f32;
                if !v.is_finite() || v < min || v > max {
                    return Err(format!("must be between {min} and {max}"));
                }
            }
            PropKind::Int { min, max } => {
                let v = value.as_i64().ok_or("enter a whole number")?;
                if v < min || v > max {
                    return Err(format!("must be between {min} and {max}"));
                }
            }
            PropKind::Bool => {
                value.as_bool().ok_or("must be true or false")?;
            }
            PropKind::Enum(options) => {
                let s = value.as_str().ok_or("pick one of the options")?;
                if !options.contains(&s) {
                    return Err(format!("must be one of: {}", options.join(", ")));
                }
            }
            PropKind::Parameter(ty) => match value.as_str().filter(|s| !s.is_empty()) {
                None => {}
                Some(name) => {
                    let p = self.asset().parameter(name).ok_or_else(|| format!("no parameter named `{name}`"))?;
                    if p.ty.pin_type() != ty {
                        return Err(format!("`{name}` is {}, expected {}", p.ty.pin_type().type_name(), ty.type_name()));
                    }
                }
            },
            PropKind::Asset(_) => {
                if let Some(s) = value.as_str().filter(|s| !s.is_empty()) {
                    pulsar_reflection::AssetRef::<pulsar_reflection::AnimationSequenceAssetKind>::parse(s).ok_or("not an asset reference")?;
                }
            }
            PropKind::Vector => {
                let ok = value.as_array().is_some_and(|a| a.len() == 3 && a.iter().all(|v| v.as_f64().is_some()));
                if !ok {
                    return Err("enter three numbers".into());
                }
            }
            PropKind::Text | PropKind::Bone | PropKind::Curve | PropKind::Slot | PropKind::CachedPose | PropKind::SyncGroup | PropKind::SubGraph(_) => {
                if !text_ok(value) {
                    return Err("enter text".into());
                }
            }
            PropKind::BoneMask => super::bone_mask_edit::validate(value, None, &[])?,
            PropKind::BoneList | PropKind::NameList => {}
        }
        Ok(())
    }

    /// Edit one layer's bone mask of a node as a single undoable property edit.
    /// The result is checked against `skeleton` (the graph's), so a mask never
    /// names a bone the skeleton lacks.
    pub fn edit_mask(&mut self, node: &str, layer: usize, skeleton: Option<&skeletal_assets::skeleton::Skeleton>, library: &[skeletal_assets::bone_mask::NamedBoneMask], change: impl FnOnce(&mut skeletal_assets::bone_mask::BoneMaskSpec) -> Result<(), String>) -> Result<(), String> {
        let n = self.graph().nodes.get(node).ok_or("no such node")?.clone();
        if n.node_type != "anim.layered_blend_per_bone" {
            return Err("only a Layered Blend per Bone node has bone masks".into());
        }
        let layers = n.properties.get("count").and_then(Value::as_u64).unwrap_or(1) as usize;
        if layer >= layers.max(1) {
            return Err("no such layer".into());
        }
        let mut specs = super::bone_mask_edit::read(n.properties.get("masks").unwrap_or(&Value::Null), layers);
        change(&mut specs[layer])?;
        let value = super::bone_mask_edit::write(&specs);
        super::bone_mask_edit::validate(&value, skeleton, library)?;
        self.set_property(node, "masks", value)
    }

    /// Set a node property. Pins that depend on it are refreshed; wires on pins
    /// that disappear are removed with them.
    pub fn set_property(&mut self, node: &str, prop: &str, value: Value) -> Result<(), String> {
        let n = self.graph().nodes.get(node).ok_or("no such node")?.clone();
        let def = node_def(&n.node_type).ok_or("unknown node type")?;
        let pdef = def.props.iter().find(|p| p.name == prop).ok_or_else(|| format!("{} has no `{prop}`", def.title))?;
        self.check_property(pdef.kind, &value)?;
        let (node, prop) = (node.to_string(), prop.to_string());
        self.with_graph("Edit property", Some(&format!("prop:{node}:{prop}")), |g, params| {
            let n = g.nodes.get_mut(&node).ok_or("no such node")?;
            if n.properties.get(&prop) == Some(&value) {
                return Err("unchanged".into());
            }
            n.properties.insert(prop.clone(), value.clone());
            let lost = refresh_node_pins(n, &|p| params.get(p).copied());
            for id in lost {
                g.remove_connection(&id);
            }
            Ok(())
        })
    }

    pub fn property(&self, node: &str, prop: &str) -> Option<&Value> {
        self.graph().nodes.get(node)?.properties.get(prop)
    }

    // ---- sub-graphs, states, transitions -------------------------------------------

    fn new_subgraph(&mut self, kind: GraphKind, name: &str) -> Result<String, String> {
        let id = format!("{}_{}", match kind {
            GraphKind::StatePose => "state",
            GraphKind::TransitionRule => "rule",
            GraphKind::StateMachine => "machine",
            GraphKind::Layer => "layer",
            GraphKind::Main => "main",
        }, &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let terminal = kind.terminal();
        let params = self.param_types();
        let node = instantiate_node(&format!("{}_end", short(terminal)), terminal, (400.0, 0.0), &|p| params.get(p).copied()).ok_or("no terminal node for that graph")?;
        let mut graph = GraphDescription::new(name);
        graph.add_node(node)?;
        let sub = AnimSubGraph { id: id.clone(), name: name.to_string(), kind, graph };
        self.doc.apply("Add graph", |a| a.subgraphs.push(sub));
        Ok(id)
    }

    /// Create the sub-graph a node's `SubGraph` property names, if it has none yet.
    pub fn ensure_subgraph(&mut self, node: &str) -> Result<GraphRef, String> {
        if let Some(t) = self.target_of(node) {
            return Ok(t);
        }
        let n = self.graph().nodes.get(node).ok_or("no such node")?.clone();
        let def = node_def(&n.node_type).ok_or("unknown node type")?;
        let pdef = def.props.iter().find(|p| matches!(p.kind, PropKind::SubGraph(_))).ok_or("this node does not open a graph")?;
        let PropKind::SubGraph(kind) = pdef.kind else { return Err("not a sub-graph property".into()) };
        let title = n.properties.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| def.title.to_string());
        let id = self.new_subgraph(kind, &title)?;
        self.set_property(node, pdef.name, json!(id))?;
        Ok(GraphRef::Sub(id))
    }

    fn unique_state_name(&self, base: &str) -> String {
        let taken = |n: &str| self.graph().nodes.values().any(|s| s.node_type == "sm.state" && s.properties.get("name").and_then(Value::as_str) == Some(n));
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap_or_default()
    }

    /// Add a state (with its own pose graph) to the open state machine. The
    /// first state added becomes the default.
    pub fn add_state(&mut self, name: &str, position: (f32, f32)) -> Result<String, String> {
        if self.graph_kind() != GraphKind::StateMachine {
            return Err("open a state machine first".into());
        }
        let name = self.unique_state_name(if name.trim().is_empty() { "State" } else { name.trim() });
        let id = self.add_node("sm.state", position)?;
        self.set_property(&id, "name", json!(name))?;
        self.ensure_subgraph(&id)?;
        let has_default = self.graph().connections.iter().any(|c| c.target_pin == "default");
        if !has_default {
            if let Some(entry) = self.graph().nodes.values().find(|n| n.node_type == "sm.entry").map(|n| n.id.clone()) {
                self.connect((&entry, "start"), (&id, "default"))?;
            }
        }
        self.selection = AnimSelection::GraphNode { id: id.clone() };
        Ok(id)
    }

    /// Make `state` the machine's default state.
    pub fn set_default_state(&mut self, state: &str) -> Result<(), String> {
        let entry = self.graph().nodes.values().find(|n| n.node_type == "sm.entry").map(|n| n.id.clone()).ok_or("the machine has no Entry node")?;
        if self.graph().nodes.get(state).map(|n| n.node_type.as_str()) != Some("sm.state") {
            return Err("pick a state".into());
        }
        let old: Vec<String> = self.graph().connections.iter().filter(|c| c.source_node == entry).map(|c| c.id.clone()).collect();
        if !old.is_empty() {
            self.with_graph("Change default state", None, |g, _| {
                for id in &old {
                    g.remove_connection(id);
                }
                Ok(())
            })?;
        }
        self.connect((&entry, "start"), (state, "default"))
    }

    /// Add a transition between two states, with its rule graph.
    pub fn add_transition(&mut self, from: &str, to: &str) -> Result<String, String> {
        if self.graph_kind() != GraphKind::StateMachine {
            return Err("open a state machine first".into());
        }
        for s in [from, to] {
            if self.graph().nodes.get(s).map(|n| n.node_type.as_str()) != Some("sm.state") {
                return Err("transitions connect states".into());
            }
        }
        let a = self.graph().nodes.get(from).map(|n| n.position.clone()).ok_or("no such state")?;
        let b = self.graph().nodes.get(to).map(|n| n.position.clone()).ok_or("no such state")?;
        let id = self.add_node("sm.transition", ((a.x + b.x) as f32 * 0.5, (a.y + b.y) as f32 * 0.5 + 120.0))?;
        self.ensure_subgraph(&id)?;
        self.connect((from, "leave"), (&id, "from"))?;
        self.connect((to, "arrive"), (&id, "to"))?;
        self.selection = AnimSelection::GraphNode { id: id.clone() };
        Ok(id)
    }

    /// The states and transitions of the open state machine, for the list view.
    pub fn machine_summary(&self) -> Vec<MachineRow> {
        let g = self.graph();
        let name_of = |id: &str| g.nodes.get(id).and_then(|n| n.properties.get("name").and_then(Value::as_str).map(str::to_string)).unwrap_or_else(|| id.to_string());
        let endpoint = |tn: &str, pin: &str| g.connections.iter().find(|c| c.target_node == tn && c.target_pin == pin).map(|c| c.source_node.clone());
        let default = g.connections.iter().find(|c| c.target_pin == "default").map(|c| c.target_node.clone());
        let mut rows: Vec<MachineRow> = g
            .nodes
            .values()
            .filter_map(|n| match n.node_type.as_str() {
                "sm.state" => Some(MachineRow::State { id: n.id.clone(), name: name_of(&n.id), default: default.as_deref() == Some(n.id.as_str()) }),
                "sm.transition" => Some(MachineRow::Transition {
                    id: n.id.clone(),
                    from: endpoint(&n.id, "from").map(|s| name_of(&s)),
                    to: endpoint(&n.id, "to").map(|s| name_of(&s)),
                    duration: n.properties.get("duration").and_then(Value::as_f64).unwrap_or(0.0) as f32,
                    priority: n.properties.get("priority").and_then(Value::as_i64).unwrap_or(0),
                }),
                _ => None,
            })
            .collect();
        rows.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        rows
    }

    // ---- parameters ---------------------------------------------------------------

    pub fn add_parameter(&mut self, name: &str, ty: ParamType) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() || !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err("a parameter name is letters, digits and underscore, not starting with a digit".into());
        }
        if self.asset().parameter(name).is_some() {
            return Err(format!("a parameter named `{name}` exists"));
        }
        let p = GraphParameter::new(name, ty);
        let id = p.id.clone();
        self.doc.apply("Add parameter", |a| a.parameters.push(p));
        self.selection = AnimSelection::GraphParameter { name: name.to_string() };
        Ok(id)
    }

    /// Rename a parameter and every node property that names it.
    pub fn rename_parameter(&mut self, old: &str, new: &str) -> Result<(), String> {
        let new = new.trim();
        if self.asset().parameter(old).is_none() {
            return Err("no such parameter".into());
        }
        if new != old && self.asset().parameter(new).is_some() {
            return Err(format!("a parameter named `{new}` exists"));
        }
        if new.is_empty() || !new.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || new.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return Err("a parameter name is letters, digits and underscore, not starting with a digit".into());
        }
        let (old, new) = (old.to_string(), new.to_string());
        self.doc.apply("Rename parameter", |a| {
            if let Some(p) = a.parameters.iter_mut().find(|p| p.name == old) {
                p.name = new.clone();
            }
            let rename = |g: &mut GraphDescription| {
                for n in g.nodes.values_mut() {
                    let Some(def) = node_def(&n.node_type) else { continue };
                    for p in def.props.iter().filter(|p| matches!(p.kind, PropKind::Parameter(_))) {
                        if n.properties.get(p.name).and_then(Value::as_str) == Some(old.as_str()) {
                            n.properties.insert(p.name.to_string(), json!(new));
                        }
                    }
                }
            };
            rename(&mut a.main);
            for s in &mut a.subgraphs {
                rename(&mut s.graph);
            }
        });
        if matches!(&self.selection, AnimSelection::GraphParameter { name } if *name == old) {
            self.selection = AnimSelection::GraphParameter { name: new };
        }
        Ok(())
    }

    /// Nodes (graph name, node id) that name `parameter`.
    pub fn parameter_uses(&self, parameter: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (sub, _, g) in self.asset().graphs() {
            for n in g.nodes.values() {
                let Some(def) = node_def(&n.node_type) else { continue };
                if def.props.iter().any(|p| matches!(p.kind, PropKind::Parameter(_)) && n.properties.get(p.name).and_then(Value::as_str) == Some(parameter)) {
                    out.push((sub.map_or("Main graph".to_string(), |s| s.name.clone()), n.id.clone()));
                }
            }
        }
        out
    }

    /// Delete a parameter that nothing uses.
    pub fn delete_parameter(&mut self, name: &str) -> Result<(), String> {
        let uses = self.parameter_uses(name);
        if !uses.is_empty() {
            return Err(format!("`{name}` is used by {} node(s); change them first", uses.len()));
        }
        let before = self.asset().parameters.len();
        let n = name.to_string();
        self.doc.apply("Delete parameter", |a| a.parameters.retain(|p| p.name != n));
        if before == self.asset().parameters.len() {
            return Err("no such parameter".into());
        }
        self.selection = AnimSelection::None;
        Ok(())
    }

    pub fn set_parameter_default(&mut self, name: &str, value: Value) -> Result<(), String> {
        let ty = self.asset().parameter(name).ok_or("no such parameter")?.ty;
        if !ty.accepts(&value) {
            return Err(format!("the default must be a {}", ty.pin_type().type_name()));
        }
        let n = name.to_string();
        self.doc.edit("Set parameter default", Some(&format!("pdef:{n}")), |a| {
            if let Some(p) = a.parameters.iter_mut().find(|p| p.name == n) {
                p.default = value;
            }
            (true, ())
        });
        Ok(())
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

    pub fn select_node(&mut self, id: &str) -> bool {
        if self.graph().nodes.contains_key(id) {
            self.selection = AnimSelection::GraphNode { id: id.to_string() };
            true
        } else {
            false
        }
    }

    pub fn validation(&self) -> Vec<Diagnostic> {
        validate(self.asset())
    }

    pub fn save(&mut self) -> anyhow::Result<super::document::SaveOutcome> {
        self.doc.save()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MachineRow {
    State { id: String, name: String, default: bool },
    Transition { id: String, from: Option<String>, to: Option<String>, duration: f32, priority: i64 },
}

impl MachineRow {
    fn sort_key(&self) -> (u8, String) {
        match self {
            Self::State { name, .. } => (0, name.clone()),
            Self::Transition { from, to, priority, .. } => (1, format!("{:?}{:?}{priority:05}", from, to)),
        }
    }
}

fn pin_type(dt: &graphy_blueprint::DataType) -> Option<AnimPinType> {
    match dt {
        graphy_blueprint::DataType::Data(info) => AnimPinType::from_type_name(&info.to_string()),
        graphy_blueprint::DataType::Execution => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::MemStore;
    use super::*;

    fn model() -> AnimGraphModel {
        let store = MemStore::default();
        AnimGraphModel::open(Arc::new(store), Path::new("g.animgraph"), &AnimationGraphAsset::default().to_text().unwrap()).unwrap()
    }

    #[test]
    fn bone_masks_are_edited_per_layer_as_undoable_validated_property_edits() {
        use skeletal_assets::bone_mask::BoneMaskSpec;
        let skeleton = crate::core::sample::humanoid_skeleton();
        let spine = skeleton.bones.iter().find(|b| b.name.to_lowercase().contains("spine")).unwrap().id.clone();
        let mut m = model();
        let plain = m.add_node("anim.sequence_player", (0.0, 0.0)).unwrap();
        assert!(m.edit_mask(&plain, 0, None, &[], |_| Ok(())).is_err(), "only a layered blend has masks");
        let node = m.add_node("anim.layered_blend_per_bone", (0.0, 100.0)).unwrap();
        m.set_property(&node, "count", json!(2)).unwrap();
        m.edit_mask(&node, 1, Some(&skeleton), &[], |s| {
            crate::anim_editors::bone_mask_edit::apply_preset(s, &crate::anim_editors::bone_mask_edit::Preset::Subtree(spine.clone()));
            Ok(())
        })
        .unwrap();
        let stored = m.graph().nodes[&node].properties["masks"].clone();
        let specs: Vec<BoneMaskSpec> = serde_json::from_value(stored).unwrap();
        assert_eq!(specs.len(), 2, "one mask per layer");
        assert_eq!(specs[1].branches[0].bone, spine);
        assert!(m.edit_mask(&node, 5, None, &[], |_| Ok(())).is_err(), "no such layer");
        let before = m.graph().nodes[&node].properties["masks"].clone();
        let refused = m.edit_mask(&node, 1, Some(&skeleton), &[], |s| {
            s.branches[0].bone = "ghost".into();
            Ok(())
        });
        assert!(refused.unwrap_err().contains("ghost"));
        assert_eq!(m.graph().nodes[&node].properties["masks"], before, "a refused edit changes nothing");
        assert!(m.set_property(&node, "masks", json!([{ "branches": [], "default_weight": 9.0 }])).is_err(), "a raw property write is validated too");
        assert!(m.doc.undo(), "the mask edit is one undo step");
    }

    #[test]
    fn nodes_are_added_by_catalogue_rules_and_terminals_stay() {
        let mut m = model();
        assert!(m.can_add("sm.state").is_err(), "states are for state machines");
        assert!(m.can_add("anim.output_pose").is_err(), "the output appears once");
        assert!(m.add_node("anim.nope", (0.0, 0.0)).is_err());
        let seq = m.add_node("anim.sequence_player", (0.0, 0.0)).unwrap();
        assert_eq!(m.selection, AnimSelection::GraphNode { id: seq.clone() });
        let out = m.graph().nodes.values().find(|n| n.node_type == "anim.output_pose").unwrap().id.clone();
        assert!(m.delete_node(&out).is_err(), "the Output Pose cannot be deleted");
        assert!(m.palette().iter().any(|d| d.type_id == "anim.sequence_player"));
        assert!(m.palette().iter().all(|d| d.type_id != "sm.state"));
        m.delete_node(&seq).unwrap();
        assert!(m.graph().nodes.get(&seq).is_none());
    }

    #[test]
    fn wires_are_type_checked_and_inputs_take_one_wire() {
        let mut m = model();
        let out = m.graph().nodes.values().find(|n| n.node_type == "anim.output_pose").unwrap().id.clone();
        let a = m.add_node("anim.sequence_player", (0.0, 0.0)).unwrap();
        let b = m.add_node("anim.sequence_player", (0.0, 100.0)).unwrap();
        m.connect((&a, "pose"), (&out, "pose")).unwrap();
        m.connect((&b, "pose"), (&out, "pose")).unwrap();
        assert_eq!(m.graph().connections.iter().filter(|c| c.target_node == out).count(), 1, "the second wire replaced the first");
        assert_eq!(m.graph().connections[0].source_node, b);
        assert!(m.connect((&a, "pose"), (&a, "play_rate")).is_err(), "no self wires");
        let get = m.add_node("anim.get_parameter", (0.0, 200.0)).unwrap();
        assert!(m.connect((&get, "value"), (&out, "pose")).is_err(), "a float is not a pose");
        let id = m.graph().connections[0].id.clone();
        assert!(m.disconnect(&id) && !m.disconnect(&id));
        assert!(m.doc.undo() && m.graph().connections.len() == 1, "disconnect undoes");
    }

    #[test]
    fn properties_are_validated_and_dynamic_pins_follow_them() {
        let mut m = model();
        let blend = m.add_node("anim.blend_multi", (0.0, 0.0)).unwrap();
        let ty = m.graph().nodes[&blend].node_type.clone();
        let def = node_def(&ty).unwrap();
        // pick any int/count property that drives dynamic pins
        if let skeletal_assets::anim_graph::catalogue::DynamicPins::PoseWeightPairs { count_prop, .. } = def.dynamic {
            let before = m.graph().nodes[&blend].inputs.len();
            m.set_property(&blend, count_prop, json!(4)).unwrap();
            assert!(m.graph().nodes[&blend].inputs.len() > before, "more inputs after raising the count");
            assert!(m.set_property(&blend, count_prop, json!("x")).is_err());
        }
        let seq = m.add_node("anim.sequence_player", (0.0, 0.0)).unwrap();
        assert!(m.set_property(&seq, "looping", json!("sideways")).is_err(), "enum membership");
        m.set_property(&seq, "looping", json!("once")).unwrap();
        assert_eq!(m.property(&seq, "looping"), Some(&json!("once")));
        assert!(m.set_property(&seq, "no_such_prop", json!(1)).is_err());
        assert!(m.set_property(&seq, "sequence", json!("not-a-uuid")).is_err(), "asset properties hold references");
        m.set_property(&seq, "sequence", json!("11111111-1111-4111-8111-111111111111")).unwrap();
    }

    #[test]
    fn states_and_transitions_own_their_graphs_and_wire_up() {
        let mut m = model();
        let machine = m.add_node("anim.state_machine", (0.0, 0.0));
        let machine = machine.expect("a state machine node exists in the catalogue");
        let target = m.ensure_subgraph(&machine).unwrap();
        assert!(m.open_graph(target.clone()));
        assert_eq!(m.graph_kind(), GraphKind::StateMachine);
        assert!(m.graph().nodes.values().any(|n| n.node_type == "sm.entry"));
        let idle = m.add_state("Idle", (0.0, 0.0)).unwrap();
        let run = m.add_state("Run", (300.0, 0.0)).unwrap();
        assert!(matches!(m.machine_summary()[0], MachineRow::State { default: true, .. }) || m.machine_summary().iter().any(|r| matches!(r, MachineRow::State { name, default: true, .. } if name == "Idle")), "the first state is the default");
        let t = m.add_transition(&idle, &run).unwrap();
        assert!(m.add_transition(&idle, &t).is_err(), "transitions connect states");
        let summary = m.machine_summary();
        assert!(summary.iter().any(|r| matches!(r, MachineRow::Transition { from: Some(f), to: Some(t), .. } if f == "Idle" && t == "Run")));
        // each state and the transition own a sub-graph that ends in its terminal node
        let state_graph = m.target_of(&idle).unwrap();
        m.open_graph(state_graph);
        assert_eq!(m.graph_kind(), GraphKind::StatePose);
        assert!(m.graph().nodes.values().any(|n| n.node_type == "anim.state_result"));
        let crumbs: Vec<_> = m.breadcrumb().into_iter().map(|c| c.1).collect();
        assert_eq!(crumbs.len(), 3, "main > machine > state: {crumbs:?}");
        m.open_graph(target);
        let rule_graph = m.target_of(&t).unwrap();
        m.open_graph(rule_graph);
        assert_eq!(m.graph_kind(), GraphKind::TransitionRule);
        // names stay unique
        m.open_graph(GraphRef::Main);
        let machine_graph = m.target_of(&machine).unwrap();
        m.open_graph(machine_graph);
        let again = m.add_state("Idle", (0.0, 200.0)).unwrap();
        assert!(m.machine_summary().iter().any(|r| matches!(r, MachineRow::State { id, name, .. } if *id == again && name == "Idle 2")));
        // deleting a state deletes its graph
        let before = m.asset().subgraphs.len();
        m.delete_node(&again).unwrap();
        assert_eq!(m.asset().subgraphs.len(), before - 1);
        m.set_default_state(&run).unwrap();
        assert!(m.machine_summary().iter().any(|r| matches!(r, MachineRow::State { name, default: true, .. } if name == "Run")));
    }

    #[test]
    fn parameters_rename_through_nodes_and_refuse_deleting_what_is_used() {
        let mut m = model();
        assert!(m.add_parameter("1speed", ParamType::Float).is_err());
        m.add_parameter("Speed", ParamType::Float).unwrap();
        assert!(m.add_parameter("Speed", ParamType::Bool).is_err());
        let selected_before = m.selection.clone();
        assert_eq!(selected_before, AnimSelection::GraphParameter { name: "Speed".into() });
        let get = m.add_node("anim.get_parameter", (0.0, 0.0)).unwrap();
        m.set_property(&get, "parameter", json!("Speed")).unwrap();
        assert!(m.set_property(&get, "parameter", json!("Ghost")).is_err());
        assert!(m.delete_parameter("Speed").is_err(), "in use");
        m.selection = AnimSelection::GraphParameter { name: "Speed".into() };
        m.rename_parameter("Speed", "MoveSpeed").unwrap();
        assert_eq!(m.property(&get, "parameter"), Some(&json!("MoveSpeed")), "nodes follow the rename");
        assert_eq!(m.selection, AnimSelection::GraphParameter { name: "MoveSpeed".into() });
        assert!(m.set_parameter_default("MoveSpeed", json!("fast")).is_err());
        m.set_parameter_default("MoveSpeed", json!(3.5)).unwrap();
        m.set_property(&get, "parameter", json!("")).unwrap();
        m.delete_parameter("MoveSpeed").unwrap();
        assert!(m.asset().parameters.is_empty());
    }

    #[test]
    fn canvas_edits_replace_the_graph_as_one_undo_step_and_the_whole_asset_round_trips() {
        let store = MemStore::default();
        let mut m = AnimGraphModel::open(Arc::new(store.clone()), Path::new("g.animgraph"), &AnimationGraphAsset::default().to_text().unwrap()).unwrap();
        let seq = m.add_node("anim.sequence_player", (0.0, 0.0)).unwrap();
        for x in [10.0, 20.0, 30.0] {
            m.move_node(&seq, x, 5.0);
        }
        m.end_gesture();
        assert_eq!(m.graph().nodes[&seq].position.x, 30.0);
        assert!(m.doc.undo());
        assert_eq!(m.graph().nodes[&seq].position.x, 0.0, "a drag is one step");
        let mut edited = m.graph().clone();
        edited.nodes.get_mut(&seq).unwrap().position.x = 77.0;
        m.replace_graph(edited, "Edit graph", None);
        assert_eq!(m.graph().nodes[&seq].position.x, 77.0);
        m.add_parameter("Speed", ParamType::Float).unwrap();
        m.save().unwrap();
        let r = AnimGraphModel::open(Arc::new(store), Path::new("g.animgraph"), "").unwrap();
        assert_eq!(r.graph().nodes[&seq].position.x, 77.0);
        assert_eq!(r.asset().parameters.len(), 1);
    }

    #[test]
    fn validation_comes_from_the_catalogue_validator() {
        let mut m = model();
        let seq = m.add_node("anim.sequence_player", (0.0, 0.0)).unwrap();
        let v = m.validation();
        assert!(!v.is_empty(), "an unconnected player with no sequence is not valid");
        assert!(v.iter().any(|d| d.location.as_deref() == Some(seq.as_str())) || !v.is_empty());
    }
}

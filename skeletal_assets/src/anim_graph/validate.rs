//! Animation graph validation: what makes a `.animgraph` document evaluable.
//!
//! Checks node types against the [`catalogue`], pins and wire types, required
//! inputs, property values, parameter and cached-pose references, sub-graph
//! structure, state machine topology and cycles. Every finding names the
//! graph and node and, where a mechanical fix exists, says what it is.
//! Validation never edits the document.

use std::collections::{HashMap, HashSet};

use graphy_blueprint::{Connection, ConnectionType, DataType, NodeInstance};
use serde_json::Value;

use super::catalogue::{node_def, resolve_pins, GraphKind, NodeDef, ParamType, PinSpec, PropDef, PropKind};
use super::{AnimSubGraph, AnimationGraphAsset};
use crate::bone_mask::BoneMaskSpec;
use crate::diagnostics::Diagnostic;
use crate::skeleton::Skeleton;

/// A typed asset reference found in a node property.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphAssetRef {
    /// `AssetKind::KIND` of the target.
    pub kind: &'static str,
    pub id: String,
    pub graph: String,
    pub node: String,
    pub property: &'static str,
}

fn label(sub: Option<&AnimSubGraph>) -> String {
    sub.map_or_else(|| "main".to_string(), |s| s.id.clone())
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn text(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| !s.trim().is_empty())
}

/// Every typed asset reference in the graph's nodes, for dependency indexing.
pub fn asset_references(asset: &AnimationGraphAsset) -> Vec<GraphAssetRef> {
    let mut out = Vec::new();
    for (sub, _, graph) in asset.graphs() {
        for node in graph.nodes.values() {
            let Some(def) = node_def(&node.node_type) else { continue };
            for prop in def.props {
                if let PropKind::Asset(kind) = prop.kind {
                    if let Some(id) = node.properties.get(prop.name).and_then(text) {
                        out.push(GraphAssetRef {
                            kind,
                            id: id.to_string(),
                            graph: label(sub),
                            node: node.id.clone(),
                            property: prop.name,
                        });
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| (&a.graph, &a.node, a.property).cmp(&(&b.graph, &b.node, b.property)));
    out
}

pub fn validate(asset: &AnimationGraphAsset) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let params = asset.param_types();

    for parameter in &asset.parameters {
        if !is_identifier(&parameter.name) {
            out.push(
                Diagnostic::error("graph.parameter_name", format!("Parameter `{}` needs an identifier name", parameter.name))
                    .at(&parameter.id),
            );
        }
    }

    for (sub, kind, graph) in asset.graphs() {
        let graph_label = label(sub);
        validate_graph(asset, &params, &graph_label, kind, graph, &mut out);
    }

    // Sub-graph wiring: every reference resolves to the right kind; unused
    // sub-graphs are reported; layers may not run themselves.
    let mut referenced: HashMap<&str, usize> = HashMap::new();
    let mut layer_edges: HashMap<String, Vec<String>> = HashMap::new();
    for (sub, _, graph) in asset.graphs() {
        for node in graph.nodes.values() {
            let Some(def) = node_def(&node.node_type) else { continue };
            for prop in def.props {
                let PropKind::SubGraph(want) = prop.kind else { continue };
                let Some(id) = node.properties.get(prop.name).and_then(text) else { continue };
                let at = format!("{}/{}", label(sub), node.id);
                match asset.subgraph(id) {
                    None => out.push(
                        Diagnostic::error("graph.missing_subgraph", format!("`{}` points at a graph that does not exist", prop.name))
                            .at(at)
                            .repair("Create the graph or clear the property"),
                    ),
                    Some(target) if target.kind != want => out.push(
                        Diagnostic::error("graph.wrong_subgraph_kind", format!("`{}` needs a {want:?} graph but `{}` is {:?}", prop.name, target.name, target.kind))
                            .at(at),
                    ),
                    Some(target) => {
                        *referenced.entry(target.id.as_str()).or_default() += 1;
                        if want == GraphKind::Layer {
                            layer_edges.entry(label(sub)).or_default().push(target.id.clone());
                        }
                    }
                }
            }
        }
    }
    for sub in &asset.subgraphs {
        match referenced.get(sub.id.as_str()) {
            None => out.push(
                Diagnostic::warning("graph.unused_subgraph", format!("Graph `{}` is not used by any node", sub.name))
                    .at(&sub.id)
                    .repair("Delete the graph or reference it from a node"),
            ),
            Some(n) if *n > 1 && matches!(sub.kind, GraphKind::StateMachine | GraphKind::StatePose) => out.push(
                Diagnostic::warning("graph.shared_stateful_subgraph", format!("{:?} graph `{}` is used {n} times and its instances would share intent but not state", sub.kind, sub.name))
                    .at(&sub.id),
            ),
            _ => {}
        }
    }
    if let Some(cycle) = find_cycle(&layer_edges) {
        out.push(Diagnostic::error("graph.layer_cycle", format!("Layers run each other in a loop: {}", cycle.join(" -> "))).at(&cycle[0]));
    }

    if asset.skeleton.is_none() {
        let needs: Vec<_> = asset
            .graphs()
            .flat_map(|(sub, _, g)| g.nodes.values().map(move |n| (label(sub), n)))
            .filter(|(_, n)| node_def(&n.node_type).is_some_and(|d| d.eval.needs_skeleton))
            .map(|(l, n)| format!("{l}/{}", n.id))
            .collect();
        if let Some(first) = needs.first() {
            out.push(
                Diagnostic::error("graph.skeleton_required", format!("{} node(s) need a skeleton and the graph has none", needs.len()))
                    .at(first.clone())
                    .repair("Pick the graph's skeleton"),
            );
        }
    }
    out
}

fn validate_graph(
    asset: &AnimationGraphAsset,
    params: &HashMap<String, ParamType>,
    graph_label: &str,
    kind: GraphKind,
    graph: &graphy_blueprint::GraphDescription,
    out: &mut Vec<Diagnostic>,
) {
    let at = |node: &str| format!("{graph_label}/{node}");
    let mut node_ids: Vec<&String> = graph.nodes.keys().collect();
    node_ids.sort();

    // expected pins per node, for wire checking
    let mut pins: HashMap<&str, (Vec<PinSpec>, Vec<PinSpec>)> = HashMap::new();
    let mut known: HashMap<&str, &'static NodeDef> = HashMap::new();
    let mut terminals = 0;

    for id in &node_ids {
        let node = &graph.nodes[*id];
        let Some(def) = node_def(&node.node_type) else {
            out.push(Diagnostic::error("graph.unknown_node", format!("Node type `{}` is not an animation node", node.node_type)).at(at(&node.id)).repair("Delete the node"));
            continue;
        };
        known.insert(node.id.as_str(), def);
        if !def.graphs.contains(&kind) {
            out.push(Diagnostic::error("graph.wrong_graph_kind", format!("{} cannot be placed in a {kind:?} graph", def.title)).at(at(&node.id)));
        }
        if node.node_type == kind.terminal() {
            terminals += 1;
        }
        check_properties(asset, params, def, node, &at(&node.id), out);
        let expected = resolve_pins(def, &node.properties, |n| params.get(n).copied());
        check_pins(def, node, &expected, &at(&node.id), out);
        pins.insert(node.id.as_str(), expected);
    }
    if terminals != 1 {
        let term = super::catalogue::node_def(kind.terminal()).map_or(kind.terminal(), |d| d.title);
        out.push(
            Diagnostic::error(
                if terminals == 0 { "graph.missing_terminal" } else { "graph.duplicate_terminal" },
                format!("The {kind:?} graph needs exactly one {term} node and has {terminals}"),
            )
            .at(graph_label.to_string()),
        );
    }

    // wires
    let mut fed: HashMap<(&str, &str), usize> = HashMap::new();
    let mut valid: Vec<&Connection> = Vec::new();
    for c in &graph.connections {
        let (Some(src), Some(dst)) = (graph.nodes.get(&c.source_node), graph.nodes.get(&c.target_node)) else {
            out.push(Diagnostic::error("graph.dangling_wire", "A wire ends at a node that does not exist").at(format!("{graph_label}/{}", c.id)).repair("Delete the wire"));
            continue;
        };
        let out_pin = src.outputs.iter().find(|p| p.id == c.source_pin);
        let in_pin = dst.inputs.iter().find(|p| p.id == c.target_pin);
        let (Some(out_pin), Some(in_pin)) = (out_pin, in_pin) else {
            out.push(Diagnostic::error("graph.dangling_wire", format!("Wire {} uses a pin that does not exist", c.id)).at(format!("{graph_label}/{}", c.id)).repair("Delete the wire"));
            continue;
        };
        if !matches!(c.connection_type, ConnectionType::Data) || matches!(out_pin.pin.data_type, DataType::Execution) {
            out.push(Diagnostic::error("graph.exec_wire", "Animation graphs have data wires only").at(format!("{graph_label}/{}", c.id)));
            continue;
        }
        if !out_pin.pin.data_type.is_compatible_with(&in_pin.pin.data_type) {
            out.push(
                Diagnostic::error(
                    "graph.pin_type_mismatch",
                    format!("{} -> {}: {} does not connect to {}", c.source_pin, c.target_pin, out_pin.pin.data_type, in_pin.pin.data_type),
                )
                .at(at(&c.target_node))
                .repair("Insert a conversion node or use a pin of the same type"),
            );
            continue;
        }
        *fed.entry((c.target_node.as_str(), c.target_pin.as_str())).or_default() += 1;
        valid.push(c);
    }
    for ((node, pin), n) in &fed {
        if *n > 1 {
            out.push(Diagnostic::error("graph.multiple_inputs", format!("Input `{pin}` has {n} wires but takes one")).at(at(node)).repair("Delete all but one wire"));
        }
    }
    for id in &node_ids {
        let node = &graph.nodes[*id];
        let Some((inputs, _)) = pins.get(node.id.as_str()) else { continue };
        for pin in inputs.iter().filter(|p| p.required) {
            if !fed.contains_key(&(node.id.as_str(), pin.name.as_str())) {
                out.push(Diagnostic::error("graph.unconnected_input", format!("Required input `{}` is not connected", pin.name)).at(at(&node.id)));
            }
        }
    }

    // dependency graph: a node depends on whatever feeds it
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    if kind != GraphKind::StateMachine {
        for c in &valid {
            deps.entry(c.target_node.clone()).or_default().push(c.source_node.clone());
        }
    }
    // cached poses
    let mut saves: HashMap<&str, &NodeInstance> = HashMap::new();
    for id in &node_ids {
        let node = &graph.nodes[*id];
        if node.node_type == "anim.save_cached_pose" {
            if let Some(name) = node.properties.get("name").and_then(text) {
                if saves.insert(name, node).is_some() {
                    out.push(Diagnostic::error("graph.duplicate_cached_pose", format!("Cached pose `{name}` is saved twice")).at(at(&node.id)));
                }
            }
        }
    }
    let mut used_saves: HashSet<&str> = HashSet::new();
    for id in &node_ids {
        let node = &graph.nodes[*id];
        if node.node_type == "anim.use_cached_pose" {
            if let Some(name) = node.properties.get("name").and_then(text) {
                match saves.get(name) {
                    Some(save) => {
                        used_saves.insert(name);
                        deps.entry(node.id.clone()).or_default().push(save.id.clone());
                    }
                    None => out.push(Diagnostic::error("graph.missing_cached_pose", format!("No Save Cached Pose named `{name}` in this graph")).at(at(&node.id))),
                }
            }
        }
    }
    if kind != GraphKind::StateMachine {
        if let Some(cycle) = find_cycle(&deps) {
            out.push(Diagnostic::error("graph.cycle", format!("Nodes feed each other in a loop: {}", cycle.join(" -> "))).at(at(&cycle[0])).repair("Break the loop; use a cached pose to reuse a result"));
        }
        // reachability from the terminal
        let mut reached: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = graph.nodes.values().filter(|n| n.node_type == kind.terminal()).map(|n| n.id.clone()).collect();
        while let Some(n) = stack.pop() {
            if reached.insert(n.clone()) {
                stack.extend(deps.get(&n).cloned().unwrap_or_default());
            }
        }
        for id in &node_ids {
            if !reached.contains(id.as_str()) {
                let node = &graph.nodes[*id];
                let is_save = node.node_type == "anim.save_cached_pose";
                let name = node.properties.get("name").and_then(text);
                let code = if is_save { "graph.unused_cached_pose" } else { "graph.unused_node" };
                if is_save && name.is_some_and(|n| used_saves.contains(n)) {
                    continue;
                }
                out.push(Diagnostic::warning(code, "Node does not contribute to the graph output").at(at(&node.id)).repair("Connect it or delete it"));
            }
        }
    } else {
        validate_state_machine(graph_label, graph, &valid, &known, out);
    }
}

fn check_pins(def: &NodeDef, node: &NodeInstance, expected: &(Vec<PinSpec>, Vec<PinSpec>), at: &str, out: &mut Vec<Diagnostic>) {
    for (actual, wanted, dir) in [(&node.inputs, &expected.0, "input"), (&node.outputs, &expected.1, "output")] {
        for spec in wanted {
            match actual.iter().find(|p| p.id == spec.name) {
                None => out.push(Diagnostic::error("graph.pin_mismatch", format!("{} is missing {dir} pin `{}`", def.title, spec.name)).at(at).repair("Refresh the node's pins")),
                Some(p) if p.pin.data_type != spec.ty.data_type() => out.push(
                    Diagnostic::error("graph.pin_mismatch", format!("{dir} pin `{}` should be {}", spec.name, spec.ty.type_name())).at(at).repair("Refresh the node's pins"),
                ),
                Some(_) => {}
            }
        }
        for p in actual {
            if !wanted.iter().any(|s| s.name == p.id) {
                out.push(Diagnostic::error("graph.pin_mismatch", format!("{} has an unexpected {dir} pin `{}`", def.title, p.id)).at(at).repair("Refresh the node's pins"));
            }
        }
    }
}

fn check_properties(
    asset: &AnimationGraphAsset,
    params: &HashMap<String, ParamType>,
    def: &NodeDef,
    node: &NodeInstance,
    at: &str,
    out: &mut Vec<Diagnostic>,
) {
    for prop in def.props {
        let value = node.properties.get(prop.name).filter(|v| !v.is_null());
        let unset = value.is_none_or(|v| v.as_str().is_some_and(|s| s.trim().is_empty()));
        if unset {
            if prop.required {
                out.push(Diagnostic::error("graph.missing_property", format!("{} needs `{}`", def.title, prop.name)).at(at));
            }
            continue;
        }
        if let Err(message) = check_value(asset, params, prop, value.expect("set"), node) {
            out.push(Diagnostic::error("graph.bad_property", format!("`{}`: {message}", prop.name)).at(at));
        }
    }
    match def.type_id {
        "anim.retarget" => {
            let set = |n: &str| node.properties.get(n).and_then(text).is_some();
            if set("profile") == set("ik_retargeter") {
                out.push(Diagnostic::error("graph.retarget_source", "Set exactly one of a retarget profile or an IK retargeter").at(at));
            }
        }
        "anim.linked_graph" => {
            if node.properties.get("graph").and_then(text) == Some(asset.id.as_str()) {
                out.push(Diagnostic::error("graph.self_link", "A graph cannot link itself").at(at));
            }
        }
        "anim.layered_blend_per_bone" => {
            let count = node.properties.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
            let masks = node.properties.get("masks").and_then(Value::as_array).map_or(0, Vec::len);
            if masks != count {
                out.push(Diagnostic::error("graph.mask_count", format!("{count} layer(s) need {count} mask(s) but there are {masks}")).at(at).repair("Add or remove masks to match the layer count"));
            }
        }
        _ => {}
    }
}

fn check_value(
    _asset: &AnimationGraphAsset,
    params: &HashMap<String, ParamType>,
    prop: &PropDef,
    value: &Value,
    node: &NodeInstance,
) -> Result<(), String> {
    match prop.kind {
        PropKind::Float { min, max } => {
            let v = value.as_f64().ok_or("must be a number")?;
            if !v.is_finite() || v < min as f64 || v > max as f64 {
                return Err(format!("must be between {min} and {max}"));
            }
        }
        PropKind::Int { min, max } => {
            let v = value.as_i64().ok_or("must be an integer")?;
            if v < min || v > max {
                return Err(format!("must be between {min} and {max}"));
            }
        }
        PropKind::Bool => {
            value.as_bool().ok_or("must be true or false")?;
        }
        PropKind::Text => {
            value.as_str().ok_or("must be text")?;
        }
        PropKind::Enum(options) => {
            let v = value.as_str().ok_or("must be text")?;
            if !options.contains(&v) {
                return Err(format!("`{v}` is not one of {}", options.join(", ")));
            }
        }
        PropKind::Bone | PropKind::Curve | PropKind::CachedPose | PropKind::SyncGroup => {
            value.as_str().ok_or("must be text")?;
        }
        PropKind::Slot => {
            let v = value.as_str().ok_or("must be text")?;
            if !is_identifier(v) {
                return Err("slot names are identifiers".into());
            }
        }
        PropKind::Parameter(_) => {
            let v = value.as_str().ok_or("must be text")?;
            if !params.contains_key(v) {
                return Err(format!("no parameter named `{v}`"));
            }
        }
        PropKind::Asset(_) => {
            let v = value.as_str().ok_or("must be text")?;
            if uuid::Uuid::parse_str(v).is_err() {
                return Err("must be an asset id".into());
            }
        }
        PropKind::SubGraph(_) => {
            value.as_str().ok_or("must be a graph id")?;
        }
        PropKind::BoneMask => {
            let masks = value.as_array().ok_or("must be an array of masks")?;
            for mask in masks {
                let spec: BoneMaskSpec = serde_json::from_value(mask.clone()).map_err(|e| e.to_string())?;
                spec.validate()?;
            }
        }
        PropKind::BoneList => {
            let list = value.as_array().ok_or("must be an array")?;
            if list.iter().any(|v| text(v).is_none()) {
                return Err("bone ids must be text".into());
            }
        }
        PropKind::NameList => {
            let list = value.as_array().ok_or("must be an array")?;
            let mut seen = HashSet::new();
            for v in list {
                let name = v.as_str().filter(|s| is_identifier(s)).ok_or("names must be identifiers")?;
                if !seen.insert(name) {
                    return Err(format!("`{name}` is listed twice"));
                }
            }
        }
        PropKind::Vector => {
            if !ParamType::Vector.accepts(value) {
                return Err("must be [x, y, z]".into());
            }
        }
    }
    let _ = node;
    Ok(())
}

fn validate_state_machine(
    graph_label: &str,
    graph: &graphy_blueprint::GraphDescription,
    wires: &[&Connection],
    known: &HashMap<&str, &'static NodeDef>,
    out: &mut Vec<Diagnostic>,
) {
    let at = |node: &str| format!("{graph_label}/{node}");
    let type_of = |id: &str| known.get(id).map(|d| d.type_id);
    let states: Vec<&NodeInstance> = graph.nodes.values().filter(|n| n.node_type == "sm.state").collect();
    let mut names = HashSet::new();
    for s in &states {
        match s.properties.get("name").and_then(text) {
            Some(n) if !names.insert(n.to_string()) => out.push(Diagnostic::error("sm.duplicate_state", format!("Two states are named `{n}`")).at(at(&s.id))),
            _ => {}
        }
    }
    // entry
    let entry_targets: Vec<&&Connection> = wires.iter().filter(|c| type_of(&c.source_node) == Some("sm.entry")).collect();
    match entry_targets.as_slice() {
        [] => out.push(Diagnostic::error("sm.no_default_state", "Entry is not connected to a state").at(graph_label.to_string()).repair("Connect Entry to the default state")),
        [only] if type_of(&only.target_node) != Some("sm.state") || only.target_pin != "default" => {
            out.push(Diagnostic::error("sm.bad_entry", "Entry must connect to a state").at(at(&only.target_node)))
        }
        [_] => {}
        _ => out.push(Diagnostic::error("sm.multiple_defaults", "Entry connects to more than one state").at(graph_label.to_string())),
    }
    // transitions
    let mut by_source: HashMap<&str, Vec<(i64, &str)>> = HashMap::new();
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    for t in graph.nodes.values().filter(|n| n.node_type == "sm.transition") {
        let from = wires.iter().find(|c| c.target_node == t.id && c.target_pin == "from");
        let to = wires.iter().find(|c| c.target_node == t.id && c.target_pin == "to");
        // Missing wires are reported as unconnected required inputs.
        let (Some(from), Some(to)) = (from, to) else { continue };
        if type_of(&from.source_node) != Some("sm.state")
            || type_of(&to.source_node) != Some("sm.state")
            || from.source_pin != "leave"
            || to.source_pin != "arrive"
        {
            out.push(Diagnostic::error("sm.bad_transition_endpoint", "A transition's `from` takes a state's `leave` pin and its `to` takes a state's `arrive` pin").at(at(&t.id)));
            continue;
        }
        let allow = t.properties.get("allow_reentry").and_then(Value::as_bool).unwrap_or(false);
        if from.source_node == to.source_node && !allow {
            out.push(Diagnostic::error("sm.self_transition", "A transition to the same state needs allow_reentry").at(at(&t.id)).repair("Enable allow_reentry or retarget the transition"));
        }
        let priority = t.properties.get("priority").and_then(Value::as_i64).unwrap_or(0);
        by_source.entry(from.source_node.as_str()).or_default().push((priority, t.id.as_str()));
        edges.entry(from.source_node.clone()).or_default().push(to.source_node.clone());
    }
    for (source, list) in &by_source {
        let mut seen: HashMap<i64, &str> = HashMap::new();
        for (priority, id) in list {
            if let Some(other) = seen.insert(*priority, id) {
                out.push(
                    Diagnostic::warning("sm.ambiguous_priority", format!("Transitions {other} and {id} leave the same state with priority {priority}; the winner would depend on wire order"))
                        .at(at(source))
                        .repair("Give the transitions different priorities"),
                );
            }
        }
    }
    // reachability
    if let [only] = entry_targets.as_slice() {
        let mut reached: HashSet<&str> = HashSet::new();
        let mut stack = vec![only.target_node.as_str()];
        while let Some(s) = stack.pop() {
            if reached.insert(s) {
                for n in edges.get(s).into_iter().flatten() {
                    stack.push(n.as_str());
                }
            }
        }
        for s in &states {
            if !reached.contains(s.id.as_str()) {
                out.push(Diagnostic::warning("sm.unreachable_state", "No transition path from the default state reaches this state").at(at(&s.id)).repair("Add a transition into it or delete it"));
            }
        }
    }
}

/// A cycle in a directed graph given as adjacency lists, as a node path that
/// ends where it began, or `None`. Deterministic (nodes visited in sorted order).
pub fn find_cycle(edges: &HashMap<String, Vec<String>>) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    fn visit(n: &str, edges: &HashMap<String, Vec<String>>, marks: &mut HashMap<String, Mark>, path: &mut Vec<String>) -> Option<Vec<String>> {
        match marks.get(n) {
            Some(Mark::Done) => return None,
            Some(Mark::Visiting) => {
                let start = path.iter().position(|p| p == n).unwrap_or(0);
                let mut cycle = path[start..].to_vec();
                cycle.push(n.to_string());
                return Some(cycle);
            }
            None => {}
        }
        marks.insert(n.to_string(), Mark::Visiting);
        path.push(n.to_string());
        let mut next: Vec<&String> = edges.get(n).into_iter().flatten().collect();
        next.sort();
        for m in next {
            if let Some(c) = visit(m, edges, marks, path) {
                return Some(c);
            }
        }
        path.pop();
        marks.insert(n.to_string(), Mark::Done);
        None
    }
    let mut keys: Vec<&String> = edges.keys().collect();
    keys.sort();
    let mut marks = HashMap::new();
    for k in keys {
        if let Some(c) = visit(k, edges, &mut marks, &mut Vec::new()) {
            return Some(c);
        }
    }
    None
}

/// Checks that need the graph's skeleton: bone properties and masks name real
/// bones, and IK chains are shaped as the solver needs.
pub fn validate_against_skeleton(asset: &AnimationGraphAsset, skeleton: &Skeleton) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (sub, _, graph) in asset.graphs() {
        let mut ids: Vec<&String> = graph.nodes.keys().collect();
        ids.sort();
        for id in ids {
            let node = &graph.nodes[id];
            let Some(def) = node_def(&node.node_type) else { continue };
            let at = format!("{}/{}", label(sub), node.id);
            for prop in def.props {
                match prop.kind {
                    PropKind::Bone => {
                        if let Some(bone) = node.properties.get(prop.name).and_then(text) {
                            if skeleton.bone(bone).is_none() {
                                out.push(Diagnostic::error("graph.unknown_bone", format!("`{}` names bone `{bone}` which the skeleton does not have", prop.name)).at(&at).repair("Pick a bone of the graph's skeleton"));
                            }
                        }
                    }
                    PropKind::BoneMask => {
                        for (i, mask) in node.properties.get(prop.name).and_then(Value::as_array).into_iter().flatten().enumerate() {
                            if let Ok(spec) = serde_json::from_value::<BoneMaskSpec>(mask.clone()) {
                                for bone in spec.missing_bones(skeleton) {
                                    out.push(Diagnostic::error("graph.unknown_bone", format!("Mask {i} names bone `{bone}` which the skeleton does not have")).at(&at));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            let bone = |p: &str| node.properties.get(p).and_then(text).and_then(|b| skeleton.bone(b));
            match def.type_id {
                "anim.two_bone_ik" => {
                    if let Some(end) = bone("end_bone") {
                        let has_grandparent = end.parent.as_deref().and_then(|p| skeleton.bone(p)).and_then(|p| p.parent.as_deref()).is_some();
                        if !has_grandparent {
                            out.push(Diagnostic::error("graph.ik_chain", "Two Bone IK needs the end bone to have a parent and a grandparent").at(&at));
                        }
                    }
                }
                "anim.fabrik" => {
                    if let (Some(tip), Some(root)) = (bone("tip_bone"), bone("root_bone")) {
                        let mut cursor = Some(tip.id.as_str());
                        let mut found = false;
                        while let Some(c) = cursor {
                            if c == root.id {
                                found = true;
                                break;
                            }
                            cursor = skeleton.bone(c).and_then(|b| b.parent.as_deref());
                        }
                        if !found || tip.id == root.id {
                            out.push(Diagnostic::error("graph.ik_chain", "FABRIK root bone must be a proper ancestor of the tip bone").at(&at));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Nodes (as `(graph label, node id)`) whose poses are over a Retarget node's
/// *source* skeleton: everything upstream of a Retarget `source` input. Their
/// sequences belong to the profile's source skeleton, not the graph's, so
/// dependency checks must not hold them to the graph's skeleton.
pub fn retarget_source_nodes(asset: &AnimationGraphAsset) -> HashSet<(String, String)> {
    let mut out = HashSet::new();
    for (sub, _, graph) in asset.graphs() {
        let graph_label = label(sub);
        let mut stack: Vec<String> = graph
            .connections
            .iter()
            .filter(|c| c.target_pin == "source" && graph.nodes.get(&c.target_node).is_some_and(|n| n.node_type == "anim.retarget"))
            .map(|c| c.source_node.clone())
            .collect();
        while let Some(node) = stack.pop() {
            if !out.insert((graph_label.clone(), node.clone())) {
                continue;
            }
            stack.extend(graph.connections.iter().filter(|c| c.target_node == node).map(|c| c.source_node.clone()));
            let Some(n) = graph.nodes.get(&node) else { continue };
            if n.node_type == "anim.use_cached_pose" {
                let name = n.properties.get("name").and_then(Value::as_str);
                stack.extend(
                    graph
                        .nodes
                        .values()
                        .filter(|s| s.node_type == "anim.save_cached_pose" && s.properties.get("name").and_then(Value::as_str) == name)
                        .map(|s| s.id.clone()),
                );
            }
        }
    }
    out
}

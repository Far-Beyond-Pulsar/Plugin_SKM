//! Reading the version 1 `.animgraph` schema: `nodes` of Sequence, BlendSpace
//! and Output with string ids, and `connections` of bare `from`/`to` node ids.
//!
//! The asset id, skeleton, node ids and node names survive. Each v1 node
//! becomes the catalogue node of the same meaning; a v1 connection becomes a
//! `pose` wire. v1 allowed things the pose model does not (a pose into a
//! sequence, several poses into the output); those connections are dropped, and
//! every dropped one is reported and kept under `extra.migrated_dropped_connections`
//! so nothing the user drew is lost silently.

use std::collections::HashMap;

use graphy_blueprint::GraphDescription;
use pulsar_reflection::{AnimationGraphAssetKind, AssetRef};
use serde_json::{json, Value};

use super::{data_connection, instantiate_node, AnimationGraphAsset, ANIM_GRAPH_VERSION};
use crate::blend_space::Migration;
use crate::codec::{CodecError, Extra};

pub fn from_v1(value: Value) -> Result<(AnimationGraphAsset, Migration), CodecError> {
    let Value::Object(object) = value else {
        return Err(CodecError::Malformed("expected a JSON object".into()));
    };
    let mut migration = Migration { from_version: 1, ..Default::default() };

    let id = match object.get("id").and_then(Value::as_str) {
        Some(text) => AssetRef::<AnimationGraphAssetKind>::parse(text)
            .ok_or_else(|| CodecError::Invalid("asset id is not a UUID".into()))?,
        None => {
            migration.id_assigned = true;
            migration.notes.push("File had no id; one was assigned and must be saved".into());
            AssetRef::new()
        }
    };
    let name = object.get("name").and_then(Value::as_str).unwrap_or("Animation Graph").to_string();
    let skeleton = match object.get("skeleton") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .and_then(AssetRef::parse)
                .ok_or_else(|| CodecError::Invalid("skeleton is not a UUID".into()))?,
        ),
    };

    let old_nodes = object.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
    let old_edges = object.get("connections").and_then(Value::as_array).cloned().unwrap_or_default();

    // Layout: depth from the output along incoming edges, so the chain reads
    // left to right.
    let mut incoming: HashMap<String, Vec<String>> = HashMap::new();
    for edge in &old_edges {
        if let (Some(from), Some(to)) = (edge.get("from").and_then(Value::as_str), edge.get("to").and_then(Value::as_str)) {
            incoming.entry(to.to_string()).or_default().push(from.to_string());
        }
    }
    let mut depth: HashMap<String, usize> = HashMap::new();
    let mut frontier: Vec<String> = old_nodes
        .iter()
        .filter(|n| n.get("type").and_then(Value::as_str) == Some("output"))
        .filter_map(|n| n.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let mut level = 0;
    while !frontier.is_empty() && level < old_nodes.len() + 1 {
        let mut next = Vec::new();
        for id in frontier {
            depth.entry(id.clone()).or_insert(level);
            for from in incoming.get(&id).into_iter().flatten() {
                next.push(from.clone());
            }
        }
        frontier = next;
        level += 1;
    }
    let max_depth = depth.values().copied().max().unwrap_or(0);

    let mut main = GraphDescription::new(&name);
    let mut rows: HashMap<usize, usize> = HashMap::new();
    let mut kinds: HashMap<String, &'static str> = HashMap::new();
    let mut seen_ids = std::collections::HashSet::new();

    for node in &old_nodes {
        let node_id = node.get("id").and_then(Value::as_str).ok_or_else(|| CodecError::Invalid("a node has no id".into()))?;
        if !seen_ids.insert(node_id.to_string()) {
            return Err(CodecError::Invalid(format!("duplicate node id `{node_id}`")));
        }
        let kind = node.get("type").and_then(Value::as_str).unwrap_or_default();
        let type_id = match kind {
            "sequence" => "anim.sequence_player",
            "blend_space" => "anim.blend_space_player",
            "output" => "anim.output_pose",
            other => return Err(CodecError::Invalid(format!("unknown v1 node type `{other}`"))),
        };
        // Unreached nodes go in a column left of the chain.
        let column = depth.get(node_id).map_or(max_depth + 1, |d| *d);
        let row = rows.entry(column).or_insert(0);
        let position = ((max_depth + 1 - column.min(max_depth + 1)) as f32 * 280.0, *row as f32 * 150.0);
        *row += 1;

        let mut instance = instantiate_node(node_id, type_id, position, &|_| None).expect("catalogue node");
        if let Some(title) = node.get("name").and_then(Value::as_str) {
            instance.properties.insert("title".into(), json!(title));
        }
        let (field, prop) = match kind {
            "sequence" => (Some("sequence"), "sequence"),
            "blend_space" => (Some("blend_space"), "blend_space"),
            _ => (None, ""),
        };
        if let Some(field) = field {
            if let Some(reference) = node.get(field).and_then(Value::as_str) {
                if AssetRef::<AnimationGraphAssetKind>::parse(reference).is_none() {
                    return Err(CodecError::Invalid(format!("node `{node_id}` {field} is not a UUID")));
                }
                instance.properties.insert(prop.into(), json!(reference));
            }
        }
        kinds.insert(node_id.to_string(), type_id);
        main.add_node(instance).map_err(CodecError::Invalid)?;
    }

    let mut dropped = Vec::new();
    let mut output_fed = std::collections::HashSet::new();
    for edge in &old_edges {
        let (Some(from), Some(to)) = (edge.get("from").and_then(Value::as_str), edge.get("to").and_then(Value::as_str)) else {
            dropped.push(edge.clone());
            migration.notes.push("A connection without from/to was dropped".into());
            continue;
        };
        let reason = match (kinds.get(from), kinds.get(to)) {
            (None, _) | (_, None) => Some("it names a node that does not exist"),
            (Some(&"anim.output_pose"), _) => Some("the output node has no output"),
            (_, Some(&"anim.output_pose")) if !output_fed.insert(to.to_string()) => {
                Some("the output node takes one pose and an earlier connection already feeds it")
            }
            (_, Some(&"anim.output_pose")) => None,
            _ => Some("a player node takes no pose input"),
        };
        match reason {
            None => main
                .add_connection(data_connection(from, "pose", to, "pose"))
                .map_err(CodecError::Invalid)?,
            Some(reason) => {
                migration.notes.push(format!("Connection {from} -> {to} dropped: {reason}"));
                dropped.push(edge.clone());
            }
        }
    }

    let mut extra = Extra::new();
    // Keep what we did not understand, except the fields we consumed.
    for (key, value) in object {
        if !matches!(key.as_str(), "version" | "id" | "name" | "skeleton" | "nodes" | "connections") {
            extra.insert(key, value);
        }
    }
    if !dropped.is_empty() {
        extra.insert("migrated_dropped_connections".into(), Value::Array(dropped));
    }

    Ok((
        AnimationGraphAsset {
            version: ANIM_GRAPH_VERSION,
            id,
            name,
            skeleton,
            parameters: Vec::new(),
            main,
            subgraphs: Vec::new(),
            extra,
        },
        migration,
    ))
}

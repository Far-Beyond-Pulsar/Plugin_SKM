//! A small programmatic builder for animation graphs: used by tests, importers
//! and tools that create graphs without the editor. Every node is created from
//! the catalogue, so its pins always match its properties.

use std::collections::HashMap;

use graphy_blueprint::GraphDescription;
use serde_json::Value;

use super::catalogue::{ParamType, node_def};
use super::{data_connection, instantiate_node, refresh_node_pins};

pub struct GraphBuilder {
    pub graph: GraphDescription,
    params: HashMap<String, ParamType>,
}

impl GraphBuilder {
    pub fn new(name: &str, params: &[(&str, ParamType)]) -> Self {
        Self { graph: GraphDescription::new(name), params: params.iter().map(|(n, t)| ((*n).to_string(), *t)).collect() }
    }

    /// Add a node of catalogue type `ty` with the given property overrides.
    /// Panics on an unknown type: builders are for code that knows the catalogue.
    pub fn node(&mut self, id: &str, ty: &str, props: &[(&str, Value)]) -> &mut Self {
        let types = self.params.clone();
        let lookup = move |n: &str| types.get(n).copied();
        let mut node = instantiate_node(id, ty, (0.0, 0.0), &lookup).unwrap_or_else(|| panic!("unknown animation node type {ty}"));
        for (k, v) in props {
            node.properties.insert((*k).to_string(), v.clone());
        }
        refresh_node_pins(&mut node, &lookup);
        self.graph.add_node(node).expect("unique node id");
        self
    }

    /// Wire the first output of `from` to input `pin` of `to`.
    pub fn wire(&mut self, from: &str, to: &str, pin: &str) -> &mut Self {
        let out = self.graph.nodes[from].outputs.first().unwrap_or_else(|| panic!("{from} has no output")).id.clone();
        self.wire_pin(from, &out, to, pin)
    }

    pub fn wire_pin(&mut self, from: &str, from_pin: &str, to: &str, to_pin: &str) -> &mut Self {
        self.graph.connections.push(data_connection(from, from_pin, to, to_pin));
        self
    }

    pub fn into_graph(self) -> GraphDescription {
        self.graph
    }
}

#[allow(dead_code)]
fn _def(ty: &str) -> bool {
    node_def(ty).is_some()
}

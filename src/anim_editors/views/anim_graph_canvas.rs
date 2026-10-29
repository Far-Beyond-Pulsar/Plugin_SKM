//! The animation graph on the shared Graphy canvas: the profile (catalogue,
//! node factory, pin looks, exact-type connections, sub-graph navigation,
//! protected terminal nodes) and the boundary between the authored `.animgraph`
//! graph and the document the canvas edits.
//!
//! Drawing, layout, hit-testing and input are Graphy's; this file only says
//! what animation nodes and pins are.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use graphy::core::GraphComment;
use graphy::{Compatibility, Connection, ConnectionType, DataType, GraphDescription, NodeInstance, Pin, PinInstance, PinType, Position, StructuralResolver, TypeResolver};
use graphy_blueprint as authored;
use graphy_gpui::{CatalogEntry, GraphProfile, MacroTarget};
use graphy_render::geom::rgba;
use graphy_render::presenter::{default_pin_style, prettify, CommentPresentation, NodePresentation, PinStyle, Presenter};
use graphy_render::{PinShape, PinSide};
use serde_json::Value;
use skeletal_assets::anim_graph::catalogue::{node_def, AnimPinType, GraphKind, ParamType, PropKind, CATALOGUE};
use skeletal_assets::anim_graph::{instantiate_node, AnimationGraphAsset};

// ---- authored <-> canvas ---------------------------------------------------

fn datatype_in(dt: &authored::DataType) -> DataType {
    match dt {
        authored::DataType::Execution => DataType::Exec,
        authored::DataType::Data(info) => DataType::typed(info.to_string()),
    }
}

fn datatype_out(dt: &DataType) -> authored::DataType {
    match dt {
        DataType::Exec => authored::DataType::Execution,
        DataType::Data(info) => authored::DataType::from_type_str(&info.type_string),
    }
}

fn node_in(node: &authored::NodeInstance) -> NodeInstance {
    let mut out = NodeInstance::new(&node.id, &node.node_type, Position::new(f64::from(node.position.x), f64::from(node.position.y)));
    out.properties = node.properties.clone();
    let pin = |p: &authored::PinInstance, kind: PinType| PinInstance { id: p.id.clone(), pin: Pin { id: p.id.clone(), name: p.pin.name.clone(), data_type: datatype_in(&p.pin.data_type), pin_type: kind } };
    out.inputs = node.inputs.iter().map(|p| pin(p, PinType::Input)).collect();
    out.outputs = node.outputs.iter().map(|p| pin(p, PinType::Output)).collect();
    out
}

/// The canvas document for an authored graph.
pub fn to_canvas(graph: &authored::GraphDescription) -> GraphDescription {
    let mut doc = GraphDescription::new(&graph.metadata.name);
    doc.metadata.description = graph.metadata.description.clone();
    doc.metadata.version = graph.metadata.version.clone();
    doc.metadata.created_at = graph.metadata.created_at.clone();
    doc.metadata.modified_at = graph.metadata.modified_at.clone();
    for (key, node) in &graph.nodes {
        doc.nodes.insert(key.clone(), node_in(node));
    }
    doc.connections = graph
        .connections
        .iter()
        .map(|c| {
            let mut out = Connection::new(&c.source_node, &c.source_pin, &c.target_node, &c.target_pin, match c.connection_type {
                authored::ConnectionType::Execution => ConnectionType::Execution,
                authored::ConnectionType::Data => ConnectionType::Data,
            });
            out.id = Some(c.id.clone());
            out
        })
        .collect();
    doc.comments = graph
        .comments
        .iter()
        .map(|c| {
            let mut out = GraphComment::new(&c.text, Position::new(f64::from(c.position.0), f64::from(c.position.1)), (f64::from(c.size.0), f64::from(c.size.1)));
            out.id = c.id.clone();
            out.color = c.color;
            out.contained_node_ids = c.contained_node_ids.clone();
            out
        })
        .collect();
    doc
}

fn connection_id(c: &Connection) -> String {
    c.id.clone().unwrap_or_else(|| format!("{}.{}->{}.{}", c.source_node, c.source_pin, c.target_node, c.target_pin))
}

/// The authored graph for what the canvas holds. Ids, pin ids, connection ids
/// and comments survive; pin back-links are rebuilt from the connections.
pub fn to_authored(doc: &GraphDescription) -> authored::GraphDescription {
    let mut out = authored::GraphDescription::new(&doc.metadata.name);
    out.metadata.description = doc.metadata.description.clone();
    out.metadata.version = doc.metadata.version.clone();
    out.metadata.created_at = doc.metadata.created_at.clone();
    out.metadata.modified_at = doc.metadata.modified_at.clone();
    for (key, node) in &doc.nodes {
        let pin = |p: &PinInstance, kind: authored::PinType| authored::PinInstance { id: p.id.clone(), pin: authored::Pin { name: p.pin.name.clone(), pin_type: kind, data_type: datatype_out(&p.pin.data_type), connected_to: Vec::new() } };
        out.nodes.insert(
            key.clone(),
            authored::NodeInstance {
                id: node.id.clone(),
                node_type: node.node_type.clone(),
                position: authored::Position { x: node.position.x as f32, y: node.position.y as f32 },
                properties: node.properties.clone(),
                inputs: node.inputs.iter().map(|p| pin(p, authored::PinType::Input)).collect(),
                outputs: node.outputs.iter().map(|p| pin(p, authored::PinType::Output)).collect(),
            },
        );
    }
    for c in &doc.connections {
        let id = connection_id(c);
        if let Some(n) = out.nodes.get_mut(&c.source_node) {
            if let Some(p) = n.outputs.iter_mut().find(|p| p.id == c.source_pin) {
                p.pin.connected_to.push(id.clone());
            }
        }
        if let Some(n) = out.nodes.get_mut(&c.target_node) {
            if let Some(p) = n.inputs.iter_mut().find(|p| p.id == c.target_pin) {
                p.pin.connected_to.push(id.clone());
            }
        }
        out.connections.push(authored::Connection {
            id,
            source_node: c.source_node.clone(),
            source_pin: c.source_pin.clone(),
            target_node: c.target_node.clone(),
            target_pin: c.target_pin.clone(),
            connection_type: match c.connection_type {
                ConnectionType::Execution => authored::ConnectionType::Execution,
                ConnectionType::Data => authored::ConnectionType::Data,
            },
        });
    }
    out.comments = doc
        .comments
        .iter()
        .enumerate()
        .map(|(i, c)| authored::BlueprintComment {
            id: if c.id.is_empty() { format!("comment_{i}") } else { c.id.clone() },
            text: c.text.clone(),
            position: (c.position.x as f32, c.position.y as f32),
            size: (c.size.0 as f32, c.size.1 as f32),
            color: c.color,
            contained_node_ids: c.contained_node_ids.clone(),
        })
        .collect();
    out
}

// ---- the profile -----------------------------------------------------------

/// The parameter types the profile resolves dynamic pins with. The editor keeps
/// it current.
pub type ParamTypes = Arc<RwLock<HashMap<String, ParamType>>>;

pub fn pin_look(ty: AnimPinType) -> PinStyle {
    let (r, g, b) = match ty {
        AnimPinType::Pose => (0xa7, 0x8b, 0xfa),
        AnimPinType::AdditivePose => (0xf4, 0x72, 0xb6),
        AnimPinType::Float => (0x3b, 0x82, 0xf6),
        AnimPinType::Bool => (0x22, 0xc5, 0x5e),
        AnimPinType::Int => (0x06, 0xb6, 0xd4),
        AnimPinType::Vector => (0xea, 0xb3, 0x08),
        AnimPinType::Transform => (0xf9, 0x73, 0x16),
        AnimPinType::Name => (0xe8, 0x79, 0xf9),
        AnimPinType::StateFlow => (0xe4, 0xe4, 0xe7),
    };
    PinStyle {
        shape: match ty {
            AnimPinType::StateFlow => PinShape::Arrow,
            AnimPinType::Pose | AnimPinType::AdditivePose => PinShape::Hexagon,
            AnimPinType::Transform => PinShape::Diamond,
            _ => PinShape::Circle,
        },
        ringed: false,
        color: rgba(r, g, b, 1.0),
    }
}

fn category_accent(category: &str) -> graphy_render::Color {
    match category {
        "Sources" => rgba(0x8b, 0x5c, 0xf6, 1.0),
        "Blending" => rgba(0x3b, 0x82, 0xf6, 1.0),
        "IK" => rgba(0x22, 0xc5, 0x5e, 1.0),
        "State Machine" => rgba(0xf5, 0x9e, 0x0b, 1.0),
        "Rule" | "Logic" | "Math" => rgba(0x06, 0xb6, 0xd4, 1.0),
        "Structure" => rgba(0xef, 0x44, 0x44, 1.0),
        _ => rgba(0x8b, 0x8b, 0x8b, 1.0),
    }
}

fn category_glyph(category: &str) -> char {
    match category {
        "Sources" => '\u{25b6}',
        "Blending" => '\u{2295}',
        "IK" => '\u{2693}',
        "State Machine" => '\u{25a3}',
        "Rule" | "Logic" => '\u{2753}',
        "Math" => '\u{2211}',
        _ => '\u{25c6}',
    }
}

/// Exact type match, as the catalogue demands.
#[derive(Clone, Copy, Debug, Default)]
pub struct AnimResolver;

impl TypeResolver for AnimResolver {
    fn compatibility(&self, from: &DataType, to: &DataType) -> Compatibility {
        let (DataType::Data(a), DataType::Data(b)) = (from, to) else { return StructuralResolver.compatibility(from, to) };
        match (AnimPinType::from_type_name(&a.type_string), AnimPinType::from_type_name(&b.type_string)) {
            (Some(x), Some(y)) if x.connects_to(y) => Compatibility::Exact,
            (Some(x), Some(y)) => Compatibility::Incompatible(format!("{} cannot connect to {}", x.type_name(), y.type_name())),
            _ => Compatibility::Incompatible("unknown pin type".into()),
        }
    }
}

pub struct AnimPresenter;

impl Presenter for AnimPresenter {
    fn node(&self, node: &NodeInstance) -> NodePresentation {
        if node.is_reroute() {
            let mut p = NodePresentation::card("", rgba(0x8b, 0x8b, 0x8b, 1.0));
            p.shape = graphy_render::NodeShape::Reroute;
            return p;
        }
        let Some(def) = node_def(&node.node_type) else { return NodePresentation::card(prettify(&node.node_type), rgba(0x4a, 0x55, 0x68, 1.0)) };
        let mut title = def.title.to_string();
        let mut subtitle = None;
        match node.node_type.as_str() {
            "sm.state" => {
                if let Some(name) = node.properties.get("name").and_then(Value::as_str) {
                    title = name.to_string();
                }
                subtitle = Some("State \u{b7} double-click to open".to_string());
            }
            "sm.transition" => subtitle = Some(format!("{} s \u{b7} priority {}", node.properties.get("duration").and_then(Value::as_f64).unwrap_or(0.0), node.properties.get("priority").and_then(Value::as_i64).unwrap_or(0))),
            "anim.state_machine" | "anim.layer" => subtitle = Some("double-click to open".to_string()),
            _ => {}
        }
        let mut p = NodePresentation::card(title, category_accent(def.category));
        p.icon = Some(category_glyph(def.category));
        p.subtitle = subtitle;
        p
    }

    fn pin(&self, _node: &NodeInstance, pin: &PinInstance, _side: PinSide) -> PinStyle {
        match pin.data_type() {
            DataType::Data(info) => AnimPinType::from_type_name(&info.type_string).map(pin_look).unwrap_or_else(|| default_pin_style(pin.data_type())),
            other => default_pin_style(other),
        }
    }

    fn pin_label(&self, _node: &NodeInstance, pin: &PinInstance, _side: PinSide) -> String {
        prettify(&pin.pin.name)
    }

    fn comment(&self, _index: usize, comment: &GraphComment) -> CommentPresentation {
        CommentPresentation { accent: comment.color }
    }
}

/// The animation graph profile of one open document.
pub struct AnimProfile {
    presenter: AnimPresenter,
    resolver: AnimResolver,
    params: ParamTypes,
    kind: Arc<RwLock<GraphKind>>,
}

impl AnimProfile {
    pub fn new(params: ParamTypes, kind: Arc<RwLock<GraphKind>>) -> Self {
        Self { presenter: AnimPresenter, resolver: AnimResolver, params, kind }
    }

    fn graph_kind(&self) -> GraphKind {
        *self.kind.read().unwrap_or_else(|e| e.into_inner())
    }
}

impl GraphProfile for AnimProfile {
    fn id(&self) -> &str {
        "animation-graph"
    }

    fn presenter(&self) -> &dyn Presenter {
        &self.presenter
    }

    fn type_resolver(&self) -> &dyn TypeResolver {
        &self.resolver
    }

    fn catalog(&self) -> Vec<CatalogEntry> {
        let kind = self.graph_kind();
        CATALOGUE
            .iter()
            .filter(|d| d.graphs.contains(&kind) && d.type_id != kind.terminal())
            .map(|d| CatalogEntry {
                definition: d.type_id.to_string(),
                title: d.title.to_string(),
                category: d.category.to_string(),
                keywords: vec![d.type_id.to_string()],
                description: d.doc.to_string(),
                icon: category_glyph(d.category).to_string(),
                color: Some(category_accent(d.category)),
                inputs: Some(d.inputs.iter().map(|p| p.ty.data_type_core()).collect()),
            })
            .collect()
    }

    fn create_node(&self, definition: &str, id: &str, pos: Position) -> Option<NodeInstance> {
        let params = self.params.read().unwrap_or_else(|e| e.into_inner()).clone();
        instantiate_node(id, definition, (pos.x as f32, pos.y as f32), &|p| params.get(p).copied()).map(|n| node_in(&n))
    }

    fn is_deletable(&self, node: &NodeInstance) -> bool {
        node.node_type != self.graph_kind().terminal()
    }

    fn macro_target(&self, node: &NodeInstance) -> Option<MacroTarget> {
        let def = node_def(&node.node_type)?;
        def.props.iter().find(|p| matches!(p.kind, PropKind::SubGraph(_))).and_then(|p| node.properties.get(p.name)?.as_str()).filter(|id| !id.is_empty()).map(|id| MacroTarget { graph_id: id.to_string() })
    }
}

impl graphy_gpui::canvas::GpuiProfile for AnimProfile {}

trait CoreType {
    fn data_type_core(self) -> DataType;
}
impl CoreType for AnimPinType {
    fn data_type_core(self) -> DataType {
        DataType::typed(self.type_name())
    }
}

/// Used by tests: the pose graph of a fresh asset as a canvas document.
pub fn main_canvas(asset: &AnimationGraphAsset) -> GraphDescription {
    to_canvas(&asset.main)
}

#[cfg(test)]
mod tests {
    use super::*;
    use skeletal_assets::anim_graph::{data_connection, AnimationGraphAsset};

    fn asset_with_wire() -> AnimationGraphAsset {
        let mut a = AnimationGraphAsset::default();
        let player = instantiate_node("player", "anim.sequence_player", (-300.0, 20.5), &|_| None).unwrap();
        a.main.add_node(player).unwrap();
        a.main.add_connection(data_connection("player", "pose", "output", "pose")).unwrap();
        a
    }

    #[test]
    fn the_canvas_document_round_trips_the_authored_graph() {
        let a = asset_with_wire();
        let canvas = to_canvas(&a.main);
        assert_eq!(canvas.nodes.len(), 2);
        assert_eq!(canvas.connections.len(), 1);
        let back = to_authored(&canvas);
        assert_eq!(back.nodes.len(), 2);
        assert_eq!(back.connections[0].id, a.main.connections[0].id, "connection ids survive");
        let pin = |g: &authored::GraphDescription, n: &str, p: &str, out: bool| {
            let node = &g.nodes[n];
            let pins = if out { &node.outputs } else { &node.inputs };
            pins.iter().find(|x| x.id == p).unwrap().pin.connected_to.clone()
        };
        assert_eq!(pin(&back, "player", "pose", true), pin(&a.main, "player", "pose", true), "back-links are rebuilt");
        assert_eq!(back.nodes["player"].position.y, 20.5);
        assert_eq!(back.nodes["player"].properties, a.main.nodes["player"].properties);
        // the result is a valid asset graph
        let mut b = a.clone();
        b.main = back;
        b.to_text().expect("still serialises");
    }

    #[test]
    fn the_resolver_demands_exact_pin_types() {
        let r = AnimResolver;
        let t = |ty: AnimPinType| DataType::typed(ty.type_name());
        assert!(matches!(r.compatibility(&t(AnimPinType::Pose), &t(AnimPinType::Pose)), Compatibility::Exact));
        assert!(matches!(r.compatibility(&t(AnimPinType::Float), &t(AnimPinType::Pose)), Compatibility::Incompatible(_)));
        assert!(matches!(r.compatibility(&t(AnimPinType::Int), &t(AnimPinType::Float)), Compatibility::Incompatible(_)), "no implicit widening");
    }

    #[test]
    fn the_catalog_lists_only_nodes_valid_in_the_open_graph_and_never_the_terminal() {
        let kind = Arc::new(RwLock::new(GraphKind::Main));
        let p = AnimProfile::new(Default::default(), kind.clone());
        let names: Vec<_> = p.catalog().iter().map(|e| e.definition.clone()).collect();
        assert!(names.contains(&"anim.sequence_player".to_string()));
        assert!(!names.contains(&"anim.output_pose".to_string()), "the terminal is not offered");
        assert!(!names.contains(&"sm.state".to_string()));
        *kind.write().unwrap() = GraphKind::StateMachine;
        let names: Vec<_> = p.catalog().iter().map(|e| e.definition.clone()).collect();
        assert!(names.contains(&"sm.state".to_string()) && names.contains(&"sm.transition".to_string()));
        assert!(!names.contains(&"sm.entry".to_string()));
    }

    #[test]
    fn the_factory_builds_catalogue_nodes_with_their_pins_and_protects_terminals() {
        let p = AnimProfile::new(Default::default(), Arc::new(RwLock::new(GraphKind::Main)));
        let n = p.create_node("anim.blend_multi", "b", Position::new(1.0, 2.0)).unwrap();
        assert!(n.inputs.iter().any(|i| i.id.starts_with("pose_")), "dynamic pose pins exist");
        assert!(p.create_node("anim.nope", "x", Position::new(0.0, 0.0)).is_none());
        let out = p.create_node("anim.output_pose", "o", Position::new(0.0, 0.0)).unwrap();
        assert!(!p.is_deletable(&out));
        assert!(p.is_deletable(&n));
        let state = {
            let mut s = p.create_node("anim.state_machine", "m", Position::new(0.0, 0.0)).unwrap();
            assert!(p.macro_target(&s).is_none(), "no graph yet");
            s.properties.insert("machine".into(), serde_json::json!("machine_1"));
            s
        };
        assert_eq!(p.macro_target(&state).map(|t| t.graph_id), Some("machine_1".into()));
    }
}

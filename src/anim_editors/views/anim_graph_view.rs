//! The Animation Graph editor: the Graphy canvas with the animation profile,
//! graph navigation (main graph, state machines, state pose graphs, transition
//! rules, layers), parameters, a typed inspector for the selected node, and a
//! live preview of the graph running on the real runtime below the canvas.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use gpui::prelude::FluentBuilder;
use gpui::*;
use graphy_gpui::canvas::{system_fonts, CanvasEvent, GraphCanvas};
use graphy_gpui::Action;
use graphy::GraphCommand;
use plugin_editor_api::{OpenAsset, PluginError};
use pulsar_reflection::{AnimationGraphAssetKind, AssetKind, AssetRef, SkeletonAssetKind};
use serde_json::{json, Value};
use skeletal_assets::anim_graph::catalogue::{node_def, GraphKind, ParamType, PropKind};
use skeletal_assets::anim_graph::AnimationGraphAsset;
use ui::button::{Button, ButtonVariants};
use ui::{h_flex, v_flex, ActiveTheme, Icon, IconName, Selectable, Sizable, StyledExt};

use crate::anim_editors::anim_graph_model::{AnimGraphModel, GraphRef, MachineRow};
use crate::anim_editors::graph_preview::{GraphPreview, ProjectAssets};
use crate::anim_editors::refs::{self, ProjectRefIndex, RefStatus, SnapshotIndex};
use crate::anim_editors::selection::AnimSelection;
use crate::anim_editors::skeleton_proj::SkeletonProjection;
use crate::anim_editors::store::FsStore;
use crate::core::asset_index;

use super::anim_graph_canvas::{to_authored, to_canvas, AnimProfile, ParamTypes};
use super::chrome::{edit, empty_inspector, fmt_f32, frame, labelled, parse_f32, section, value_row, Chrome, EditorBody, EditorHost, EditorModel, Widgets};
use super::skeleton_canvas;

mod bone_mask_panel;

fn graph_theme(cx: &App) -> graphy_render::Theme {
    let t = cx.theme();
    let opaque = |c: Hsla| {
        let c = c.to_rgb();
        [c.r, c.g, c.b, 1.0]
    };
    graphy_render::Theme::from_colors(&graphy_render::ThemeColors {
        background: opaque(t.background),
        canvas: opaque(t.background),
        foreground: opaque(t.foreground),
        muted_foreground: opaque(t.muted_foreground),
        border: opaque(t.border),
        accent: opaque(t.primary),
    })
}

pub struct AnimGraphEditor {
    model: AnimGraphModel,
    chrome: Chrome,
    widgets: Widgets,
    canvas: Entity<GraphCanvas>,
    params: ParamTypes,
    kind: Arc<RwLock<GraphKind>>,
    /// The model generation the canvas shows.
    synced: u64,
    statuses: HashMap<String, RefStatus>,
    preview: Option<GraphPreview>,
    preview_error: Option<String>,
    preview_stale: bool,
    playing: bool,
    preview_bounds: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    _subscription: Option<Subscription>,
}

impl AnimGraphEditor {
    pub fn open(path: PathBuf, window: &mut Window, cx: &mut App) -> Result<Entity<Self>, PluginError> {
        let text = AnimationGraphAsset::default().to_text().unwrap_or_default();
        let model = AnimGraphModel::open(Arc::new(FsStore), &path, &text).map_err(|message| PluginError::Other { message })?;
        let chrome = Chrome::new(&path, cx);
        asset_index::record_asset(&chrome.root, &path);
        let params: ParamTypes = Arc::new(RwLock::new(model.param_types()));
        let kind = Arc::new(RwLock::new(GraphKind::Main));
        let profile = Arc::new(AnimProfile::new(params.clone(), kind.clone()));
        let doc = to_canvas(model.graph());
        let canvas = cx.new(|cx| GraphCanvas::new(doc, profile, system_fonts(), graph_theme(cx), cx).expect("the bundled canvas font parses"));
        let synced = model.doc.generation();
        let entity = cx.new(|cx| {
            let sub = cx.subscribe_in(&canvas, window, |this: &mut Self, _canvas, event: &CanvasEvent, window, cx| this.on_canvas_event(event, window, cx));
            let mut this = Self {
                model,
                chrome,
                widgets: Widgets::default(),
                canvas: canvas.clone(),
                params,
                kind,
                synced,
                statuses: HashMap::new(),
                preview: None,
                preview_error: None,
                preview_stale: true,
                playing: false,
                preview_bounds: Default::default(),
                _subscription: Some(sub),
            };
            this.refresh_refs();
            this
        });
        Ok(entity)
    }

    fn refresh_refs(&mut self) {
        let index = SnapshotIndex::new(&self.chrome.root);
        let skeleton = self.model.asset().skeleton.as_ref().map(|s| s.as_str().to_string());
        self.statuses.insert("skeleton".into(), refs::ref_status(&index, SkeletonAssetKind::KIND, skeleton.as_deref(), None));
        let graph = self.model.graph().clone();
        for node in graph.nodes.values() {
            let Some(def) = node_def(&node.node_type) else { continue };
            for p in def.props {
                if let PropKind::Asset(kind) = p.kind {
                    let id = node.properties.get(p.name).and_then(Value::as_str).filter(|s| !s.is_empty());
                    self.statuses.insert(format!("{}:{}", node.id, p.name), refs::ref_status(&index, kind, id, skeleton.as_deref()));
                }
            }
        }
    }

    /// Show the model's open graph on the canvas.
    fn push_to_canvas(&mut self, frame_all: bool, cx: &mut Context<Self>) {
        *self.kind.write().unwrap_or_else(|e| e.into_inner()) = self.model.graph_kind();
        *self.params.write().unwrap_or_else(|e| e.into_inner()) = self.model.param_types();
        let doc = to_canvas(self.model.graph());
        self.canvas.update(cx, |c, cx| {
            c.set_graph(doc, cx);
            if frame_all {
                c.frame_all(cx);
            }
        });
        self.synced = self.model.doc.generation();
    }

    fn on_canvas_event(&mut self, event: &CanvasEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            CanvasEvent::Edited { label, .. } => {
                let authored = to_authored(self.canvas.read(cx).graph());
                self.model.replace_graph(authored, label, None);
                self.model.end_gesture();
                self.synced = self.model.doc.generation();
                self.refresh_refs();
                self.preview_stale = true;
                self.chrome.aux_generation = None;
                cx.notify();
            }
            CanvasEvent::Rejected(why) => {
                self.chrome.status = Some(why.clone());
                cx.notify();
            }
            CanvasEvent::SelectionChanged => {
                let first = self.canvas.read(cx).controller().selection.nodes.iter().next().cloned();
                match first {
                    Some(id) => {
                        self.model.select_node(&id);
                    }
                    None => {
                        if matches!(self.model.selection, AnimSelection::GraphNode { .. }) {
                            self.model.selection = AnimSelection::None;
                        }
                    }
                }
                cx.notify();
            }
            CanvasEvent::ViewChanged => {}
            CanvasEvent::Request(Action::OpenGraph(target)) => {
                let (id, entity, handle) = (target.graph_id.clone(), cx.weak_entity(), window.window_handle());
                // the double-click lands while the canvas is updating
                cx.defer(move |cx| {
                    let _ = cx.update_window(handle, |_, _, cx| {
                        let _ = entity.update(cx, |this, cx| this.open_graph(GraphRef::Sub(id), cx));
                    });
                });
            }
            CanvasEvent::Request(Action::NodeActivated(id)) => {
                let (id, entity, handle) = (id.clone(), cx.weak_entity(), window.window_handle());
                cx.defer(move |cx| {
                    let _ = cx.update_window(handle, |_, _, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            match this.model.ensure_subgraph(&id) {
                                Ok(target) => this.open_graph(target, cx),
                                Err(_) => {
                                    this.model.select_node(&id);
                                    cx.notify();
                                }
                            }
                        });
                    });
                });
            }
            CanvasEvent::Request(Action::ConnectionRefused(why)) => {
                self.chrome.status = Some(why.clone());
                cx.notify();
            }
            CanvasEvent::Request(_) => {}
            CanvasEvent::NodePicked { definition, graph_pos, source } => {
                let (definition, pos, source) = (definition.clone(), (graph_pos.x, graph_pos.y), source.clone());
                self.place_picked(&definition, pos, source, window, cx);
            }
        }
    }

    fn place_picked(&mut self, definition: &str, pos: (f32, f32), source: Option<graphy::PinRef>, window: &mut Window, cx: &mut Context<Self>) {
        // sync first: the canvas may hold edits not yet in the model
        let authored = to_authored(self.canvas.read(cx).graph());
        self.model.replace_graph(authored, "Edit graph", None);
        self.model.end_gesture();
        let result = self.model.add_node(definition, pos).and_then(|id| {
            if self.model.target_of(&id).is_none() && node_def(definition).is_some_and(|d| d.props.iter().any(|p| matches!(p.kind, PropKind::SubGraph(_)))) {
                self.model.ensure_subgraph(&id)?;
            }
            if let Some(src) = source {
                let wire = self.model.graph().nodes.get(&id).and_then(|n| {
                    let from = self.model.graph().nodes.get(&src.node)?.outputs.iter().find(|p| p.id == src.pin)?.pin.data_type.clone();
                    n.inputs.iter().find(|p| p.pin.data_type == from).map(|p| p.id.clone())
                });
                if let Some(pin) = wire {
                    let _ = self.model.connect((&src.node, &src.pin), (&id, &pin));
                }
            }
            Ok(())
        });
        if let Err(message) = result {
            self.chrome.status = Some(message);
        }
        self.push_to_canvas(false, cx);
        self.refresh_refs();
        self.preview_stale = true;
        self.chrome.aux_generation = None;
        let _ = window;
        cx.notify();
    }

    fn open_graph(&mut self, target: GraphRef, cx: &mut Context<Self>) {
        // keep what the canvas holds before leaving the graph
        let authored = to_authored(self.canvas.read(cx).graph());
        self.model.replace_graph(authored, "Edit graph", None);
        self.model.end_gesture();
        if self.model.open_graph(target) {
            self.push_to_canvas(true, cx);
        }
        cx.notify();
    }

    fn rebuild_preview(&mut self) {
        let assets = ProjectAssets::new(&self.chrome.root);
        match GraphPreview::build(self.model.asset(), &assets) {
            Ok(p) => {
                self.preview = Some(p);
                self.preview_error = None;
            }
            Err(d) => {
                self.preview = None;
                self.preview_error = Some(d.iter().map(|d| d.message.clone()).take(3).collect::<Vec<_>>().join(" \u{b7} "));
            }
        }
        self.preview_stale = false;
    }

    fn start_play(&mut self, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        cx.spawn(async move |_, cx| loop {
            smol::Timer::after(std::time::Duration::from_millis(33)).await;
            let go = weak.update(cx, |this, cx| {
                if !this.playing {
                    return false;
                }
                if let Some(p) = &mut this.preview {
                    p.advance(0.033, false);
                }
                cx.notify();
                true
            });
            if !matches!(go, Ok(true)) {
                break;
            }
        })
        .detach();
    }

    fn preview_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if self.preview_stale && self.preview.is_none() && self.preview_error.is_none() {
            self.rebuild_preview();
        }
        let header = h_flex()
            .w_full()
            .px_3()
            .h(px(26.0))
            .flex_shrink_0()
            .gap_2()
            .items_center()
            .bg(theme.secondary)
            .border_t_1()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .child(div().font_semibold().text_color(theme.muted_foreground).child("PREVIEW \u{b7} runs the compiled graph on a transient instance"))
            .child(div().flex_1())
            .child(Button::new("ag-pv-build").icon(IconName::Refresh).label(if self.preview_stale { "Rebuild (graph changed)" } else { "Rebuild" }).xsmall().ghost().on_click(cx.listener(|this, _, _, cx| {
                this.rebuild_preview();
                cx.notify();
            })))
            .child(Button::new("ag-pv-play").icon(if self.playing { IconName::Pause } else { IconName::Play }).xsmall().ghost().on_click(cx.listener(|this, _, _, cx| {
                this.playing = !this.playing;
                if this.playing {
                    if this.preview.is_none() {
                        this.rebuild_preview();
                    }
                    this.start_play(cx);
                }
                cx.notify();
            })))
            .child(Button::new("ag-pv-step").icon(IconName::ChevronRight).xsmall().ghost().tooltip("Step one frame").on_click(cx.listener(|this, _, _, cx| {
                if let Some(p) = &mut this.preview {
                    p.advance(1.0 / 30.0, false);
                }
                cx.notify();
            })));
        let body: AnyElement = match (&self.preview, &self.preview_error) {
            (None, Some(error)) => div().p_3().text_xs().text_color(theme.warning).child(format!("The graph does not compile yet: {error}")).into_any_element(),
            (None, None) => div().p_3().text_xs().text_color(theme.muted_foreground).child("Building\u{2026}").into_any_element(),
            (Some(preview), _) => {
                let projection: SkeletonProjection = preview.projection();
                let report = preview.report();
                let params = preview.parameters();
                let pane = div().w(px(200.0)).h_full().flex_shrink_0().border_r_1().border_color(theme.border).child(skeleton_canvas::element(projection, self.preview_bounds.clone(), theme.foreground, theme.background, Vec::new(), Vec::new(), None, theme.primary));
                let mut param_col = v_flex().w(px(210.0)).flex_shrink_0().h_full().overflow_y_scroll().p_2().gap_1().border_r_1().border_color(theme.border);
                param_col = param_col.child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child("PARAMETERS (preview only)"));
                if params.is_empty() {
                    param_col = param_col.child(div().text_xs().text_color(theme.muted_foreground).child("The graph has no parameters."));
                }
                for (name, ty, value) in params {
                    let text = match &value {
                        Value::Array(a) => a.iter().map(|v| fmt_f32(v.as_f64().unwrap_or(0.0) as f32)).collect::<Vec<_>>().join(", "),
                        Value::Bool(b) => b.to_string(),
                        other => other.as_f64().map(|f| fmt_f32(f as f32)).unwrap_or_default(),
                    };
                    param_col = param_col.child(self.widgets.text_field(&format!("pv.{name}"), &format!("{name} ({ty:?})"), &text, window, cx));
                }
                let mut info = v_flex().flex_1().min_w_0().h_full().overflow_y_scroll().p_2().gap_0p5();
                info = info.child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child(format!("STATE / TRACE \u{b7} t = {} s", fmt_f32(report.time))));
                if report.trace.is_empty() {
                    info = info.child(div().text_xs().text_color(theme.muted_foreground).child("No state machine or IK activity."));
                }
                for line in report.trace.iter().take(24) {
                    info = info.child(div().text_xs().text_color(theme.foreground).child(line.clone()));
                }
                info = info.child(div().pt_1().text_xs().font_semibold().text_color(theme.muted_foreground).child("CURVES"));
                for (name, v) in report.curves.iter().take(12) {
                    info = info.child(div().text_xs().text_color(theme.foreground).child(format!("{name}: {}", fmt_f32(*v))));
                }
                info = info.child(div().pt_1().text_xs().font_semibold().text_color(theme.muted_foreground).child("ROOT MOTION (last advance)"));
                info = info.child(div().text_xs().text_color(theme.foreground).child(format!("{}, {}, {}", fmt_f32(report.root_motion_translation[0]), fmt_f32(report.root_motion_translation[1]), fmt_f32(report.root_motion_translation[2]))));
                h_flex().size_full().child(pane).child(param_col).child(info).into_any_element()
            }
        };
        v_flex().w_full().h(px(210.0)).flex_shrink_0().child(header).child(div().flex_1().min_h_0().child(body)).into_any_element()
    }

    fn node_inspector(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(node) = self.model.graph().nodes.get(id).cloned() else { return empty_inspector(&self.model.selection, "The node no longer exists.", cx) };
        let Some(def) = node_def(&node.node_type) else { return empty_inspector(&self.model.selection, "This node type is not in the catalogue.", cx) };
        let theme = cx.theme().clone();
        let root = self.chrome.root.clone();
        let skeleton = self.model.asset().skeleton.as_ref().map(|s| s.as_str().to_string());
        let mut body = v_flex().w_full().gap_2();
        body = body.child(div().text_sm().font_semibold().text_color(theme.foreground).child(def.title));
        body = body.child(div().text_xs().text_color(theme.muted_foreground).child(def.doc));
        body = body.child(value_row("Type", def.type_id, cx));
        let mut rows: Vec<AnyElement> = Vec::new();
        for p in def.props {
            let current = node.properties.get(p.name).cloned().unwrap_or(Value::Null);
            let label = format!("{}{}", p.name, if p.required { " *" } else { "" });
            let nid = id.to_string();
            let pname = p.name.to_string();
            match p.kind {
                PropKind::Bool => {
                    let on = current.as_bool().unwrap_or(false);
                    rows.push(labelled(&label, Button::new(SharedString::from(format!("ag-b-{}", p.name))).label(if on { "On" } else { "Off" }).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                        let (n, pn) = (nid.clone(), pname.clone());
                        edit(this, window, cx, |m| m.set_property(&n, &pn, json!(!on)));
                        this.push_to_canvas(false, cx);
                    })), cx));
                }
                PropKind::Enum(options) => {
                    let cur = current.as_str().unwrap_or(options[0]).to_string();
                    rows.push(labelled(&label, Button::new(SharedString::from(format!("ag-e-{}", p.name))).label(cur.clone()).small().outline().tooltip(options.join(" / ")).on_click(cx.listener(move |this, _, window, cx| {
                        let next = options.iter().position(|o| *o == cur).map(|i| options[(i + 1) % options.len()]).unwrap_or(options[0]);
                        let (n, pn) = (nid.clone(), pname.clone());
                        edit(this, window, cx, |m| m.set_property(&n, &pn, json!(next)));
                        this.push_to_canvas(false, cx);
                    })), cx));
                }
                PropKind::Parameter(ty) => {
                    let names: Vec<String> = self.model.asset().parameters.iter().filter(|q| q.ty.pin_type() == ty).map(|q| q.name.clone()).collect();
                    let cur = current.as_str().unwrap_or("").to_string();
                    rows.push(labelled(&label, Button::new(SharedString::from(format!("ag-p-{}", p.name))).label(if cur.is_empty() { "None".to_string() } else { cur.clone() }).small().outline().tooltip(if names.is_empty() { format!("Add a {} parameter first", ty.type_name()) } else { "Cycle through the parameters".into() }).on_click(cx.listener(move |this, _, window, cx| {
                        let next = match names.iter().position(|n| *n == cur) {
                            Some(i) if i + 1 < names.len() => names[i + 1].clone(),
                            Some(_) => String::new(),
                            None => names.first().cloned().unwrap_or_default(),
                        };
                        let (n, pn) = (nid.clone(), pname.clone());
                        edit(this, window, cx, |m| m.set_property(&n, &pn, json!(next)));
                        this.push_to_canvas(false, cx);
                    })), cx));
                }
                PropKind::Asset(kind) => {
                    let status = self.statuses.get(&format!("{}:{}", node.id, p.name)).cloned().unwrap_or(RefStatus::Unset);
                    let cur = current.as_str().filter(|s| !s.is_empty()).map(str::to_string);
                    rows.push(self.widgets.ref_field(&format!("prop:{}", p.name), &label, kind, skeleton.clone(), cur.clone(), &status, &root, window, cx));
                    if let Some(path) = cur.as_deref().and_then(|c| ProjectRefIndex::new(&root).locate_path(kind, c)) {
                        rows.push(Button::new(SharedString::from(format!("ag-open-{}", p.name))).icon(IconName::ExternalLink).label("Open source asset").small().ghost().on_click(move |_, window, cx| window.dispatch_action(Box::new(OpenAsset { path: path.clone() }), cx)).into_any_element());
                    }
                }
                PropKind::SubGraph(_) => {
                    let nid = id.to_string();
                    let has = self.model.target_of(id).is_some();
                    rows.push(Button::new(SharedString::from(format!("ag-sub-{}", p.name))).icon(IconName::ChevronRight).label(if has { format!("Open {}", p.name) } else { format!("Create and open {}", p.name) }).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                        let nid = nid.clone();
                        let target = this.model.ensure_subgraph(&nid);
                        match target {
                            Ok(t) => this.open_graph(t, cx),
                            Err(e) => this.chrome.status = Some(e),
                        }
                        let _ = window;
                    })).into_any_element());
                }
                PropKind::BoneMask => rows.extend(bone_mask_panel::rows(self, &node, &label, window, cx)),
                _ => {
                    let text = match &current {
                        Value::Null => String::new(),
                        Value::String(s) => s.clone(),
                        Value::Array(a) => a.iter().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| fmt_f32(v.as_f64().unwrap_or(0.0) as f32))).collect::<Vec<_>>().join(", "),
                        other => other.as_f64().map(|f| fmt_f32(f as f32)).unwrap_or_else(|| other.to_string()),
                    };
                    rows.push(self.widgets.text_field(&format!("prop:{}", p.name), &label, &text, window, cx));
                }
            }
        }
        if rows.is_empty() {
            rows.push(div().text_xs().text_color(theme.muted_foreground).child("No properties.").into_any_element());
        }
        body = body.child(section("Properties", rows, cx));
        let pins: Vec<AnyElement> = node.inputs.iter().map(|p| value_row(&format!("in {}", p.pin.name), p.pin.data_type.to_string(), cx)).chain(node.outputs.iter().map(|p| value_row(&format!("out {}", p.pin.name), p.pin.data_type.to_string(), cx))).collect();
        body = body.child(section("Pins", pins, cx));
        if node.node_type == "sm.state" {
            let nid = id.to_string();
            body = body.child(Button::new("ag-default").label("Make default state").small().outline().on_click(cx.listener(move |this, _, window, cx| {
                let nid = nid.clone();
                edit(this, window, cx, |m| m.set_default_state(&nid));
                this.push_to_canvas(false, cx);
            })));
        }
        if def.type_id != self.model.graph_kind().terminal() {
            let nid = id.to_string();
            body = body.child(Button::new("ag-del").icon(IconName::Trash).label("Delete Node").small().danger().on_click(cx.listener(move |this, _, window, cx| {
                let nid = nid.clone();
                edit(this, window, cx, |m| m.delete_node(&nid));
                this.push_to_canvas(false, cx);
            })));
        }
        body.into_any_element()
    }

    fn parameter_inspector(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(p) = self.model.asset().parameter(name).cloned() else { return empty_inspector(&self.model.selection, "The parameter no longer exists.", cx) };
        let mut body = v_flex().w_full().gap_2().child(div().text_sm().font_semibold().text_color(cx.theme().foreground).child("Parameter"));
        body = body.child(self.widgets.text_field("param.name", "Name", &p.name, window, cx));
        body = body.child(value_row("Type", p.ty.pin_type().type_name(), cx));
        let default_text = match &p.default {
            Value::Array(a) => a.iter().map(|v| fmt_f32(v.as_f64().unwrap_or(0.0) as f32)).collect::<Vec<_>>().join(", "),
            Value::Bool(b) => b.to_string(),
            other => other.as_f64().map(|f| fmt_f32(f as f32)).unwrap_or_default(),
        };
        body = body.child(self.widgets.text_field("param.default", "Default", &default_text, window, cx));
        let uses = self.model.parameter_uses(&p.name);
        body = body.child(value_row("Used by", format!("{} node(s)", uses.len()), cx));
        let n = p.name.clone();
        body = body.child(Button::new("ag-param-del").icon(IconName::Trash).label("Delete Parameter").small().danger().on_click(cx.listener(move |this, _, window, cx| {
            let n = n.clone();
            edit(this, window, cx, |m| m.delete_parameter(&n));
        })));
        body.into_any_element()
    }
}

impl EditorHost for AnimGraphEditor {
    fn widgets(&mut self) -> &mut Widgets {
        &mut self.widgets
    }

    fn commit_field(&mut self, key: &str, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = key.strip_prefix("zip_tie.straight_length.").and_then(|s| s.parse::<usize>().ok()) {
            if let Some(length) = parse_f32(&text).filter(|v| v.is_finite() && *v >= 0.0) {
                let command = GraphCommand::SetZipTieStraightLength { index, length: length as f64 };
                let _ = self.canvas.update(cx, |canvas, cx| canvas.apply("Set zip-tie straight length", &[command], cx));
            } else {
                self.chrome.status = Some("Enter a non-negative length.".into());
            }
            return;
        }
        if let Some(name) = key.strip_prefix("pv.") {
            let value = self.preview.as_ref().and_then(|p| p.parameters().into_iter().find(|q| q.0 == name)).map(|q| q.1);
            let parsed = match value {
                Some(ParamType::Float) => parse_f32(&text).map(|v| json!(v)),
                Some(ParamType::Int) => text.trim().parse::<i64>().ok().map(|v| json!(v)),
                Some(ParamType::Bool) => text.trim().parse::<bool>().ok().map(|v| json!(v)),
                Some(ParamType::Vector) => {
                    let n: Vec<f32> = text.split(',').filter_map(|t| t.trim().parse().ok()).collect();
                    (n.len() == 3).then(|| json!(n))
                }
                None => None,
            };
            match (parsed, &mut self.preview) {
                (Some(v), Some(p)) => {
                    if let Err(e) = p.set_parameter(name, v) {
                        self.chrome.status = Some(e);
                    }
                }
                _ => self.chrome.status = Some(format!("Could not set {name}: enter a value of its type.")),
            }
            cx.notify();
            return;
        }
        if let Some(rest) = key.strip_prefix("skmask.") {
            bone_mask_panel::commit_library(self, rest, text, window, cx);
            return;
        }
        if let Some(rest) = key.strip_prefix("mask.") {
            bone_mask_panel::commit(self, rest, text, window, cx);
            return;
        }
        let sel = self.model.selection.clone();
        edit(self, window, cx, |m| match (key, &sel) {
            ("name", _) => {
                m.rename(&text);
                Ok(())
            }
            ("param.name", AnimSelection::GraphParameter { name }) => m.rename_parameter(name, &text),
            ("param.default", AnimSelection::GraphParameter { name }) => {
                let ty = m.asset().parameter(name).map(|p| p.ty).ok_or("no such parameter")?;
                let value = match ty {
                    ParamType::Float => parse_f32(&text).map(|v| json!(v)).ok_or("enter a number")?,
                    ParamType::Int => text.trim().parse::<i64>().map(|v| json!(v)).map_err(|_| "enter a whole number".to_string())?,
                    ParamType::Bool => text.trim().parse::<bool>().map(|v| json!(v)).map_err(|_| "enter true or false".to_string())?,
                    ParamType::Vector => {
                        let n: Vec<f32> = text.split(',').filter_map(|t| t.trim().parse().ok()).collect();
                        if n.len() != 3 {
                            return Err("enter three numbers".into());
                        }
                        json!(n)
                    }
                };
                m.set_parameter_default(name, value)
            }
            (k, AnimSelection::GraphNode { id }) if k.starts_with("prop:") => {
                let prop = &k["prop:".len()..];
                let node = m.graph().nodes.get(id).cloned().ok_or("no such node")?;
                let def = node_def(&node.node_type).ok_or("unknown node type")?;
                let kind = def.props.iter().find(|p| p.name == prop).map(|p| p.kind).ok_or("no such property")?;
                let value = match kind {
                    PropKind::Float { .. } => parse_f32(&text).map(|v| json!(v)).ok_or("enter a number")?,
                    PropKind::Int { .. } => text.trim().parse::<i64>().map(|v| json!(v)).map_err(|_| "enter a whole number".to_string())?,
                    PropKind::Vector => {
                        let n: Vec<f32> = text.split(',').filter_map(|t| t.trim().parse().ok()).collect();
                        if n.len() != 3 {
                            return Err("enter three numbers".into());
                        }
                        json!(n)
                    }
                    PropKind::BoneList | PropKind::NameList => json!(text.split(',').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>()),
                    _ => json!(text.trim()),
                };
                m.set_property(id, prop, value)
            }
            _ => Ok(()),
        });
        self.push_to_canvas(false, cx);
        self.refresh_refs();
    }

    fn pick_ref(&mut self, key: &str, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.model.selection.clone();
        edit(self, window, cx, |m| {
            if key == "skeleton" {
                let skeleton = id.as_deref().and_then(AssetRef::<SkeletonAssetKind>::parse);
                m.doc.apply("Set skeleton", |a| a.skeleton = skeleton);
                return Ok(());
            }
            let prop = key.strip_prefix("prop:").ok_or("unknown reference")?;
            let AnimSelection::GraphNode { id: node } = &selection else { return Err("select a node".into()) };
            m.set_property(node, prop, json!(id.clone().unwrap_or_default()))
        });
        self.push_to_canvas(false, cx);
        self.refresh_refs();
    }
}

impl EditorBody for AnimGraphEditor {
    type Model = AnimGraphModel;
    fn model(&self) -> &AnimGraphModel {
        &self.model
    }
    fn model_mut(&mut self) -> &mut AnimGraphModel {
        &mut self.model
    }
    fn chrome(&mut self) -> &mut Chrome {
        &mut self.chrome
    }
    fn chrome_ref(&self) -> &Chrome {
        &self.chrome
    }
    fn icon(&self) -> IconName {
        IconName::Component
    }
    fn kind_title(&self) -> &'static str {
        "Animation Graph"
    }
    fn left_title(&self) -> &'static str {
        "Graphs & Parameters"
    }

    fn after_change(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // undo, redo, reload and inspector edits change the model under the canvas
        if self.synced != self.model.doc.generation() {
            // the open graph may have disappeared with an undo
            if let GraphRef::Sub(id) = self.model.active().clone() {
                if self.model.asset().subgraph(&id).is_none() {
                    self.model.open_graph(GraphRef::Main);
                }
            }
            self.push_to_canvas(false, cx);
            self.preview_stale = true;
        }
    }

    fn select_location(&mut self, location: &str, _window: &mut Window, cx: &mut Context<Self>) {
        // locations are node ids in the open graph, sub-graph ids or parameter ids
        if self.model.select_node(location) {
            let id = location.to_string();
            self.canvas.update(cx, |c, cx| {
                c.select_nodes([id.clone()], cx);
                c.center_on(&id, cx);
            });
        } else if let Some(sub) = self.model.asset().subgraphs.iter().find(|s| s.id == location).map(|s| s.id.clone()) {
            self.open_graph(GraphRef::Sub(sub), cx);
        } else if let Some(p) = self.model.asset().parameters.iter().find(|p| p.id == location).map(|p| p.name.clone()) {
            self.model.selection = AnimSelection::GraphParameter { name: p };
        }
        cx.notify();
    }

    fn toolbar_extra(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let mut out: Vec<AnyElement> = Vec::new();
        let crumbs = self.model.breadcrumb();
        let last = crumbs.len().saturating_sub(1);
        for (i, (graph, name)) in crumbs.into_iter().enumerate() {
            let target = graph.clone();
            out.push(
                Button::new(SharedString::from(format!("ag-crumb-{i}")))
                    .label(name)
                    .xsmall()
                    .ghost()
                    .selected(i == last)
                    .on_click(cx.listener(move |this, _, _, cx| this.open_graph(target.clone(), cx)))
                    .into_any_element(),
            );
            if i < last {
                out.push(Icon::new(IconName::ChevronRight).size(px(11.0)).text_color(theme.muted_foreground).into_any_element());
            }
        }
        if self.model.graph_kind() == GraphKind::StateMachine {
            out.push(Button::new("ag-add-state").icon(IconName::Plus).label("State").small().ghost().on_click(cx.listener(|this, _, window, cx| {
                let n = this.model.graph().nodes.len() as f32;
                edit(this, window, cx, |m| m.add_state("State", (200.0 * (n % 4.0), 160.0 * (n / 4.0).floor())).map(|_| ()));
                this.push_to_canvas(false, cx);
            })).into_any_element());
            out.push(Button::new("ag-add-transition").icon(IconName::Link).label("Transition").small().ghost().tooltip("Select two states (click, then ctrl-click) and add a transition between them").on_click(cx.listener(|this, _, window, cx| {
                let selected: Vec<String> = this.canvas.read(cx).controller().selection.nodes.iter().cloned().collect();
                let states: Vec<String> = selected.into_iter().filter(|id| this.model.graph().nodes.get(id).is_some_and(|n| n.node_type == "sm.state")).collect();
                edit(this, window, cx, |m| match states.as_slice() {
                    [a, b] => m.add_transition(a, b).map(|_| ()),
                    [a] => m.add_transition(a, a).map(|_| ()),
                    _ => Err("select the state to leave and the state to enter".into()),
                });
                this.push_to_canvas(false, cx);
            })).into_any_element());
        }
        out
    }

    fn left(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let a = self.model.asset().clone();
        let selection = self.model.selection.clone();
        let mut list = v_flex().w_full();
        list = list.child(div().px_2().py_1().text_xs().font_semibold().text_color(theme.muted_foreground).child("GRAPHS"));
        let mut graphs: Vec<(GraphRef, String, &'static str)> = vec![(GraphRef::Main, "Main graph".into(), "main")];
        for s in &a.subgraphs {
            graphs.push((GraphRef::Sub(s.id.clone()), s.name.clone(), match s.kind {
                GraphKind::StateMachine => "machine",
                GraphKind::StatePose => "state",
                GraphKind::TransitionRule => "rule",
                GraphKind::Layer => "layer",
                GraphKind::Main => "main",
            }));
        }
        for (i, (graph, name, tag)) in graphs.into_iter().enumerate() {
            let active = *self.model.active() == graph;
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("ag-g-{i}")))
                    .w_full()
                    .px_2()
                    .py_0p5()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .when(active, |d| d.bg(theme.accent))
                    .hover(|d| d.bg(theme.secondary))
                    .child(div().flex_1().text_xs().text_color(theme.foreground).child(name))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(tag))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_graph(graph.clone(), cx))),
            );
        }
        if self.model.graph_kind() == GraphKind::StateMachine {
            list = list.child(div().px_2().pt_2().pb_1().text_xs().font_semibold().text_color(theme.muted_foreground).child("STATES & TRANSITIONS"));
            for row in self.model.machine_summary() {
                match row {
                    MachineRow::State { id, name, default } => {
                        list = list.child(
                            h_flex()
                                .id(SharedString::from(format!("ag-st-{id}")))
                                .w_full()
                                .px_2()
                                .py_0p5()
                                .gap_2()
                                .cursor_pointer()
                                .hover(|d| d.bg(theme.secondary))
                                .child(div().flex_1().text_xs().text_color(theme.foreground).child(name))
                                .child(div().text_xs().text_color(theme.success).child(if default { "default" } else { "" }))
                                .on_click(cx.listener(move |this, _, window, cx| this.select_location(&id.clone(), window, cx))),
                        );
                    }
                    MachineRow::Transition { id, from, to, duration, priority } => {
                        list = list.child(
                            h_flex()
                                .id(SharedString::from(format!("ag-tr-{id}")))
                                .w_full()
                                .px_2()
                                .py_0p5()
                                .gap_2()
                                .cursor_pointer()
                                .hover(|d| d.bg(theme.secondary))
                                .child(div().flex_1().text_xs().text_color(theme.foreground).child(format!("{} \u{2192} {}", from.unwrap_or_else(|| "?".into()), to.unwrap_or_else(|| "?".into()))))
                                .child(div().text_xs().text_color(theme.muted_foreground).child(format!("{} s \u{b7} p{priority}", fmt_f32(duration))))
                                .on_click(cx.listener(move |this, _, window, cx| this.select_location(&id.clone(), window, cx))),
                        );
                    }
                }
            }
        }
        list = list.child(div().px_2().pt_2().pb_1().text_xs().font_semibold().text_color(theme.muted_foreground).child("PARAMETERS"));
        for p in &a.parameters {
            let name = p.name.clone();
            let sel = matches!(&selection, AnimSelection::GraphParameter { name: n } if *n == p.name);
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("ag-p-{}", p.id)))
                    .w_full()
                    .px_2()
                    .py_0p5()
                    .gap_2()
                    .cursor_pointer()
                    .when(sel, |d| d.bg(theme.accent))
                    .hover(|d| d.bg(theme.secondary))
                    .child(div().flex_1().text_xs().text_color(theme.foreground).child(p.name.clone()))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(p.ty.pin_type().type_name()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.selection = AnimSelection::GraphParameter { name: name.clone() };
                        cx.notify();
                    })),
            );
        }
        let add = |ty: ParamType, label: &'static str, cx: &mut Context<Self>| {
            Button::new(SharedString::from(format!("ag-add-{label}"))).label(label).xsmall().ghost().on_click(cx.listener(move |this, _, window, cx| {
                let n = this.model.asset().parameters.len() + 1;
                edit(this, window, cx, |m| m.add_parameter(&format!("{label}{n}"), ty).map(|_| ()));
                this.push_to_canvas(false, cx);
            }))
        };
        list = list.child(h_flex().px_2().py_1().gap_1().flex_wrap().child(add(ParamType::Float, "Float", cx)).child(add(ParamType::Bool, "Bool", cx)).child(add(ParamType::Int, "Int", cx)).child(add(ParamType::Vector, "Vector", cx)));
        list.into_any_element()
    }

    fn center(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = graph_theme(cx);
        let canvas = self.canvas.clone();
        if canvas.read(cx).theme() != &theme {
            canvas.update(cx, |c, cx| c.set_theme(theme, cx));
        }
        let preview = self.preview_panel(window, cx);
        v_flex().size_full().child(div().flex_1().min_h_0().child(canvas)).child(preview).into_any_element()
    }

    fn inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let selected_ties: Vec<usize> = self.canvas.read(cx).controller().selection.ties.iter().copied().collect();
        if selected_ties.len() == 1 {
            let index = selected_ties[0];
            if let Some(tie) = self.canvas.read(cx).graph().zip_ties.get(index).cloned() {
                let theme = cx.theme().clone();
                let body = v_flex().w_full().gap_2()
                    .child(div().text_sm().font_semibold().text_color(theme.foreground).child("Zip Tie"))
                    .child(self.widgets.text_field(
                        &format!("zip_tie.straight_length.{index}"),
                        "Straight-through length",
                        &fmt_f32(tie.straight_length as f32),
                        window,
                        cx,
                    ))
                    .child(value_row("Wires", tie.wires.len().to_string(), cx))
                    .child(value_row("Position", format!("{}, {}", fmt_f32(tie.position.x as f32), fmt_f32(tie.position.y as f32)), cx))
                    .child(div().text_xs().text_color(theme.muted_foreground).child("Wires run straight through this length, with smooth transitions on either side."));
                return body.into_any_element();
            }
        }
        match self.model.selection.clone() {
            AnimSelection::GraphNode { id } => self.node_inspector(&id, window, cx),
            AnimSelection::GraphParameter { name } => self.parameter_inspector(&name, window, cx),
            _ => {
                let a = self.model.asset().clone();
                let status = self.statuses.get("skeleton").cloned().unwrap_or(RefStatus::Unset);
                let root = self.chrome.root.clone();
                let mut body = v_flex().w_full().gap_2().child(div().text_sm().font_semibold().text_color(cx.theme().foreground).child("Animation Graph"));
                body = body.child(self.widgets.text_field("name", "Name", &a.name, window, cx));
                body = body.child(self.widgets.ref_field("skeleton", "Skeleton", SkeletonAssetKind::KIND, None, a.skeleton.as_ref().map(|s| s.as_str().to_string()), &status, &root, window, cx));
                body = body.child(value_row("Open graph", self.model.graph_title(), cx));
                body = body.child(value_row("Nodes in it", self.model.graph().nodes.len().to_string(), cx));
                body = body.child(value_row("Sub-graphs", a.subgraphs.len().to_string(), cx));
                body = body.child(value_row("Parameters", a.parameters.len().to_string(), cx));
                body = body.child(bone_mask_panel::library_rows(self, window, cx));
                body = body.child(div().text_xs().text_color(cx.theme().muted_foreground).child("Right-click the canvas for the node list; double-click a state machine, state or transition to open its graph."));
                body.into_any_element()
            }
        }
    }
}

impl Render for AnimGraphEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame(self, window, cx)
    }
}

crate::editor_model!(AnimGraphModel, <AnimationGraphAssetKind as AssetKind>::KIND);
crate::anim_panel!(AnimGraphEditor, "animation-graph-editor");

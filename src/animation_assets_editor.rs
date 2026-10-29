//! The interim editor for the high-level animation assets: Blend Space,
//! Montage, Animation Graph, Retarget Profile, IK Rig, IK Retargeter and Pose
//! Database.
//!
//! It edits the typed documents (never raw JSON), lists every reference as a
//! typed picker, shows the validator's diagnostics, the asset's dependencies
//! and the assets that use it, and saves through the real codecs. The
//! purpose-built editors of Phase 4 replace the body of this panel; the open,
//! save, indexing and diagnostics path stays.

use std::path::PathBuf;

use gpui::*;
use plugin_editor_api::PluginError;
use pulsar_reflection::{
    AnimationGraphAssetKind, AnimationMontageAssetKind, AnimationSequenceAssetKind, AssetKind,
    BlendSpaceAssetKind, IkRetargeterAssetKind, IkRigAssetKind, PoseDatabaseAssetKind,
    RetargetProfileAssetKind, SkeletonAssetKind,
};
use serde_json::Value;
use skeletal_assets::anim_graph::catalogue::{node_def, PropKind};
use skeletal_assets::anim_graph::{data_connection, instantiate_node, AnimationGraphAsset};
use skeletal_assets::blend_space::{BlendSample, BlendSpaceAsset};
use skeletal_assets::diagnostics::Diagnostic;
use skeletal_assets::ik_retarget::IkRetargetAsset;
use skeletal_assets::ik_rig::IkRigAsset;
use skeletal_assets::issue::Severity;
use skeletal_assets::montage::{AnimationMontageAsset, MontageSegment, SlotTrack, DEFAULT_SLOT, DEFAULT_SLOT_GROUP};
use skeletal_assets::pose_db::{PoseDbAsset, PoseSource};
use skeletal_assets::report::asset_report;
use skeletal_assets::retarget::RetargetProfileAsset;
use ui::button::Button;
use ui::dock::Panel;
use ui::popover::Popover;
use ui::{h_flex, v_flex, ActiveTheme, Sizable, StyledExt};
use ui_common::{AssetPickedEvent, AssetQuery, MeshAssetPicker};

use crate::core::asset_index::{self, ProjectAssetSource};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationAssetEditorKind {
    BlendSpace,
    Montage,
    Graph,
    RetargetProfile,
    IkRig,
    IkRetarget,
    PoseDb,
}

impl AnimationAssetEditorKind {
    pub const ALL: [Self; 7] = [
        Self::BlendSpace,
        Self::Montage,
        Self::Graph,
        Self::RetargetProfile,
        Self::IkRig,
        Self::IkRetarget,
        Self::PoseDb,
    ];

    pub fn editor_id(self) -> &'static str {
        match self {
            Self::BlendSpace => "blend-space-editor",
            Self::Montage => "animation-montage-editor",
            Self::Graph => "animation-graph-editor",
            Self::RetargetProfile => "retarget-profile-editor",
            Self::IkRig => "ik-rig-editor",
            Self::IkRetarget => "ik-retargeter-editor",
            Self::PoseDb => "pose-search-database-editor",
        }
    }
    pub fn file_type(self) -> &'static str {
        match self {
            Self::BlendSpace => "blend_space",
            Self::Montage => "animation_montage",
            Self::Graph => "animation_graph",
            Self::RetargetProfile => "retarget_profile",
            Self::IkRig => "ik_rig",
            Self::IkRetarget => "ik_retargeter",
            Self::PoseDb => "pose_database",
        }
    }
    pub fn extension(self) -> &'static str {
        match self {
            Self::BlendSpace => skeletal_assets::blend_space::BLEND_SPACE_EXTENSION,
            Self::Montage => skeletal_assets::montage::MONTAGE_EXTENSION,
            Self::Graph => skeletal_assets::anim_graph::ANIMATION_GRAPH_EXTENSION,
            Self::RetargetProfile => skeletal_assets::retarget::RETARGET_EXTENSION,
            Self::IkRig => skeletal_assets::ik_rig::IK_RIG_EXTENSION,
            Self::IkRetarget => skeletal_assets::ik_retarget::IK_RETARGET_EXTENSION,
            Self::PoseDb => skeletal_assets::pose_db::POSE_DB_EXTENSION,
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::BlendSpace => "Blend Space",
            Self::Montage => "Animation Montage",
            Self::Graph => "Animation Graph",
            Self::RetargetProfile => "Retarget Profile",
            Self::IkRig => "IK Rig",
            Self::IkRetarget => "IK Retargeter",
            Self::PoseDb => "Pose Search Database",
        }
    }
    pub fn asset_kind(self) -> &'static str {
        match self {
            Self::BlendSpace => BlendSpaceAssetKind::KIND,
            Self::Montage => AnimationMontageAssetKind::KIND,
            Self::Graph => AnimationGraphAssetKind::KIND,
            Self::RetargetProfile => RetargetProfileAssetKind::KIND,
            Self::IkRig => IkRigAssetKind::KIND,
            Self::IkRetarget => IkRetargeterAssetKind::KIND,
            Self::PoseDb => PoseDatabaseAssetKind::KIND,
        }
    }

    /// The text of a new, valid, empty asset of this kind. Used as the create
    /// menu's default content and when a file is opened before it has any.
    pub fn default_text(self) -> String {
        match self {
            Self::BlendSpace => BlendSpaceAsset::default().to_text(),
            Self::Montage => AnimationMontageAsset::default().to_text(),
            Self::Graph => AnimationGraphAsset::default().to_text(),
            Self::RetargetProfile => RetargetProfileAsset::new("New Retarget Profile", None, None).to_text(),
            Self::IkRig => IkRigAsset::new("New IK Rig", None).to_text(),
            Self::IkRetarget => IkRetargetAsset::new("New IK Retargeter", None, None).to_text(),
            Self::PoseDb => PoseDbAsset::new("New Pose Database", None).to_text(),
        }
        .unwrap_or_default()
    }

    /// The default content as JSON, the form file type definitions take.
    pub fn default_json(self) -> Value {
        serde_json::from_str(&self.default_text()).unwrap_or(Value::Null)
    }
}

fn id_of<K: AssetKind>(reference: &Option<pulsar_reflection::AssetRef<K>>) -> Option<String> {
    reference.as_ref().map(|r| r.as_str().to_string())
}

/// A typed document of any of the seven kinds.
#[derive(Clone)]
enum Document {
    BlendSpace(BlendSpaceAsset),
    Montage(AnimationMontageAsset),
    Graph(AnimationGraphAsset),
    Retarget(RetargetProfileAsset),
    IkRig(IkRigAsset),
    IkRetarget(IkRetargetAsset),
    PoseDb(PoseDbAsset),
}

/// A reference field of a document, addressed by its JSON path.
#[derive(Clone, Debug)]
struct Slot {
    path: Vec<String>,
    label: String,
    kind: &'static str,
    current: Option<String>,
}

impl Document {
    fn parse(kind: AnimationAssetEditorKind, text: &str) -> Result<(Self, bool), String> {
        macro_rules! migrating {
            ($ty:ty, $variant:ident) => {
                <$ty>::parse_migrating(text)
                    .map(|(a, m)| (Self::$variant(a), m.id_assigned))
                    .map_err(|e| e.to_string())
            };
        }
        match kind {
            AnimationAssetEditorKind::BlendSpace => migrating!(BlendSpaceAsset, BlendSpace),
            AnimationAssetEditorKind::Montage => migrating!(AnimationMontageAsset, Montage),
            AnimationAssetEditorKind::Graph => migrating!(AnimationGraphAsset, Graph),
            AnimationAssetEditorKind::RetargetProfile => RetargetProfileAsset::parse(text).map(|a| (Self::Retarget(a), false)).map_err(|e| e.to_string()),
            AnimationAssetEditorKind::IkRig => IkRigAsset::parse(text).map(|a| (Self::IkRig(a), false)).map_err(|e| e.to_string()),
            AnimationAssetEditorKind::IkRetarget => IkRetargetAsset::parse(text).map(|a| (Self::IkRetarget(a), false)).map_err(|e| e.to_string()),
            AnimationAssetEditorKind::PoseDb => PoseDbAsset::parse(text).map(|a| (Self::PoseDb(a), false)).map_err(|e| e.to_string()),
        }
    }

    fn to_text(&self) -> anyhow::Result<String> {
        match self {
            Self::BlendSpace(a) => a.to_text(),
            Self::Montage(a) => a.to_text(),
            Self::Graph(a) => a.to_text(),
            Self::Retarget(a) => a.to_text(),
            Self::IkRig(a) => a.to_text(),
            Self::IkRetarget(a) => a.to_text(),
            Self::PoseDb(a) => a.to_text(),
        }
    }

    fn id(&self) -> String {
        match self {
            Self::BlendSpace(a) => a.id.as_str(),
            Self::Montage(a) => a.id.as_str(),
            Self::Graph(a) => a.id.as_str(),
            Self::Retarget(a) => a.id.as_str(),
            Self::IkRig(a) => a.id.as_str(),
            Self::IkRetarget(a) => a.id.as_str(),
            Self::PoseDb(a) => a.id.as_str(),
        }
        .to_string()
    }

    fn name(&self) -> String {
        match self {
            Self::BlendSpace(a) => a.name.clone(),
            Self::Montage(a) => a.name.clone(),
            Self::Graph(a) => a.name.clone(),
            Self::Retarget(a) => a.name.clone(),
            Self::IkRig(a) => a.name.clone(),
            Self::IkRetarget(a) => a.name.clone(),
            Self::PoseDb(a) => a.name.clone(),
        }
    }

    /// Every reference the document holds, set or not.
    fn slots(&self) -> Vec<Slot> {
        let slot = |path: &[&str], label: String, kind: &'static str, current: Option<String>| Slot {
            path: path.iter().map(|s| s.to_string()).collect(),
            label,
            kind,
            current,
        };
        let skeleton = SkeletonAssetKind::KIND;
        let seq = AnimationSequenceAssetKind::KIND;
        let mut out = Vec::new();
        match self {
            Self::BlendSpace(a) => {
                out.push(slot(&["skeleton"], "Skeleton".into(), skeleton, id_of(&a.skeleton)));
                for (i, s) in a.samples.iter().enumerate() {
                    let i = i.to_string();
                    out.push(slot(&["samples", &i, "sequence"], format!("Sample {i} (x {})", s.x), seq, id_of(&s.sequence)));
                }
            }
            Self::Montage(a) => {
                out.push(slot(&["skeleton"], "Skeleton".into(), skeleton, id_of(&a.skeleton)));
                for (t, track) in a.tracks.iter().enumerate() {
                    for (s, seg) in track.segments.iter().enumerate() {
                        let (t, s) = (t.to_string(), s.to_string());
                        out.push(slot(&["tracks", &t, "segments", &s, "sequence"], format!("{} segment {s}", track.slot), seq, id_of(&seg.sequence)));
                    }
                }
            }
            Self::Graph(a) => {
                out.push(slot(&["skeleton"], "Skeleton".into(), skeleton, id_of(&a.skeleton)));
                for (index, (sub, _, graph)) in a.graphs().enumerate() {
                    let mut ids: Vec<&String> = graph.nodes.keys().collect();
                    ids.sort();
                    for id in ids {
                        let node = &graph.nodes[id];
                        let Some(def) = node_def(&node.node_type) else { continue };
                        for prop in def.props {
                            let PropKind::Asset(kind) = prop.kind else { continue };
                            let mut path: Vec<String> = if sub.is_some() {
                                vec!["subgraphs".into(), (index - 1).to_string(), "graph".into()]
                            } else {
                                vec!["main".into()]
                            };
                            path.extend(["nodes".into(), id.clone(), "properties".into(), prop.name.into()]);
                            let current = node.properties.get(prop.name).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
                            out.push(Slot { path, label: format!("{} \u{b7} {}", def.title, prop.name), kind, current });
                        }
                    }
                }
            }
            Self::Retarget(a) => {
                out.push(slot(&["source_skeleton"], "Source skeleton".into(), skeleton, id_of(&a.source_skeleton)));
                out.push(slot(&["target_skeleton"], "Target skeleton".into(), skeleton, id_of(&a.target_skeleton)));
                out.push(slot(&["source_rig"], "Source IK rig".into(), IkRigAssetKind::KIND, id_of(&a.source_rig)));
                out.push(slot(&["target_rig"], "Target IK rig".into(), IkRigAssetKind::KIND, id_of(&a.target_rig)));
            }
            Self::IkRig(a) => out.push(slot(&["skeleton"], "Skeleton".into(), skeleton, id_of(&a.skeleton))),
            Self::IkRetarget(a) => {
                out.push(slot(&["source_rig"], "Source IK rig".into(), IkRigAssetKind::KIND, id_of(&a.source_rig)));
                out.push(slot(&["target_rig"], "Target IK rig".into(), IkRigAssetKind::KIND, id_of(&a.target_rig)));
            }
            Self::PoseDb(a) => {
                out.push(slot(&["skeleton"], "Skeleton".into(), skeleton, id_of(&a.skeleton)));
                for (i, s) in a.sources.iter().enumerate() {
                    let i = i.to_string();
                    match &s.source {
                        PoseSource::Sequence { sequence } => out.push(slot(&["sources", &i, "sequence"], format!("Source {i}"), seq, Some(sequence.as_str().to_string()))),
                        PoseSource::BlendSpace { blend_space } => out.push(slot(&["sources", &i, "blend_space"], format!("Source {i}"), BlendSpaceAssetKind::KIND, Some(blend_space.as_str().to_string()))),
                    }
                }
            }
        }
        out
    }

    /// The document with the reference at `path` replaced.
    fn with_reference(&self, kind: AnimationAssetEditorKind, path: &[String], id: &str) -> Result<Self, String> {
        let text = self.to_text().map_err(|e| e.to_string())?;
        let mut value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let mut at = &mut value;
        for (i, part) in path.iter().enumerate() {
            let last = i + 1 == path.len();
            let next = match part.parse::<usize>() {
                Ok(index) => at.as_array_mut().and_then(|a| a.get_mut(index)),
                Err(_) => {
                    if last {
                        at.as_object_mut().map(|o| o.entry(part.clone()).or_insert(Value::Null))
                    } else {
                        at.as_object_mut().and_then(|o| o.get_mut(part))
                    }
                }
            };
            at = next.ok_or_else(|| format!("no field {}", path.join(".")))?;
        }
        *at = Value::String(id.to_string());
        Self::parse(kind, &value.to_string()).map(|(doc, _)| doc)
    }

    /// Append one more entry of the document's main repeating kind.
    fn add_entry(&mut self) -> bool {
        match self {
            Self::BlendSpace(a) => {
                let x = a.samples.last().map_or(a.x_axis.min, |s| s.x) + a.x_axis.span() / a.x_axis.grid_divisions.max(1) as f32;
                let x = x.min(a.x_axis.max);
                let y = a.y_axis.as_ref().map(|y| y.min);
                a.samples.push(BlendSample::new(None, x, y));
                true
            }
            Self::Montage(a) => {
                if a.tracks.is_empty() {
                    a.tracks.push(SlotTrack::new(DEFAULT_SLOT, DEFAULT_SLOT_GROUP));
                }
                a.tracks[0].segments.push(MontageSegment::new(None, 0.0));
                true
            }
            Self::Graph(a) => {
                let n = a.main.nodes.len();
                let id = format!("sequence_{}", uuid::Uuid::new_v4().simple());
                let Some(node) = instantiate_node(&id, "anim.sequence_player", (-300.0, 150.0 * n as f32), &|_| None) else { return false };
                if a.main.add_node(node).is_err() {
                    return false;
                }
                let output_free = a.main.nodes.values().find(|n| n.node_type == "anim.output_pose").is_some_and(|o| !a.main.connections.iter().any(|c| c.target_node == o.id));
                if output_free {
                    if let Some(output) = a.main.nodes.values().find(|n| n.node_type == "anim.output_pose").map(|n| n.id.clone()) {
                        let _ = a.main.add_connection(data_connection(&id, "pose", &output, "pose"));
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn can_add(&self) -> Option<&'static str> {
        match self {
            Self::BlendSpace(_) => Some("Add sample"),
            Self::Montage(_) => Some("Add segment"),
            Self::Graph(_) => Some("Add sequence player"),
            _ => None,
        }
    }
}

/// File based panel. References are written as typed stable IDs; each picker
/// filters by asset kind and resolves choices through the project index.
pub struct AnimationAssetEditor {
    kind: AnimationAssetEditorKind,
    path: PathBuf,
    project_root: PathBuf,
    document: Document,
    diagnostics: Vec<Diagnostic>,
    pickers: Vec<(String, Entity<MeshAssetPicker>)>,
    subscriptions: Vec<Subscription>,
    dirty: bool,
    status: Option<String>,
    focus: FocusHandle,
}

fn plugin_error(message: impl ToString) -> PluginError {
    PluginError::Other { message: message.to_string() }
}

impl AnimationAssetEditor {
    pub fn open(
        path: PathBuf,
        kind: AnimationAssetEditorKind,
        _window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<Self>, PluginError> {
        let root = crate::core::library::project_root(&path);
        let existing = engine_fs::virtual_fs::read_file(&path).ok().and_then(|b| String::from_utf8(b).ok());
        let is_new = existing.as_ref().map_or(true, |t| t.trim().is_empty());
        let text = if is_new { kind.default_text() } else { existing.unwrap_or_default() };
        // A file that cannot be read is an error to show, never one to overwrite.
        let (document, id_assigned) = Document::parse(kind, &text)
            .map_err(|e| plugin_error(format!("{}: {e}", path.display())))?;
        let mut editor = Self {
            kind,
            path,
            project_root: root,
            document,
            diagnostics: Vec::new(),
            pickers: Vec::new(),
            subscriptions: Vec::new(),
            dirty: false,
            status: None,
            focus: cx.focus_handle(),
        };
        // A new file, or one whose id was just assigned, must be written so the
        // id is stable from now on.
        if is_new || id_assigned {
            if let Some(parent) = editor.path.parent() {
                engine_fs::virtual_fs::create_dir_all(parent).map_err(plugin_error)?;
            }
            editor.save().map_err(plugin_error)?;
        } else {
            asset_index::record_asset(&editor.project_root, &editor.path);
            editor.refresh_diagnostics();
        }
        Ok(cx.new(|_| editor))
    }

    fn refresh_diagnostics(&mut self) {
        let Some(source) = ProjectAssetSource::open(&self.project_root) else {
            self.diagnostics = Vec::new();
            return;
        };
        let bytes = self.document.to_text().unwrap_or_default().into_bytes();
        self.diagnostics = asset_report(&source, self.kind.asset_kind(), &bytes);
    }

    fn save(&mut self) -> anyhow::Result<()> {
        let text = self.document.to_text()?;
        engine_fs::virtual_fs::write_file_atomically(&self.path, text.as_bytes())?;
        self.dirty = false;
        asset_index::record_asset(&self.project_root, &self.path);
        self.refresh_diagnostics();
        Ok(())
    }

    fn apply(&mut self, document: Document, cx: &mut Context<Self>) {
        self.document = document;
        self.dirty = true;
        self.status = match self.save() {
            Ok(()) => None,
            Err(error) => {
                log::error!("Failed to save animation asset: {error}");
                Some(format!("Could not save: {error}"))
            }
        };
        cx.notify();
    }

    fn ensure_picker<K: AssetKind>(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>, path: Vec<String>) -> Entity<MeshAssetPicker> {
        if let Some((_, picker)) = self.pickers.iter().find(|(k, _)| *k == key) {
            return picker.clone();
        }
        let picker = cx.new(|cx| {
            MeshAssetPicker::new(String::new(), Vec::new(), Some(self.project_root.clone()), AssetQuery::for_kind::<K>(), window, cx)
        });
        self.subscriptions.push(cx.subscribe_in(&picker, window, move |this, picker, _: &AssetPickedEvent, _window, cx| {
            let Some(reference) = picker.read(cx).selected_asset_ref::<K>() else { return };
            match this.document.with_reference(this.kind, &path, reference.as_str()) {
                Ok(document) => this.apply(document, cx),
                Err(error) => {
                    this.status = Some(format!("Could not set the reference: {error}"));
                    cx.notify();
                }
            }
        }));
        self.pickers.push((key, picker.clone()));
        picker
    }

    fn render_slot(&mut self, slot: &Slot, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let key = slot.path.join(".");
        let (path, label) = (slot.path.clone(), slot.label.clone());
        let picker = match slot.kind {
            k if k == SkeletonAssetKind::KIND => self.ensure_picker::<SkeletonAssetKind>(key.clone(), window, cx, path),
            k if k == AnimationSequenceAssetKind::KIND => self.ensure_picker::<AnimationSequenceAssetKind>(key.clone(), window, cx, path),
            k if k == BlendSpaceAssetKind::KIND => self.ensure_picker::<BlendSpaceAssetKind>(key.clone(), window, cx, path),
            k if k == AnimationMontageAssetKind::KIND => self.ensure_picker::<AnimationMontageAssetKind>(key.clone(), window, cx, path),
            k if k == AnimationGraphAssetKind::KIND => self.ensure_picker::<AnimationGraphAssetKind>(key.clone(), window, cx, path),
            k if k == IkRigAssetKind::KIND => self.ensure_picker::<IkRigAssetKind>(key.clone(), window, cx, path),
            k if k == RetargetProfileAssetKind::KIND => self.ensure_picker::<RetargetProfileAssetKind>(key.clone(), window, cx, path),
            k if k == IkRetargeterAssetKind::KIND => self.ensure_picker::<IkRetargeterAssetKind>(key.clone(), window, cx, path),
            _ => self.ensure_picker::<PoseDatabaseAssetKind>(key.clone(), window, cx, path),
        };
        let shown = slot.current.as_deref().map_or_else(|| format!("{label}: unassigned"), |id| format!("{label}: {}", short_id(id)));
        let trigger = Button::new(format!("pick-{key}")).label(shown).small().outline().dropdown_caret(true);
        Popover::<MeshAssetPicker>::new(format!("popover-{key}"))
            .anchor(Corner::BottomLeft)
            .trigger(trigger)
            .content(move |_, _| picker.clone())
            .into_any_element()
    }
}

impl EventEmitter<ui::dock::PanelEvent> for AnimationAssetEditor {}
impl Focusable for AnimationAssetEditor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Panel for AnimationAssetEditor {
    fn panel_name(&self) -> &'static str {
        self.kind.editor_id()
    }
    fn tab_name(&self, _cx: &App) -> Option<SharedString> {
        self.path.file_name().map(|n| n.to_string_lossy().to_string().into())
    }
    fn tab_icon(&self, _cx: &App) -> Option<ui::IconName> {
        Some(ui::IconName::Keyframes)
    }
    fn tab_unsaved(&self, _cx: &App) -> bool {
        self.dirty
    }
    fn panel_file_path(&self, _cx: &App) -> Option<PathBuf> {
        Some(self.path.clone())
    }
    fn title(&self, _window: &Window, _cx: &App) -> AnyElement {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.kind.title().into())
            .into_any_element()
    }
}

impl Render for AnimationAssetEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let id = self.document.id();
        let mut body = v_flex().size_full().gap_3().p_4().bg(theme.background).overflow_y_scroll();
        body = body
            .child(div().text_lg().font_semibold().child(format!("{} \u{2014} {}", self.kind.title(), self.document.name())))
            .child(div().text_xs().text_color(theme.muted_foreground).child(format!("id {id}")));
        if let Some(status) = &self.status {
            body = body.child(div().text_sm().text_color(theme.danger).child(status.clone()));
        }

        for slot in self.document.slots() {
            body = body.child(self.render_slot(&slot, window, cx));
        }
        if let Some(label) = self.document.can_add() {
            let entity = cx.entity();
            body = body.child(Button::new("add-entry").label(label).small().on_click(move |_, _, cx| {
                entity.update(cx, |this, cx| {
                    let mut document = this.document.clone();
                    if document.add_entry() {
                        this.apply(document, cx);
                    }
                })
            }));
        }

        let heading = |text: String| div().text_sm().font_semibold().child(text);
        body = body.child(heading(format!("Problems ({})", self.diagnostics.len())));
        if self.diagnostics.is_empty() {
            body = body.child(div().text_xs().text_color(theme.muted_foreground).child("No problems found."));
        }
        for d in &self.diagnostics {
            let (color, label) = match d.severity {
                Severity::Error => (theme.danger, "Error"),
                Severity::Warning => (theme.warning, "Warning"),
                Severity::Info => (theme.muted_foreground, "Info"),
            };
            let mut line = format!("{label}: {}", d.message);
            if let Some(at) = &d.location {
                line.push_str(&format!(" ({at})"));
            }
            if let Some(repair) = &d.repair {
                line.push_str(&format!(" \u{2014} {repair}"));
            }
            body = body.child(div().text_xs().text_color(color).child(line));
        }

        let used_by = asset_index::used_by(&self.project_root, self.kind.asset_kind(), &id);
        body = body.child(heading(format!("Used by ({})", used_by.len())));
        for referrer in used_by.iter().take(50) {
            let name = referrer.path.strip_prefix(&self.project_root).unwrap_or(&referrer.path).display().to_string();
            body = body.child(h_flex().gap_2().text_xs().text_color(theme.muted_foreground).child(name).child(format!("({})", referrer.kind)));
        }
        body
    }
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Load `text` as `kind` and describe it, for the registration test.
#[cfg(test)]
pub(crate) fn parse_for_test(kind: AnimationAssetEditorKind, text: &str) -> Result<(String, bool), String> {
    Document::parse(kind, text).map(|(doc, assigned)| (doc.id(), assigned)).map(|(id, a)| (id, a))
}

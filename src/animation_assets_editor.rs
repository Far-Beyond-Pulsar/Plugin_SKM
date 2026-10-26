//! Editors for blend-space, montage and animation-graph assets.

use std::path::PathBuf;

use gpui::*;
use plugin_editor_api::PluginError;
use pulsar_reflection::{
    AnimationSequenceAssetKind, AssetKind, BlendSpaceAssetKind, SkeletonAssetKind,
};
use ui::button::Button;
use ui::dock::Panel;
use ui::popover::Popover;
use ui::{h_flex, v_flex, ActiveTheme, Sizable, StyledExt};
use ui_common::{AssetPickedEvent, AssetQuery, MeshAssetPicker};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationAssetEditorKind {
    BlendSpace,
    Montage,
    Graph,
}

impl AnimationAssetEditorKind {
    pub fn editor_id(self) -> &'static str {
        match self {
            Self::BlendSpace => "blend-space-editor",
            Self::Montage => "animation-montage-editor",
            Self::Graph => "animation-graph-editor",
        }
    }
    pub fn file_type(self) -> &'static str {
        match self {
            Self::BlendSpace => "blend_space",
            Self::Montage => "animation_montage",
            Self::Graph => "animation_graph",
        }
    }
    pub fn extension(self) -> &'static str {
        match self {
            Self::BlendSpace => skeletal_assets::animation_sets::BLEND_SPACE_EXTENSION,
            Self::Montage => skeletal_assets::animation_sets::MONTAGE_EXTENSION,
            Self::Graph => skeletal_assets::animation_sets::ANIMATION_GRAPH_EXTENSION,
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::BlendSpace => "Blend Space",
            Self::Montage => "Animation Montage",
            Self::Graph => "Animation Graph",
        }
    }
    fn asset_kind(self) -> &'static str {
        match self {
            Self::BlendSpace => BlendSpaceAssetKind::KIND,
            Self::Montage => pulsar_reflection::AnimationMontageAssetKind::KIND,
            Self::Graph => pulsar_reflection::AnimationGraphAssetKind::KIND,
        }
    }
    fn default_json(self) -> serde_json::Value {
        match self {
            Self::BlendSpace => {
                serde_json::to_value(skeletal_assets::animation_sets::BlendSpaceAsset::default())
                    .unwrap_or_default()
            }
            Self::Montage => serde_json::to_value(
                skeletal_assets::animation_sets::AnimationMontageAsset::default(),
            )
            .unwrap_or_default(),
            Self::Graph => serde_json::to_value(
                skeletal_assets::animation_sets::AnimationGraphAsset::default(),
            )
            .unwrap_or_default(),
        }
    }
}

/// File based panel. References are written as typed stable IDs; the picker
/// filters by file type and resolves each choice through the project index.
pub struct AnimationAssetEditor {
    kind: AnimationAssetEditorKind,
    path: PathBuf,
    project_root: PathBuf,
    data: serde_json::Value,
    pickers: Vec<(String, Entity<MeshAssetPicker>)>,
    subscriptions: Vec<Subscription>,
    dirty: bool,
    focus: FocusHandle,
}

impl AnimationAssetEditor {
    pub fn open(
        path: PathBuf,
        kind: AnimationAssetEditorKind,
        _window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<Self>, PluginError> {
        let root = crate::core::library::project_root(&path);
        let (mut data, is_new_file, parsed_existing) = match engine_fs::virtual_fs::read_file(&path)
        {
            Ok(bytes) => match String::from_utf8(bytes)
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
            {
                Some(data) => (data, false, true),
                None => {
                    log::warn!("Could not parse animation asset {}", path.display());
                    (kind.default_json(), false, false)
                }
            },
            Err(_) => (kind.default_json(), true, false),
        };
        let mut needs_save = is_new_file;
        let valid_id = data
            .get("id")
            .and_then(serde_json::Value::as_str)
            .and_then(pulsar_reflection::AssetRef::<BlendSpaceAssetKind>::parse);
        if valid_id.is_none() {
            data["id"] = serde_json::Value::String(uuid::Uuid::new_v4().to_string());
            needs_save |= parsed_existing;
        }
        if needs_save {
            if let Some(parent) = path.parent() {
                engine_fs::virtual_fs::create_dir_all(parent).map_err(|error| {
                    PluginError::Other {
                        message: error.to_string(),
                    }
                })?;
            }
            let text = serde_json::to_string_pretty(&data).map_err(|error| PluginError::Other {
                message: error.to_string(),
            })?;
            engine_fs::virtual_fs::write_file_atomically(&path, text.as_bytes()).map_err(
                |error| PluginError::Other {
                    message: error.to_string(),
                },
            )?;
        }
        if valid_id.is_some() || needs_save {
            if let Some(index) =
                engine_fs::open_project_file_index(&root).map_err(|error| PluginError::Other {
                    message: error.to_string(),
                })?
            {
                index
                    .refresh_path(&path)
                    .and_then(|_| {
                        index.record_typed_asset_id(
                            kind.asset_kind(),
                            data["id"].as_str().unwrap_or_default(),
                            &path,
                        )
                    })
                    .map_err(|error| PluginError::Other {
                        message: error.to_string(),
                    })?;
            }
        }
        Ok(cx.new(|cx| Self {
            kind,
            path,
            project_root: root,
            data,
            pickers: Vec::new(),
            subscriptions: Vec::new(),
            dirty: false,
            focus: cx.focus_handle(),
        }))
    }

    fn ensure_picker<K: AssetKind>(
        &mut self,
        path: String,
        current: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<MeshAssetPicker> {
        if let Some((_, picker)) = self.pickers.iter().find(|(key, _)| *key == path) {
            return picker.clone();
        }
        let picker = cx.new(|cx| {
            MeshAssetPicker::new(
                String::new(),
                Vec::new(),
                Some(self.project_root.clone()),
                AssetQuery::for_kind::<K>(),
                window,
                cx,
            )
        });
        let path_for_event = path.clone();
        self.subscriptions.push(cx.subscribe_in(
            &picker,
            window,
            move |this, picker, _: &AssetPickedEvent, _window, cx| {
                let selected = picker.read(cx).selected_asset_ref::<K>();
                if let Some(reference) = selected {
                    set_json_path(
                        &mut this.data,
                        &path_for_event,
                        serde_json::to_value(reference).unwrap_or(serde_json::Value::Null),
                    );
                    this.dirty = true;
                    if let Err(error) = this.save() {
                        log::error!("Failed to save animation asset: {error}");
                    }
                    cx.notify();
                }
            },
        ));
        // Existing refs remain visible by id; the picker starts unselected if
        // its path cannot be looked up from the project index.
        let _ = current;
        self.pickers.push((path, picker.clone()));
        picker
    }

    fn render_reference<K: AssetKind>(
        &mut self,
        key: String,
        label: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = get_json_path(&self.data, &key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let picker = self.ensure_picker::<K>(key.clone(), current.as_deref(), window, cx);
        let value = current.unwrap_or_else(|| "Unassigned".into());
        let trigger = Button::new(format!("pick-{key}"))
            .label(if value == "Unassigned" {
                label.to_owned()
            } else {
                format!("{label}: {}", short_id(&value))
            })
            .small()
            .outline()
            .dropdown_caret(true);
        Popover::<MeshAssetPicker>::new(format!("popover-{key}"))
            .anchor(Corner::BottomLeft)
            .trigger(trigger)
            .content(move |_, _| picker.clone())
            .into_any_element()
    }

    fn add_entry(&mut self, field: &'static str, value: serde_json::Value, cx: &mut Context<Self>) {
        if let Some(array) = self
            .data
            .get_mut(field)
            .and_then(serde_json::Value::as_array_mut)
        {
            array.push(value);
            self.dirty = true;
            if let Err(error) = self.save() {
                log::error!("Failed to save animation asset: {error}");
            }
            cx.notify();
        }
    }

    fn save(&mut self) -> anyhow::Result<()> {
        if !self.path.exists() {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let text = serde_json::to_string_pretty(&self.data)?;
        engine_fs::virtual_fs::write_file_atomically(&self.path, text.as_bytes())?;
        self.dirty = false;
        Ok(())
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
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string().into())
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
        let mut body = v_flex().size_full().gap_3().p_4().bg(theme.background);
        body = body.child(div().text_lg().font_semibold().child(self.kind.title()));
        body = body.child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("References are stable asset IDs. Choose assets from the project index."),
        );
        body = body.child(self.render_reference::<SkeletonAssetKind>(
            "skeleton".into(),
            "Choose Skeleton",
            window,
            cx,
        ));
        match self.kind {
            AnimationAssetEditorKind::BlendSpace => {
                let entries = self
                    .data
                    .get("samples")
                    .and_then(serde_json::Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                for i in 0..entries {
                    body = body.child(self.render_reference::<AnimationSequenceAssetKind>(
                        format!("samples.{i}.sequence"),
                        "Choose Sequence",
                        window,
                        cx,
                    ));
                }
                let entity = cx.entity();
                body = body.child(
                    Button::new("add-blend-sample")
                        .label("Add sequence sample")
                        .small()
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |this, cx| {
                                this.add_entry(
                                    "samples",
                                    serde_json::json!({"sequence": null, "x": 0.0}),
                                    cx,
                                )
                            })
                        }),
                );
            }
            AnimationAssetEditorKind::Montage => {
                let entries = self
                    .data
                    .get("segments")
                    .and_then(serde_json::Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                for i in 0..entries {
                    body = body.child(self.render_reference::<AnimationSequenceAssetKind>(
                        format!("segments.{i}.sequence"),
                        "Choose Sequence",
                        window,
                        cx,
                    ));
                }
                let entity = cx.entity();
                body = body.child(Button::new("add-montage-segment").label("Add sequence segment").small().on_click(move |_, _, cx| entity.update(cx, |this, cx| this.add_entry("segments", serde_json::json!({"sequence": null, "start": 0.0, "play_rate": 1.0, "looping": false}), cx))));
            }
            AnimationAssetEditorKind::Graph => {
                let nodes = self
                    .data
                    .get("nodes")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for (i, node) in nodes.iter().enumerate() {
                    match node.get("type").and_then(serde_json::Value::as_str) {
                        Some("sequence") => {
                            body = body.child(self.render_reference::<AnimationSequenceAssetKind>(
                                format!("nodes.{i}.sequence"),
                                "Choose Sequence",
                                window,
                                cx,
                            ))
                        }
                        Some("blend_space") => {
                            body = body.child(self.render_reference::<BlendSpaceAssetKind>(
                                format!("nodes.{i}.blend_space"),
                                "Choose Blend Space",
                                window,
                                cx,
                            ))
                        }
                        _ => {}
                    }
                }
                let entity = cx.entity();
                body = body.child(h_flex().gap_2().child(Button::new("add-graph-sequence").label("Add sequence node").small().on_click(move |_, _, cx| entity.update(cx, |this, cx| this.add_entry("nodes", serde_json::json!({"id": format!("sequence_{}", uuid::Uuid::new_v4().simple()), "name": "Sequence", "type": "sequence", "sequence": null}), cx)))).child({
                    let entity = cx.entity(); Button::new("add-graph-blendspace").label("Add blend-space node").small().on_click(move |_, _, cx| entity.update(cx, |this, cx| this.add_entry("nodes", serde_json::json!({"id": format!("blend_{}", uuid::Uuid::new_v4().simple()), "name": "Blend Space", "type": "blend_space", "blend_space": null}), cx)))
                }));
            }
        }
        body
    }
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn get_json_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    path.split('.').try_fold(value, |value, key| {
        if let Ok(index) = key.parse::<usize>() {
            value.get(index)
        } else {
            value.get(key)
        }
    })
}

fn set_json_path(value: &mut serde_json::Value, path: &str, replacement: serde_json::Value) {
    let mut parts = path.split('.').peekable();
    let mut current = value;
    while let Some(part) = parts.next() {
        let last = parts.peek().is_none();
        let child = if let Ok(index) = part.parse::<usize>() {
            current
                .as_array_mut()
                .and_then(|array| array.get_mut(index))
        } else {
            current
                .as_object_mut()
                .and_then(|object| object.get_mut(part))
        };
        let Some(child) = child else {
            return;
        };
        if last {
            *child = replacement;
            return;
        }
        current = child;
    }
}

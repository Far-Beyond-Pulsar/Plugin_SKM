//! Chrome shared by the animation asset editors: the toolbar, breadcrumb, file
//! banners, the left/centre/right frame, the problems / used-by / dependency
//! panel and the field and reference widgets.
//!
//! An editor view implements [`EditorBody`] (what its left, centre and
//! inspector columns show) and [`EditorHost`] (what a committed field or a
//! picked reference does to its model). [`frame`] lays them out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder;
use gpui::*;
use plugin_editor_api::{OpenAsset, Redo, Undo};
use skeletal_assets::diagnostics::Diagnostic;
use skeletal_assets::issue::Severity;
use skeletal_assets::picker::{Compat, PickerEntry};
use skeletal_assets::report::asset_report;
use ui::button::{Button, ButtonVariants};
use ui::input::{InputEvent, InputState, TextInput};
use ui::popover::Popover;
use ui::tag::Tag;
use ui::{h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, StyledExt};

use crate::anim_editors::document::SaveOutcome;
use crate::anim_editors::refs::{self, DepNode, DepState, ProjectRefIndex, RefIndex, RefStatus};
use crate::anim_editors::selection::AnimSelection;
use crate::core::asset_index::{self, ProjectAssetSource, Referrer};

/// What the shell needs from any editor's model.
pub trait EditorModel {
    fn path(&self) -> &Path;
    fn kind(&self) -> &'static str;
    fn asset_id(&self) -> String;
    fn asset_name(&self) -> String;
    fn asset_text(&self) -> Vec<u8>;
    fn is_dirty(&self) -> bool;
    fn can_undo(&self) -> bool;
    fn can_redo(&self) -> bool;
    fn undo_label(&self) -> Option<String>;
    fn redo_label(&self) -> Option<String>;
    fn undo(&mut self) -> bool;
    fn redo(&mut self) -> bool;
    fn save(&mut self) -> anyhow::Result<SaveOutcome>;
    fn save_overwrite(&mut self) -> anyhow::Result<()>;
    fn reload(&mut self) -> Result<(), String>;
    fn changed_on_disk(&self) -> bool;
    fn generation(&self) -> u64;
    fn validation(&self) -> Vec<Diagnostic>;
    fn selection(&self) -> &AnimSelection;
    fn clear_selection(&mut self);
}

/// Implements [`EditorModel`] for a model with `doc` and `selection` fields
/// and a `validation()` method.
#[macro_export]
macro_rules! editor_model {
    ($model:ty, $kind:expr) => {
        impl $crate::anim_editors::views::chrome::EditorModel for $model {
            fn path(&self) -> &std::path::Path {
                self.doc.path()
            }
            fn kind(&self) -> &'static str {
                $kind
            }
            fn asset_id(&self) -> String {
                $crate::anim_editors::document::AnimAsset::asset_id(self.doc.value())
            }
            fn asset_name(&self) -> String {
                $crate::anim_editors::document::AnimAsset::asset_name(self.doc.value())
            }
            fn asset_text(&self) -> Vec<u8> {
                $crate::anim_editors::document::AnimAsset::to_text(self.doc.value()).unwrap_or_default().into_bytes()
            }
            fn is_dirty(&self) -> bool {
                self.doc.is_dirty()
            }
            fn can_undo(&self) -> bool {
                self.doc.can_undo()
            }
            fn can_redo(&self) -> bool {
                self.doc.can_redo()
            }
            fn undo_label(&self) -> Option<String> {
                self.doc.undo_label()
            }
            fn redo_label(&self) -> Option<String> {
                self.doc.redo_label()
            }
            fn undo(&mut self) -> bool {
                self.doc.undo()
            }
            fn redo(&mut self) -> bool {
                self.doc.redo()
            }
            fn save(&mut self) -> anyhow::Result<$crate::anim_editors::document::SaveOutcome> {
                self.doc.save()
            }
            fn save_overwrite(&mut self) -> anyhow::Result<()> {
                self.doc.save_overwrite()
            }
            fn reload(&mut self) -> Result<(), String> {
                self.doc.reload()
            }
            fn changed_on_disk(&self) -> bool {
                self.doc.changed_on_disk()
            }
            fn generation(&self) -> u64 {
                self.doc.generation()
            }
            fn validation(&self) -> Vec<skeletal_assets::diagnostics::Diagnostic> {
                <$model>::validation(self)
            }
            fn selection(&self) -> &$crate::anim_editors::selection::AnimSelection {
                &self.selection
            }
            fn clear_selection(&mut self) {
                self.selection = $crate::anim_editors::selection::AnimSelection::None;
            }
        }
    };
}

/// Text a field shows for a number.
pub fn fmt_f32(v: f32) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" { "0".into() } else { s.to_string() }
}

pub fn parse_f32(text: &str) -> Option<f32> {
    text.trim().parse::<f32>().ok().filter(|v| v.is_finite())
}

// ---- hosts ---------------------------------------------------------------

/// What committed fields and picked references do in an editor.
pub trait EditorHost: Sized + 'static {
    fn widgets(&mut self) -> &mut Widgets;
    /// A text field was committed (Enter or blur) with changed text.
    fn commit_field(&mut self, key: &str, text: String, window: &mut Window, cx: &mut Context<Self>);
    /// A reference was chosen (`None` clears it).
    fn pick_ref(&mut self, key: &str, id: Option<String>, window: &mut Window, cx: &mut Context<Self>);
}

#[derive(Default)]
pub struct Widgets {
    fields: HashMap<String, Entity<InputState>>,
    shown: HashMap<String, String>,
    refs: HashMap<String, Entity<RefList>>,
    _subscriptions: Vec<Subscription>,
}

impl Widgets {
    /// Whether `text` differs from what the field last showed, recording it.
    pub fn changed(&mut self, key: &str, text: &str) -> bool {
        match self.shown.get(key) {
            Some(shown) if shown == text => false,
            _ => {
                self.shown.insert(key.to_string(), text.to_string());
                true
            }
        }
    }

    /// The text input `key`, showing `current` unless it is being typed in.
    pub fn input<V: EditorHost>(&mut self, key: &str, current: &str, window: &mut Window, cx: &mut Context<V>) -> Entity<InputState> {
        if let Some(input) = self.fields.get(key).cloned() {
            let focused = input.read(cx).focus_handle(cx).contains_focused(window, cx);
            if !focused && input.read(cx).value().as_ref() != current {
                input.update(cx, |i, cx| i.set_value(current.to_string(), window, cx));
                self.shown.insert(key.to_string(), current.to_string());
            } else if !focused {
                self.shown.insert(key.to_string(), current.to_string());
            }
            return input;
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current.to_string()));
        self.shown.insert(key.to_string(), current.to_string());
        let owned = key.to_string();
        let sub = cx.subscribe_in(&input, window, move |this: &mut V, input, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                let text = input.read(cx).value().to_string();
                if this.widgets().changed(&owned, &text) {
                    this.commit_field(&owned, text, window, cx);
                }
            }
        });
        self._subscriptions.push(sub);
        self.fields.insert(key.to_string(), input.clone());
        input
    }

    /// A labelled single-line field.
    pub fn text_field<V: EditorHost>(&mut self, key: &str, label: &str, current: &str, window: &mut Window, cx: &mut Context<V>) -> AnyElement {
        let input = self.input(key, current, window, cx);
        labelled(label, div().w(px(150.0)).child(TextInput::new(&input).small()), cx)
    }

    /// A reference picker for `key`: shows the stored reference's status and
    /// opens a list of compatible assets of `kind`.
    pub fn ref_field<V: EditorHost>(
        &mut self,
        key: &str,
        label: &str,
        kind: &'static str,
        skeleton: Option<String>,
        current: Option<String>,
        status: &RefStatus,
        root: &Path,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> AnyElement {
        let list = match self.refs.get(key).cloned() {
            Some(list) => list,
            None => {
                let list = cx.new(|cx| RefList::new(root.to_path_buf(), kind, cx));
                let owned = key.to_string();
                let sub = cx.subscribe_in(&list, window, move |this: &mut V, _list, event: &RefPicked, window, cx| {
                    this.pick_ref(&owned, event.0.clone(), window, cx);
                });
                self._subscriptions.push(sub);
                self.refs.insert(key.to_string(), list.clone());
                list
            }
        };
        list.update(cx, |l, _| {
            l.skeleton = skeleton;
            l.current = current.clone();
        });
        let theme = cx.theme().clone();
        let (text, color) = match status {
            RefStatus::Unset => ("None".to_string(), theme.muted_foreground),
            RefStatus::Ok { name } => (name.clone(), theme.foreground),
            RefStatus::Missing => (format!("Missing {}", current.as_deref().map(short_id).unwrap_or("")), theme.danger),
            RefStatus::Ambiguous { count } => (format!("Ambiguous ({count} files)"), theme.danger),
            RefStatus::Incompatible { name, .. } => (format!("{name} (other skeleton)"), theme.warning),
        };
        let refresh = list.clone();
        let trigger = Button::new(SharedString::from(format!("ref-{key}"))).label(text).small().outline().dropdown_caret(true).text_color(color).on_click(move |_, _, cx| {
            refresh.update(cx, |l, _| l.refresh());
        });
        let popover = Popover::<RefList>::new(SharedString::from(format!("refpop-{key}")))
            .anchor(Corner::BottomLeft)
            .trigger(trigger)
            .content(move |_, _| list.clone());
        let mut row = h_flex().gap_1().items_center().child(popover);
        if let Some(message) = status.message() {
            row = row.child(div().text_xs().text_color(theme.warning).child(message));
        }
        labelled(label, row, cx)
    }
}

pub fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// A caption on the left and a control on the right.
pub fn labelled(label: &str, control: impl IntoElement, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .justify_between()
        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(label.to_string()))
        .child(control)
        .into_any_element()
}

/// Reference picked: `Some(id)` or `None` to clear.
pub struct RefPicked(pub Option<String>);

/// The list a reference field opens.
pub struct RefList {
    root: PathBuf,
    kind: &'static str,
    pub skeleton: Option<String>,
    pub current: Option<String>,
    entries: Vec<PickerEntry>,
    focus: Option<FocusHandle>,
}

impl RefList {
    fn new(root: PathBuf, kind: &'static str, cx: &mut Context<Self>) -> Self {
        Self { root, kind, skeleton: None, current: None, entries: Vec::new(), focus: Some(cx.focus_handle()) }
    }

    /// Read the project's assets of this kind, narrowed to the owner's skeleton.
    pub fn refresh(&mut self) {
        self.entries = refs::picker_entries(&ProjectRefIndex::new(&self.root), self.kind, self.skeleton.as_deref());
    }
}

impl Focusable for RefList {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone().expect("set on creation")
    }
}

impl EventEmitter<RefPicked> for RefList {}
impl EventEmitter<DismissEvent> for RefList {}

impl Render for RefList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut list = v_flex().w(px(300.0)).max_h(px(320.0)).overflow_y_scroll().p_1().gap_0p5().bg(theme.background).border_1().border_color(theme.border).rounded(px(6.0));
        list = list.child(
            Button::new("ref-clear").label("None (clear)").ghost().small().on_click(cx.listener(|_, _, _, cx| {
                cx.emit(RefPicked(None));
                cx.emit(DismissEvent);
            })),
        );
        if self.entries.is_empty() {
            list = list.child(div().p_2().text_xs().text_color(theme.muted_foreground).child("No assets of this kind in the project."));
        }
        for (i, entry) in self.entries.iter().enumerate() {
            let id = entry.candidate.id.clone();
            let incompatible = matches!(entry.compat, Compat::Incompatible { .. });
            let selected = self.current.as_deref() == Some(id.as_str());
            let label = refs::entry_label(entry);
            list = list.child(
                Button::new(SharedString::from(format!("ref-entry-{i}")))
                    .label(label)
                    .ghost()
                    .small()
                    .selected(selected)
                    .disabled(incompatible)
                    .tooltip(if incompatible { "This asset is for a different skeleton".to_string() } else { entry.candidate.path.clone() })
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(RefPicked(Some(id.clone())));
                        cx.emit(DismissEvent);
                    })),
            );
        }
        list
    }
}

// ---- auxiliary panel -----------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuxTab {
    Problems,
    UsedBy,
    Dependencies,
}

/// Derived data under the editor: problems, referrers and the dependency tree.
pub struct Aux {
    pub tab: AuxTab,
    pub diagnostics: Vec<Diagnostic>,
    pub used_by: Vec<Referrer>,
    pub deps: Vec<DepNode>,
}

impl Aux {
    pub fn new() -> Self {
        Self { tab: AuxTab::Problems, diagnostics: Vec::new(), used_by: Vec::new(), deps: Vec::new() }
    }

    /// Recompute from the project index and the document.
    pub fn refresh(&mut self, root: &Path, kind: &str, id: &str, bytes: &[u8], model: Vec<Diagnostic>) {
        let mut diagnostics = model;
        if let Some(source) = ProjectAssetSource::open(root) {
            for d in asset_report(&source, kind, bytes) {
                // closure problems are the project's; the model already reports its own
                if !diagnostics.iter().any(|m| m.code == d.code && m.location == d.location && m.message == d.message) {
                    diagnostics.push(d);
                }
            }
        }
        diagnostics.sort_by_key(|d| match d.severity {
            Severity::Error => 0,
            Severity::Warning => 1,
            Severity::Info => 2,
        });
        self.diagnostics = diagnostics;
        let index = ProjectRefIndex::new(root);
        self.used_by = index.used_by(kind, id);
        self.deps = refs::dependency_tree(&index, kind, id, 5);
    }
}

fn open_asset(path: PathBuf, window: &mut Window, cx: &mut App) {
    window.dispatch_action(Box::new(OpenAsset { path }), cx);
}

fn kind_label(kind: &str) -> &str {
    match kind {
        "animseq" => "Sequence",
        "skeleton" => "Skeleton",
        "skmesh" => "Skeletal Mesh",
        "blendspace" => "Blend Space",
        "anim_montage" => "Montage",
        "anim_graph" => "Animation Graph",
        "anim_retarget" => "Retarget Profile",
        "ik_rig" => "IK Rig",
        "ik_retarget" => "IK Retargeter",
        "pose_db" => "Pose Database",
        other => other,
    }
}

fn dep_rows(nodes: &[DepNode], depth: usize, index: &ProjectRefIndex, theme: &ui::Theme, out: &mut Vec<AnyElement>, counter: &mut usize) {
    for node in nodes {
        *counter += 1;
        let (note, color, path) = match &node.state {
            DepState::Resolved { path } => (String::new(), theme.foreground, Some(path.clone())),
            DepState::Missing => ("missing".to_string(), theme.danger, None),
            DepState::Ambiguous { count } => (format!("{count} files"), theme.danger, None),
            DepState::Cycle => ("cycle".to_string(), theme.warning, None),
            DepState::Truncated => ("\u{2026}".to_string(), theme.muted_foreground, None),
        };
        let name = path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("{} {}", kind_label(&node.kind), short_id(&node.id)));
        let target = path.clone();
        let _ = index;
        out.push(
            h_flex()
                .id(SharedString::from(format!("dep-{counter}")))
                .pl(px(8.0 + depth as f32 * 14.0))
                .py_0p5()
                .gap_2()
                .items_center()
                .text_xs()
                .text_color(color)
                .when(target.is_some(), |row| row.cursor_pointer().hover(|s| s.bg(theme.secondary)))
                .child(name)
                .child(div().text_color(theme.muted_foreground).child(kind_label(&node.kind).to_string()))
                .child(div().text_color(theme.muted_foreground).child(node.via.clone()))
                .when(!note.is_empty(), |row| row.child(Tag::warning().small().child(note)))
                .when_some(target, |row, path| row.on_click(move |_, window, cx| open_asset(path.clone(), window, cx)))
                .into_any_element(),
        );
        dep_rows(&node.children, depth + 1, index, theme, out, counter);
    }
}

/// The problems / used-by / dependencies panel.
pub fn aux_panel<V: EditorBody>(this: &mut V, cx: &mut Context<V>) -> AnyElement {
    let theme = cx.theme().clone();
    let chrome = this.chrome();
    let tab = chrome.aux.tab;
    let root = chrome.root.clone();
    let counts = (chrome.aux.diagnostics.len(), chrome.aux.used_by.len(), chrome.aux.deps.iter().map(|d| d.flatten().len()).sum::<usize>());
    let tab_button = |label: String, which: AuxTab, cx: &mut Context<V>| {
        Button::new(SharedString::from(format!("aux-{which:?}")))
            .label(label)
            .ghost()
            .xsmall()
            .selected(tab == which)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.chrome().aux.tab = which;
                cx.notify();
            }))
    };
    let header = h_flex()
        .w_full()
        .flex_shrink_0()
        .px_2()
        .h(px(28.0))
        .gap_1()
        .items_center()
        .bg(theme.secondary)
        .border_b_1()
        .border_color(theme.border)
        .child(tab_button(format!("Problems ({})", counts.0), AuxTab::Problems, cx))
        .child(tab_button(format!("Used By ({})", counts.1), AuxTab::UsedBy, cx))
        .child(tab_button(format!("Dependencies ({})", counts.2), AuxTab::Dependencies, cx));
    let mut body = v_flex().w_full().flex_1().min_h_0().overflow_y_scroll().p_1().gap_0p5();
    match tab {
        AuxTab::Problems => {
            let chrome = this.chrome();
            if chrome.aux.diagnostics.is_empty() {
                body = body.child(div().p_2().text_xs().text_color(theme.success).child("No problems found."));
            }
            for (i, d) in chrome.aux.diagnostics.iter().enumerate() {
                let color = match d.severity {
                    Severity::Error => theme.danger,
                    Severity::Warning => theme.warning,
                    Severity::Info => theme.muted_foreground,
                };
                let icon = match d.severity {
                    Severity::Error => IconName::XmarkCircle,
                    Severity::Warning => IconName::TriangleAlert,
                    Severity::Info => IconName::Info,
                };
                let location = d.location.clone();
                body = body.child(
                    h_flex()
                        .id(SharedString::from(format!("diag-{i}")))
                        .w_full()
                        .px_2()
                        .py_0p5()
                        .gap_2()
                        .items_start()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.secondary))
                        .child(Icon::new(icon).size(px(13.0)).text_color(color))
                        .child(
                            v_flex()
                                .flex_1()
                                .child(div().text_xs().text_color(theme.foreground).child(d.message.clone()))
                                .when_some(d.repair.clone(), |c, r| c.child(div().text_xs().text_color(theme.muted_foreground).child(format!("Fix: {r}")))),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(at) = &location {
                                this.select_location(at, window, cx);
                            }
                        })),
                );
            }
        }
        AuxTab::UsedBy => {
            let chrome = this.chrome();
            if chrome.aux.used_by.is_empty() {
                body = body.child(div().p_2().text_xs().text_color(theme.muted_foreground).child("Nothing in the project references this asset."));
            }
            for (i, r) in chrome.aux.used_by.iter().enumerate() {
                let name = r.path.strip_prefix(&root).unwrap_or(&r.path).display().to_string();
                let path = r.path.clone();
                body = body.child(
                    h_flex()
                        .id(SharedString::from(format!("used-{i}")))
                        .w_full()
                        .px_2()
                        .py_0p5()
                        .gap_2()
                        .text_xs()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.secondary))
                        .child(div().text_color(theme.foreground).child(name))
                        .child(div().text_color(theme.muted_foreground).child(kind_label(&r.kind).to_string()))
                        .child(div().text_color(theme.muted_foreground).child(r.via.clone()))
                        .when(r.hint, |row| row.child(Tag::secondary().small().child("hint")))
                        .on_click(move |_, window, cx| open_asset(path.clone(), window, cx)),
                );
            }
        }
        AuxTab::Dependencies => {
            let index = ProjectRefIndex::new(&root);
            let mut rows = Vec::new();
            let mut counter = 0;
            dep_rows(&this.chrome().aux.deps, 0, &index, &theme, &mut rows, &mut counter);
            if rows.is_empty() {
                body = body.child(div().p_2().text_xs().text_color(theme.muted_foreground).child("This asset references nothing."));
            }
            body = body.children(rows);
        }
    }
    v_flex().w_full().h(px(150.0)).flex_shrink_0().border_t_1().border_color(theme.border).child(header).child(body).into_any_element()
}

// ---- frame -----------------------------------------------------------------

/// State every editor view carries.
pub struct Chrome {
    pub root: PathBuf,
    pub focus: FocusHandle,
    pub aux: Aux,
    pub status: Option<String>,
    /// Another tab or tool changed the file since this document read it.
    pub external_change: bool,
    /// The generation at which `aux` was last computed.
    pub aux_generation: Option<u64>,
}

impl Chrome {
    pub fn new(path: &Path, cx: &mut App) -> Self {
        Self {
            root: crate::core::library::project_root(path),
            focus: cx.focus_handle(),
            aux: Aux::new(),
            status: None,
            external_change: false,
            aux_generation: None,
        }
    }
}

/// What an editor view provides to [`frame`].
pub trait EditorBody: EditorHost {
    type Model: EditorModel;
    fn model(&self) -> &Self::Model;
    fn model_mut(&mut self) -> &mut Self::Model;
    fn chrome(&mut self) -> &mut Chrome;
    fn chrome_ref(&self) -> &Chrome;
    fn icon(&self) -> IconName;
    fn kind_title(&self) -> &'static str;
    fn left_title(&self) -> &'static str;
    fn left(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement;
    fn center(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement;
    fn inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement;
    /// Extra toolbar content right of the document controls.
    fn toolbar_extra(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> Vec<AnyElement> {
        Vec::new()
    }
    /// Select what a diagnostic's location names (a sample, segment, chain...).
    fn select_location(&mut self, _location: &str, _window: &mut Window, _cx: &mut Context<Self>) {}
    /// The model changed; refresh anything cached from it.
    fn after_change(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}
}

/// Run `edit`, then refresh derived data. Every edit in a view goes through this.
pub fn edit<V: EditorBody>(this: &mut V, window: &mut Window, cx: &mut Context<V>, edit: impl FnOnce(&mut V::Model) -> Result<(), String>) {
    match edit(this.model_mut()) {
        Ok(()) => this.chrome().status = None,
        Err(message) => this.chrome().status = Some(message),
    }
    this.after_change(window, cx);
    cx.notify();
}

pub fn save<V: EditorBody>(this: &mut V, window: &mut Window, cx: &mut Context<V>) {
    match this.model_mut().save() {
        Ok(SaveOutcome::Saved) => {
            this.chrome().status = None;
            this.chrome().external_change = false;
            let (root, path) = (this.chrome().root.clone(), this.model().path().to_path_buf());
            asset_index::record_asset(&root, &path);
            this.chrome().aux_generation = None;
        }
        Ok(SaveOutcome::Conflict { .. }) => {
            this.chrome().external_change = true;
            this.chrome().status = Some("The file changed on disk since it was opened. Keep your version or reload theirs.".into());
        }
        Err(error) => this.chrome().status = Some(format!("Could not save: {error}")),
    }
    this.after_change(window, cx);
    cx.notify();
}

pub fn overwrite<V: EditorBody>(this: &mut V, window: &mut Window, cx: &mut Context<V>) {
    match this.model_mut().save_overwrite() {
        Ok(()) => {
            this.chrome().status = None;
            this.chrome().external_change = false;
            let (root, path) = (this.chrome().root.clone(), this.model().path().to_path_buf());
            asset_index::record_asset(&root, &path);
            this.chrome().aux_generation = None;
        }
        Err(error) => this.chrome().status = Some(format!("Could not save: {error}")),
    }
    this.after_change(window, cx);
    cx.notify();
}

pub fn reload<V: EditorBody>(this: &mut V, window: &mut Window, cx: &mut Context<V>) {
    match this.model_mut().reload() {
        Ok(()) => {
            this.chrome().status = None;
            this.chrome().external_change = false;
            this.chrome().aux_generation = None;
        }
        Err(error) => this.chrome().status = Some(format!("Could not reload: {error}")),
    }
    this.after_change(window, cx);
    cx.notify();
}

pub fn undo<V: EditorBody>(this: &mut V, window: &mut Window, cx: &mut Context<V>) {
    if this.model_mut().undo() {
        this.after_change(window, cx);
        cx.notify();
    }
}

pub fn redo<V: EditorBody>(this: &mut V, window: &mut Window, cx: &mut Context<V>) {
    if this.model_mut().redo() {
        this.after_change(window, cx);
        cx.notify();
    }
}

/// A titled group of inspector rows.
pub fn section(title: &str, rows: Vec<AnyElement>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .w_full()
        .gap_1()
        .pb_2()
        .child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child(title.to_uppercase()))
        .children(rows)
        .into_any_element()
}

/// A read-only value row.
pub fn value_row(label: &str, value: impl Into<SharedString>, cx: &App) -> AnyElement {
    labelled(label, div().text_xs().text_color(cx.theme().foreground).child(value.into()), cx)
}

pub fn empty_inspector(selection: &AnimSelection, hint: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .p_3()
        .gap_1()
        .child(div().text_sm().font_semibold().text_color(theme.foreground).child(selection.title()))
        .child(div().text_xs().text_color(theme.muted_foreground).child(hint.to_string()))
        .into_any_element()
}

/// Lays out the toolbar, the three columns and the auxiliary panel.
pub fn frame<V: EditorBody>(this: &mut V, window: &mut Window, cx: &mut Context<V>) -> impl IntoElement {
    // derived data follows the document
    let generation = this.model().generation();
    if this.chrome().aux_generation != Some(generation) {
        let (root, kind, id, bytes, diags) = (this.chrome().root.clone(), this.model().kind(), this.model().asset_id(), this.model().asset_text(), this.model().validation());
        this.chrome().aux.refresh(&root, kind, &id, &bytes, diags);
        this.chrome().aux_generation = Some(generation);
        let external = this.model().changed_on_disk();
        this.chrome().external_change |= external;
    }
    let theme = cx.theme().clone();
    let dirty = this.model().is_dirty();
    let (can_undo, can_redo) = (this.model().can_undo(), this.model().can_redo());
    let undo_tip = this.model().undo_label().filter(|l| !l.is_empty()).map_or("Undo (Ctrl+Z)".to_string(), |l| format!("Undo {l} (Ctrl+Z)"));
    let redo_tip = this.model().redo_label().filter(|l| !l.is_empty()).map_or("Redo (Ctrl+Shift+Z)".to_string(), |l| format!("Redo {l} (Ctrl+Shift+Z)"));
    let name = this.model().asset_name();
    let crumbs = refs::breadcrumb(&this.chrome_ref().root, this.model().path());
    let errors = this.chrome().aux.diagnostics.iter().filter(|d| d.severity == Severity::Error).count();
    let warnings = this.chrome().aux.diagnostics.iter().filter(|d| d.severity == Severity::Warning).count();
    let status = this.chrome().status.clone();
    let external = this.chrome().external_change;
    let icon = this.icon();
    let kind_title = this.kind_title();
    let left_title = this.left_title();
    let extra = this.toolbar_extra(window, cx);
    let left = this.left(window, cx);
    let center = this.center(window, cx);
    let inspector = this.inspector(window, cx);
    let aux = aux_panel(this, cx);
    let focus = this.chrome().focus.clone();

    let toolbar = h_flex()
        .w_full()
        .flex_shrink_0()
        .h(px(40.0))
        .px_2()
        .gap_2()
        .items_center()
        .bg(theme.secondary)
        .border_b_1()
        .border_color(theme.border)
        .child(
            Button::new("anim-save")
                .icon(IconName::FloppyDisk)
                .label("Save")
                .small()
                .when(dirty, |b| b.primary())
                .when(!dirty, |b| b.ghost())
                .tooltip("Save (Ctrl+S)")
                .on_click(cx.listener(|this, _, window, cx| save(this, window, cx))),
        )
        .child(
            Button::new("anim-reload")
                .icon(IconName::Refresh)
                .ghost()
                .small()
                .tooltip("Discard unsaved changes and reload the file")
                .on_click(cx.listener(|this, _, window, cx| reload(this, window, cx))),
        )
        .child(
            Button::new("anim-undo")
                .icon(IconName::Undo)
                .ghost()
                .small()
                .disabled(!can_undo)
                .tooltip(undo_tip)
                .on_click(cx.listener(|this, _, window, cx| undo(this, window, cx))),
        )
        .child(
            Button::new("anim-redo")
                .icon(IconName::Redo)
                .ghost()
                .small()
                .disabled(!can_redo)
                .tooltip(redo_tip)
                .on_click(cx.listener(|this, _, window, cx| redo(this, window, cx))),
        )
        .child(Icon::new(icon).size(px(15.0)).text_color(theme.info))
        .child(div().text_sm().font_semibold().text_color(theme.foreground).child(name))
        .child(Tag::secondary().small().child(kind_title))
        .children(extra)
        .child(div().flex_1())
        .when(errors > 0, |t| t.child(Tag::danger().small().child(format!("{errors} errors"))))
        .when(warnings > 0, |t| t.child(Tag::warning().small().child(format!("{warnings} warnings"))))
        .child(if dirty { Tag::warning().small().child("Unsaved changes") } else { Tag::success().small().child("Saved") });

    let breadcrumb = h_flex()
        .w_full()
        .flex_shrink_0()
        .px_3()
        .h(px(22.0))
        .gap_1()
        .items_center()
        .border_b_1()
        .border_color(theme.border)
        .text_xs()
        .text_color(theme.muted_foreground)
        .children(crumbs.into_iter().enumerate().flat_map(|(i, c)| {
            let sep = (i > 0).then(|| div().child("/").into_any_element());
            sep.into_iter().chain(std::iter::once(div().child(c).into_any_element()))
        }));

    let banner = |text: String, color: Hsla, actions: Vec<Button>| {
        h_flex()
            .w_full()
            .flex_shrink_0()
            .px_3()
            .py_1()
            .gap_2()
            .items_center()
            .bg(color.opacity(0.10))
            .border_b_1()
            .border_color(color.opacity(0.35))
            .child(div().flex_1().text_xs().text_color(theme.foreground).child(text))
            .children(actions)
    };

    v_flex()
        .id("anim-editor")
        .key_context("AnimEditor")
        .track_focus(&focus)
        .size_full()
        .bg(theme.background)
        .on_action(cx.listener(|this, _: &Undo, window, cx| undo(this, window, cx)))
        .on_action(cx.listener(|this, _: &Redo, window, cx| redo(this, window, cx)))
        .child(toolbar)
        .child(breadcrumb)
        .when(external, |c| {
            c.child(banner(
                "This file changed on disk (another tab or tool saved it).".into(),
                theme.warning,
                vec![
                    Button::new("anim-keep-mine").label("Keep my version").small().on_click(cx.listener(|this, _, window, cx| overwrite(this, window, cx))),
                    Button::new("anim-use-theirs").label("Reload theirs").small().on_click(cx.listener(|this, _, window, cx| reload(this, window, cx))),
                ],
            ))
        })
        .when_some(status, |c, message| c.child(banner(message, theme.danger, Vec::new())))
        .child(
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
                    v_flex()
                        .w(px(230.0))
                        .flex_shrink_0()
                        .h_full()
                        .border_r_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .px_3()
                                .h(px(28.0))
                                .flex()
                                .items_center()
                                .flex_shrink_0()
                                .bg(theme.secondary)
                                .border_b_1()
                                .border_color(theme.border)
                                .text_xs()
                                .font_semibold()
                                .text_color(theme.muted_foreground)
                                .child(left_title.to_uppercase()),
                        )
                        .child(div().flex_1().min_h_0().overflow_y_scroll().child(left)),
                )
                .child(div().flex_1().min_w_0().h_full().child(center))
                .child(
                    v_flex()
                        .w(px(300.0))
                        .flex_shrink_0()
                        .h_full()
                        .border_l_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .px_3()
                                .h(px(28.0))
                                .flex()
                                .items_center()
                                .flex_shrink_0()
                                .bg(theme.secondary)
                                .border_b_1()
                                .border_color(theme.border)
                                .text_xs()
                                .font_semibold()
                                .text_color(theme.muted_foreground)
                                .child("PROPERTIES"),
                        )
                        .child(div().flex_1().min_h_0().overflow_y_scroll().p_3().child(inspector)),
                ),
        )
        .child(aux)
}

/// Implements `Panel`, focus and the panel event for an editor view.
#[macro_export]
macro_rules! anim_panel {
    ($view:ty, $id:expr) => {
        impl gpui::EventEmitter<ui::dock::PanelEvent> for $view {}
        impl gpui::Focusable for $view {
            fn focus_handle(&self, _cx: &gpui::App) -> gpui::FocusHandle {
                $crate::anim_editors::views::chrome::EditorBody::chrome_ref(self).focus.clone()
            }
        }
        impl ui::dock::Panel for $view {
            fn panel_name(&self) -> &'static str {
                $id
            }
            fn tab_name(&self, _cx: &gpui::App) -> Option<gpui::SharedString> {
                let model = $crate::anim_editors::views::chrome::EditorBody::model(self);
                $crate::anim_editors::views::chrome::EditorModel::path(model).file_name().map(|n| n.to_string_lossy().to_string().into())
            }
            fn tab_icon(&self, _cx: &gpui::App) -> Option<ui::IconName> {
                Some($crate::anim_editors::views::chrome::EditorBody::icon(self))
            }
            fn tab_unsaved(&self, _cx: &gpui::App) -> bool {
                $crate::anim_editors::views::chrome::EditorModel::is_dirty($crate::anim_editors::views::chrome::EditorBody::model(self))
            }
            fn panel_file_path(&self, _cx: &gpui::App) -> Option<std::path::PathBuf> {
                Some($crate::anim_editors::views::chrome::EditorModel::path($crate::anim_editors::views::chrome::EditorBody::model(self)).to_path_buf())
            }
            fn title(&self, _window: &gpui::Window, _cx: &gpui::App) -> gpui::AnyElement {
                let model = $crate::anim_editors::views::chrome::EditorBody::model(self);
                $crate::anim_editors::views::chrome::EditorModel::path(model)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
                    .into_any_element()
            }
        }
    };
}

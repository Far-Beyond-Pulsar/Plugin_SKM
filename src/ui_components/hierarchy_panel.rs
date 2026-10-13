//! Left dock panel: the skeleton's bone tree.
//!
//! Rows are laid out to match the engine's other trees (same metrics and
//! colors as `ui::hierarchical_tree`), with the extras a rig needs: a filter
//! box, expand/collapse all, tree guide lines, animated-bone badges, keyboard
//! navigation, and hover that lights up the bone in the 3D viewport.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use gpui::prelude::FluentBuilder;
use gpui::*;
use ui::button::{Button, ButtonVariants};
use ui::dock::PanelEvent;
use ui::hierarchical_tree::tree_colors;
use ui::input::{InputEvent, InputState, TextInput};
use ui::scroll::ScrollbarAxis;
use ui::status_bar::StatusBar;
use ui::tag::Tag;
use ui::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt};

use crate::core::Skeleton;
use crate::editor::SkeletalAnimEditorPanel;
use crate::ui_components::chrome::{empty_message, panel_header, MONO};

const ROW_HEIGHT: f32 = 28.0;
const INDENT: f32 = 14.0;

/// One visible row of the tree.
#[derive(Clone)]
struct Entry {
    id: String,
    name: String,
    depth: usize,
    has_children: bool,
    expanded: bool,
    selected: bool,
    hovered: bool,
    /// Keyframe count if the bone has an animation track.
    keys: Option<usize>,
    keyed_here: bool,
    is_root: bool,
    /// Matches the filter text itself (not merely an ancestor of a match).
    direct_match: bool,
}

/// Everything the tree displays, copied out of the editor.
///
/// The panel renders from this instead of reading the editor entity: a view
/// that reads an entity while rendering is rebuilt on every notify of it, and
/// the editor notifies on every playback frame. The snapshot is refreshed only
/// when [`Model::signature`] says something shown here actually changed.
#[derive(Default)]
struct Model {
    skeleton: Skeleton,
    expanded: HashSet<String>,
    selected: Option<String>,
    hovered: Option<String>,
    /// Keyframe count per animated bone.
    keys: HashMap<String, usize>,
    /// Bones with a key exactly at the playhead.
    keyed_here: HashSet<String>,
    signature: u64,
}

impl Model {
    fn capture(e: &SkeletalAnimEditorPanel) -> Self {
        Self {
            skeleton: e.skeleton.clone(),
            expanded: e.expanded_bones.clone(),
            selected: e.selected_bone.clone(),
            hovered: e.hovered_bone.clone(),
            keys: e
                .animation
                .tracks
                .iter()
                .map(|t| (t.bone_id.clone(), t.keyframes.len()))
                .collect(),
            keyed_here: e
                .animation
                .tracks
                .iter()
                .filter(|t| e.keyframe_at_playhead(&t.bone_id).is_some())
                .map(|t| t.bone_id.clone())
                .collect(),
            signature: Self::signature(e),
        }
    }

    /// Cheap hash of the editor state the tree shows.
    fn signature(e: &SkeletalAnimEditorPanel) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        e.selected_bone.hash(&mut h);
        e.hovered_bone.hash(&mut h);
        e.skeleton.bones.len().hash(&mut h);
        // Sets hash order-independently.
        let mut expanded = 0u64;
        for id in &e.expanded_bones {
            let mut item = std::collections::hash_map::DefaultHasher::new();
            id.hash(&mut item);
            expanded ^= item.finish();
        }
        expanded.hash(&mut h);
        for t in &e.animation.tracks {
            t.bone_id.hash(&mut h);
            t.keyframes.len().hash(&mut h);
            e.keyframe_at_playhead(&t.bone_id).is_some().hash(&mut h);
        }
        h.finish()
    }
}

pub struct BoneHierarchyPanel {
    editor: WeakEntity<SkeletalAnimEditorPanel>,
    focus_handle: FocusHandle,
    filter: Option<Entity<InputState>>,
    model: Model,
    _subscription: Option<Subscription>,
    _editor_observer: Option<Subscription>,
}

impl BoneHierarchyPanel {
    pub fn new(editor: WeakEntity<SkeletalAnimEditorPanel>, cx: &mut Context<Self>) -> Self {
        Self {
            editor,
            focus_handle: cx.focus_handle(),
            filter: None,
            model: Model::default(),
            _subscription: None,
            _editor_observer: None,
        }
    }

    /// Start following the editor. Call once, outside of render.
    pub fn attach(
        &mut self,
        state: &SkeletalAnimEditorPanel,
        editor: &Entity<SkeletalAnimEditorPanel>,
        cx: &mut Context<Self>,
    ) {
        self.model = Model::capture(state);
        self._editor_observer = Some(cx.observe(editor, |this, editor, cx| {
            let e = editor.read(cx);
            if Model::signature(e) != this.model.signature {
                this.model = Model::capture(e);
                cx.notify();
            }
        }));
    }

    fn init_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter bones…"));
        self._subscription =
            Some(
                cx.subscribe_in(&input, window, |_this, _input, event: &InputEvent, _w, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                }),
            );
        self.filter = Some(input);
    }

    fn filter_text(&self, cx: &App) -> String {
        self.filter
            .as_ref()
            .map(|f| f.read(cx).value().trim().to_lowercase())
            .unwrap_or_default()
    }

    /// The rows currently visible, honoring expansion and the filter.
    fn entries(&self, filter: &str) -> Vec<Entry> {
        let model = &self.model;
        let skeleton = &model.skeleton;
        let filtering = !filter.is_empty();

        // With a filter active, show matches plus the ancestors leading to them.
        let mut shown: HashSet<&str> = HashSet::new();
        let mut direct: HashSet<&str> = HashSet::new();
        if filtering {
            for bone in &skeleton.bones {
                if bone.name.to_lowercase().contains(filter) || bone.id.to_lowercase().contains(filter) {
                    direct.insert(bone.id.as_str());
                    let mut current = Some(bone.id.as_str());
                    while let Some(id) = current {
                        if !shown.insert(id) {
                            break;
                        }
                        current = skeleton.bone(id).and_then(|b| b.parent.as_deref());
                    }
                }
            }
        }

        let mut out = Vec::new();
        let mut skip_below: Option<usize> = None;
        for (bone, depth) in skeleton.depth_first() {
            if filtering {
                if !shown.contains(bone.id.as_str()) {
                    continue;
                }
            } else {
                if let Some(skip) = skip_below {
                    if depth > skip {
                        continue;
                    }
                    skip_below = None;
                }
            }

            let has_children = !skeleton.children_of(&bone.id).is_empty();
            let expanded = has_children && (filtering || model.expanded.contains(&bone.id));
            if !filtering && has_children && !expanded {
                skip_below = Some(depth);
            }

            out.push(Entry {
                id: bone.id.clone(),
                name: bone.name.clone(),
                depth,
                has_children,
                expanded,
                selected: model.selected.as_deref() == Some(bone.id.as_str()),
                hovered: model.hovered.as_deref() == Some(bone.id.as_str()),
                keys: model.keys.get(&bone.id).copied(),
                keyed_here: model.keyed_here.contains(&bone.id),
                is_root: bone.parent.is_none(),
                direct_match: !filtering || direct.contains(bone.id.as_str()),
            });
        }
        out
    }

    fn render_row(&self, entry: Entry, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let Entry {
            id,
            name,
            depth,
            has_children,
            expanded,
            selected,
            hovered,
            keys,
            keyed_here,
            is_root,
            direct_match,
        } = entry;

        let icon_color = if is_root {
            tree_colors::SPECIAL_YELLOW
        } else if has_children {
            tree_colors::CODE_PURPLE
        } else {
            tree_colors::CODE_BLUE
        };

        let editor_select = self.editor.clone();
        let editor_toggle = self.editor.clone();
        let editor_hover = self.editor.clone();
        let focus = self.focus_handle.clone();
        let (select_id, toggle_id, hover_id) = (id.clone(), id.clone(), id.clone());

        let guides = (0..depth).map(|d| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(8.0 + d as f32 * INDENT + 8.0))
                .w(px(1.0))
                .bg(theme.border.opacity(0.55))
        });

        h_flex()
            .id(SharedString::from(format!("bone-{id}")))
            .relative()
            .flex_shrink_0()
            .h(px(ROW_HEIGHT))
            .w_full()
            .pl(px(8.0 + depth as f32 * INDENT))
            .pr_2()
            .gap_1()
            .items_center()
            .cursor_pointer()
            .bg(if selected {
                theme.list_active
            } else if hovered {
                theme.info.opacity(0.12)
            } else {
                theme.transparent
            })
            .hover(|s| s.bg(theme.list_hover))
            .when(selected, |row| {
                row.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(3.0))
                        .bg(theme.primary),
                )
            })
            .children(guides)
            .on_hover(move |is_hovered, _w, cx| {
                let _ = editor_hover.update(cx, |e, cx| {
                    let current = e.hovered_bone.as_deref();
                    if *is_hovered && current != Some(hover_id.as_str()) {
                        e.hovered_bone = Some(hover_id.clone());
                        cx.notify();
                    } else if !*is_hovered && current == Some(hover_id.as_str()) {
                        e.hovered_bone = None;
                        cx.notify();
                    }
                });
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                focus.focus(window, cx);
                let id = select_id.clone();
                let _ = editor_select.update(cx, |e, cx| e.select_bone(Some(id), window, cx));
            })
            .child(
                div()
                    .id(SharedString::from(format!("bone-toggle-{toggle_id}")))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(16.0))
                    .flex_shrink_0()
                    .rounded(px(3.0))
                    .when(has_children, |d| {
                        d.hover(|s| s.bg(theme.foreground.opacity(0.10)))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                cx.stop_propagation();
                                let id = toggle_id.clone();
                                let _ = editor_toggle.update(cx, |e, cx| e.toggle_bone_expanded(&id, cx));
                            })
                            .child(
                                Icon::new(if expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .size(px(12.0))
                                .text_color(theme.muted_foreground),
                            )
                    }),
            )
            .child(Icon::new(IconName::Component).size_3p5().text_color(icon_color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(if selected || (has_children && depth == 0) {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(if direct_match {
                        theme.foreground
                    } else {
                        theme.muted_foreground
                    })
                    .child(name),
            )
            .when_some(keys, |row, count| {
                row.child(
                    h_flex()
                        .gap_0p5()
                        .items_center()
                        .child(
                            Icon::new(if keyed_here {
                                IconName::Keyframe
                            } else {
                                IconName::Keyframes
                            })
                            .size(px(11.0))
                            .text_color(if keyed_here {
                                theme.warning
                            } else {
                                theme.info.opacity(0.75)
                            }),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .font_family(MONO)
                                .text_color(theme.muted_foreground)
                                .child(count.to_string()),
                        ),
                )
            })
            .into_any_element()
    }

    /// Up / Down / Left / Right / Home / End navigation.
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let filter = self.filter_text(cx);
        let entries = self.entries(&filter);
        if entries.is_empty() {
            return;
        }
        let current = entries.iter().position(|e| e.selected);
        let select = |id: String, cx: &mut Context<Self>, window: &mut Window| {
            editor.update(cx, |e, cx| e.select_bone(Some(id), window, cx));
        };

        match event.keystroke.key.as_str() {
            "down" => {
                let next = current.map_or(0, |i| (i + 1).min(entries.len() - 1));
                select(entries[next].id.clone(), cx, window);
            }
            "up" => {
                let prev = current.map_or(0, |i| i.saturating_sub(1));
                select(entries[prev].id.clone(), cx, window);
            }
            "home" => select(entries[0].id.clone(), cx, window),
            "end" => select(entries[entries.len() - 1].id.clone(), cx, window),
            "right" => {
                let Some(entry) = current.map(|i| entries[i].clone()) else {
                    return;
                };
                if entry.has_children && !entry.expanded {
                    editor.update(cx, |e, cx| e.toggle_bone_expanded(&entry.id, cx));
                } else if entry.has_children {
                    if let Some(child) = entries.get(current.unwrap() + 1) {
                        select(child.id.clone(), cx, window);
                    }
                }
            }
            "left" => {
                let Some(entry) = current.map(|i| entries[i].clone()) else {
                    return;
                };
                if entry.has_children && entry.expanded {
                    editor.update(cx, |e, cx| e.toggle_bone_expanded(&entry.id, cx));
                } else {
                    let parent = self
                        .model
                        .skeleton
                        .bone(&entry.id)
                        .and_then(|b| b.parent.clone());
                    if let Some(parent) = parent {
                        select(parent, cx, window);
                    }
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl EventEmitter<PanelEvent> for BoneHierarchyPanel {}

impl Focusable for BoneHierarchyPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ui::dock::Panel for BoneHierarchyPanel {
    fn panel_name(&self) -> &'static str {
        "skeletal-bone-hierarchy"
    }

    fn title(&self, _window: &Window, _cx: &App) -> AnyElement {
        "Hierarchy".into_any_element()
    }
}

impl Render for BoneHierarchyPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        if self.filter.is_none() {
            self.init_filter(window, cx);
        }

        let filter = self.filter_text(cx);
        let entries = self.entries(&filter);
        let total = self.model.skeleton.bones.len();
        let animated = self.model.keys.len();
        let shown = entries.len();
        let rows: Vec<AnyElement> = entries
            .into_iter()
            .map(|entry| self.render_row(entry, cx))
            .collect();

        let editor_expand = self.editor.clone();
        let editor_collapse = self.editor.clone();

        let header = panel_header(
            IconName::Component,
            "Skeleton",
            Some(
                Tag::secondary()
                    .small()
                    .rounded_full()
                    .child(total.to_string())
                    .into_any_element(),
            ),
            cx,
        );

        let search_row = h_flex()
            .w_full()
            .flex_shrink_0()
            .px_2()
            .py_1p5()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .when_some(self.filter.as_ref(), |d, input| {
                        d.child(
                            TextInput::new(input).small().cleanable().prefix(
                                Icon::new(IconName::Search)
                                    .size_3p5()
                                    .text_color(theme.muted_foreground),
                            ),
                        )
                    }),
            )
            .child(
                Button::new("hier-expand-all")
                    .icon(IconName::Expand)
                    .ghost()
                    .small()
                    .tooltip("Expand all")
                    .on_click(move |_, _, cx| {
                        let _ = editor_expand.update(cx, |e, cx| e.expand_all_bones(cx));
                    }),
            )
            .child(
                Button::new("hier-collapse-all")
                    .icon(IconName::Collapse)
                    .ghost()
                    .small()
                    .tooltip("Collapse all")
                    .on_click(move |_, _, cx| {
                        let _ = editor_collapse.update(cx, |e, cx| e.collapse_all_bones(cx));
                    }),
            );

        let body = if rows.is_empty() {
            let (title, hint) = if filter.is_empty() {
                ("No bones", "This skeleton has no bones yet")
            } else {
                ("No matches", "Nothing in the hierarchy matches the filter")
            };
            div()
                .flex_1()
                .min_h_0()
                .child(empty_message(IconName::Search, title, hint, cx))
                .into_any_element()
        } else {
            v_flex()
                .id("hierarchy-rows")
                .flex_1()
                .min_h_0()
                .py_1()
                .scrollable(ScrollbarAxis::Vertical)
                .children(rows)
                .into_any_element()
        };

        let footer = StatusBar::new()
            .left(if filter.is_empty() {
                format!("{total} bones")
            } else {
                format!("{shown} of {total} shown")
            })
            .right(div().font_family(MONO).child(format!("{animated} animated")));

        v_flex()
            .id("skeletal-hierarchy")
            .size_full()
            .bg(theme.sidebar)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, w, cx| this.on_key_down(e, w, cx)))
            .child(header)
            .child(search_row)
            .child(body)
            .child(footer)
            .into_any_element()
    }
}

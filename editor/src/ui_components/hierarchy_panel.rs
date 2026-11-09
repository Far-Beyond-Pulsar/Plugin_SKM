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
use ui::context_menu::ContextMenuExt;
use ui::dock::PanelEvent;
use ui::hierarchical_tree::tree_colors;
use ui::input::{InputEvent, InputState, TextInput};
use ui::scroll::ScrollbarAxis;
use ui::status_bar::StatusBar;
use ui::tag::Tag;
use ui::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt};

use crate::core::Skeleton;
use crate::editor::hierarchy_edit::HierarchyCommand;
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
    sockets: skeletal_assets::sockets::Sockets,
    selected_socket: Option<String>,
    expanded: HashSet<String>,
    selected: Option<String>,
    hovered: Option<String>,
    /// Keyframe count per animated bone.
    keys: HashMap<String, usize>,
    /// Bones with a key exactly at the playhead.
    keyed_here: HashSet<String>,
    signature: u64,
    editable: bool,
}

impl Model {
    fn capture(e: &SkeletalAnimEditorPanel) -> Self {
        Self {
            skeleton: e.skeleton.clone(),
            sockets: e.sockets.clone(),
            selected_socket: e.selected_socket.clone(),
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
            editable: e.mode == crate::editor::EditorMode::Skeleton,
        }
    }

    /// Cheap hash of the editor state the tree shows.
    fn signature(e: &SkeletalAnimEditorPanel) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        e.selected_bone.hash(&mut h);
        e.selected_socket.hash(&mut h);
        e.hovered_bone.hash(&mut h);
        e.skeleton.bones.len().hash(&mut h);
        e.hierarchy_revision.hash(&mut h);
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
    tools: Option<Entity<super::hierarchy_tools::HierarchyTools>>,
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
            tools: None,
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
        self._subscription = Some(cx.subscribe_in(
            &input,
            window,
            |_this, _input, event: &InputEvent, _w, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        ));
        self.filter = Some(input);
        if let Some(editor) = self.editor.upgrade() {
            self.tools =
                Some(cx.new(|cx| super::hierarchy_tools::HierarchyTools::new(editor, window, cx)));
        }
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
                if bone.name.to_lowercase().contains(filter)
                    || bone.id.to_lowercase().contains(filter)
                    || model
                        .sockets
                        .iter()
                        .any(|s| s.bone_id == bone.id && s.name.to_lowercase().contains(filter))
                {
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

            let has_children = !skeleton.children_of(&bone.id).is_empty()
                || model.sockets.iter().any(|s| s.bone_id == bone.id);
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
                selected: model.selected_socket.is_none()
                    && model.selected.as_deref() == Some(bone.id.as_str()),
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

        let menu_editor = self.editor.clone();
        let menu_id = id.clone();
        let editable = self.model.editable;
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
            .debug_selector(|| format!("bone-{id}"))
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
            .when(editable, |row| {
                row.context_menu(move |menu, _, _| {
                    let child_editor = menu_editor.clone();
                    let child_id = menu_id.clone();
                    let socket_editor = menu_editor.clone();
                    let socket_id = menu_id.clone();
                    let root_editor = menu_editor.clone();
                    let root_id = menu_id.clone();
                    let delete_editor = menu_editor.clone();
                    let delete_id = menu_id.clone();
                    menu.menu_handler("Add Child", move |window, cx| {
                        let _ = child_editor.update(cx, |e, cx| {
                            e.request_hierarchy_command(
                                HierarchyCommand::AddBone {
                                    parent: Some(child_id.clone()),
                                },
                                window,
                                cx,
                            )
                        });
                    })
                    .menu_handler("Add Socket", move |window, cx| {
                        let _ = socket_editor.update(cx, |e, cx| {
                            e.request_hierarchy_command(
                                HierarchyCommand::AddSocket {
                                    bone: socket_id.clone(),
                                },
                                window,
                                cx,
                            )
                        });
                    })
                    .menu_handler("Move to Root", move |window, cx| {
                        let _ = root_editor.update(cx, |e, cx| {
                            e.request_hierarchy_command(
                                HierarchyCommand::Reparent {
                                    id: root_id.clone(),
                                    parent: None,
                                },
                                window,
                                cx,
                            )
                        });
                    })
                    .separator()
                    .menu_handler(
                        if has_children {
                            "Delete Subtree…"
                        } else {
                            "Delete…"
                        },
                        move |window, cx| {
                            let _ = delete_editor.update(cx, |e, cx| {
                                e.request_hierarchy_command(
                                    HierarchyCommand::DeleteBone {
                                        id: delete_id.clone(),
                                        subtree: has_children,
                                    },
                                    window,
                                    cx,
                                )
                            });
                        },
                    )
                })
            })
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
                                let _ = editor_toggle
                                    .update(cx, |e, cx| e.toggle_bone_expanded(&id, cx));
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
            .child(
                Icon::new(IconName::Component)
                    .size_3p5()
                    .text_color(icon_color),
            )
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
        if !self.focus_handle.is_focused(window) || event.keystroke.modifiers.secondary() {
            return;
        }
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        if self.model.editable && event.keystroke.key == "f2" {
            if let Some(tools) = &self.tools {
                let focus = tools.read(cx).name_focus(cx);
                focus.focus(window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if self.model.editable && event.keystroke.key == "insert" {
            editor.update(cx, |e, cx| {
                e.request_hierarchy_command(
                    crate::editor::hierarchy_edit::HierarchyCommand::AddBone {
                        parent: e.selected_bone.clone(),
                    },
                    window,
                    cx,
                )
            });
            cx.stop_propagation();
            return;
        }
        if self.model.editable
            && (event.keystroke.key == "delete" || event.keystroke.key == "backspace")
        {
            editor.update(cx, |e, cx| {
                if let Some(id) = e.selected_socket.clone() {
                    e.request_hierarchy_command(HierarchyCommand::DeleteSocket(id), window, cx);
                } else if let Some(id) = e.selected_bone.clone() {
                    e.request_hierarchy_command(
                        HierarchyCommand::DeleteBone { id, subtree: false },
                        window,
                        cx,
                    );
                }
            });
            cx.stop_propagation();
            return;
        }
        let filter = self.filter_text(cx);
        let entries = self.entries(&filter);
        if entries.is_empty() {
            return;
        }
        let current = entries.iter().position(|e| {
            e.selected
                || (self.model.selected_socket.is_some()
                    && self.model.selected.as_deref() == Some(e.id.as_str()))
        });
        let select = |id: String, cx: &mut Context<Self>, window: &mut Window| {
            editor.update(cx, |e, cx| e.select_bone(Some(id), window, cx));
        };

        let mut navigation = Vec::new();
        for entry in &entries {
            navigation.push((entry.id.clone(), None));
            for socket in self.model.sockets.iter().filter(|s| {
                s.bone_id == entry.id
                    && (entry.expanded || !filter.is_empty())
                    && (filter.is_empty()
                        || entry.direct_match
                        || s.name.to_lowercase().contains(&filter))
            }) {
                navigation.push((entry.id.clone(), Some(socket.id.clone())));
            }
        }
        let selected = navigation.iter().position(|(bone, socket)| {
            Some(bone) == self.model.selected.as_ref() && socket == &self.model.selected_socket
        });
        match event.keystroke.key.as_str() {
            "down" | "up" | "home" | "end" => {
                let index = match event.keystroke.key.as_str() {
                    "down" => selected.map_or(0, |i| (i + 1).min(navigation.len() - 1)),
                    "up" => selected.map_or(0, |i| i.saturating_sub(1)),
                    "home" => 0,
                    _ => navigation.len() - 1,
                };
                let (bone, socket) = &navigation[index];
                if let Some(socket) = socket {
                    editor.update(cx, |e, cx| e.select_socket(socket.clone(), window, cx));
                } else {
                    select(bone.clone(), cx, window);
                }
            }
            "right" => {
                if self.model.selected_socket.is_some() {
                    return;
                }
                let Some(entry) = current.map(|i| entries[i].clone()) else {
                    return;
                };
                if entry.has_children && !entry.expanded {
                    editor.update(cx, |e, cx| e.toggle_bone_expanded(&entry.id, cx));
                } else if entry.has_children {
                    if let Some(socket) = self.model.sockets.iter().find(|s| s.bone_id == entry.id)
                    {
                        let id = socket.id.clone();
                        editor.update(cx, |e, cx| e.select_socket(id, window, cx));
                    } else if let Some(child) = current
                        .and_then(|i| entries.get(i + 1))
                        .filter(|child| child.depth > entry.depth)
                    {
                        select(child.id.clone(), cx, window);
                    }
                }
            }
            "left" => {
                if self.model.selected_socket.is_some() {
                    if let Some(bone) = self.model.selected.clone() {
                        select(bone, cx, window);
                    }
                    cx.stop_propagation();
                    return;
                }
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
        let mut rows = Vec::new();
        for entry in entries {
            let sockets = self
                .model
                .sockets
                .iter()
                .filter(|s| s.bone_id == entry.id && (entry.expanded || !filter.is_empty()))
                .filter(|s| {
                    filter.is_empty()
                        || s.name.to_lowercase().contains(&filter)
                        || entry.direct_match
                })
                .map(|s| (s.id.clone(), s.name.clone()))
                .collect::<Vec<_>>();
            let depth = entry.depth;
            rows.push(self.render_row(entry, cx));
            for (id, name) in sockets {
                let editor = self.editor.clone();
                let focus = self.focus_handle.clone();
                let selected = self.model.selected_socket.as_ref() == Some(&id);
                rows.push(
                    h_flex()
                        .id(SharedString::from(format!("socket-{id}")))
                        .debug_selector(|| format!("socket-{id}"))
                        .h(px(ROW_HEIGHT))
                        .pl(px(30.0 + depth as f32 * INDENT))
                        .gap_1()
                        .items_center()
                        .cursor_pointer()
                        .bg(if selected {
                            theme.list_active
                        } else {
                            theme.transparent
                        })
                        .child(Icon::new(IconName::Link).size_3p5().text_color(theme.info))
                        .child(div().text_sm().child(name))
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            focus.focus(window, cx);
                            let _ =
                                editor.update(cx, |e, cx| e.select_socket(id.clone(), window, cx));
                        })
                        .into_any_element(),
                );
            }
        }

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
                    .debug_selector(|| "hier-expand-all".into())
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
                    .debug_selector(|| "hier-collapse-all".into())
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
            .right(
                div()
                    .font_family(MONO)
                    .child(format!("{animated} animated")),
            );

        v_flex()
            .id("skeletal-hierarchy")
            .size_full()
            .bg(theme.sidebar)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, w, cx| this.on_key_down(e, w, cx)))
            .child(header)
            .child(search_row)
            .when_some(self.tools.clone(), |col, tools| col.child(tools))
            .child(body)
            .child(footer)
            .into_any_element()
    }
}

#[cfg(test)]
mod interaction_tests {
    use super::BoneHierarchyPanel;
    use crate::core::{assets, sample};
    use crate::editor::SkeletalAnimEditorPanel;
    use gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext};

    fn draw(cx: &mut VisualTestContext) {
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update(|w, cx| {
                w.refresh();
                w.draw(cx).clear()
            });
        }
    }

    fn paste_text(cx: &mut VisualTestContext, text: &str) {
        cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_owned())));
        cx.simulate_keystrokes("secondary-v");
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        draw(cx);
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        cx.simulate_mouse_move(bounds.center(), None, Modifiers::default());
        cx.simulate_mouse_down(
            bounds.center(),
            gpui::MouseButton::Left,
            Modifiers::default(),
        );
        draw(cx);
        cx.simulate_mouse_up(
            bounds.center(),
            gpui::MouseButton::Left,
            Modifiers::default(),
        );
        draw(cx);
    }

    #[gpui::test]
    fn rendered_socket_rows_expand_select_and_navigate_with_keyboard_and_mouse(
        cx: &mut TestAppContext,
    ) {
        cx.update(ui::init);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
        let path = dir.path().join("rig.skeleton");
        let mut skeleton = sample::humanoid_skeleton();
        skeleton.bones.retain(|b| b.id == "root");
        let mut asset = assets::SkeletonAsset::new(skeleton);
        asset
            .sockets
            .add(
                &asset.skeleton(),
                skeletal_assets::sockets::Socket {
                    id: "attachment".into(),
                    name: "Attachment".into(),
                    bone_id: "root".into(),
                    local_transform: crate::core::Transform::IDENTITY,
                },
            )
            .unwrap();
        assets::save_skeleton_asset(&path, &asset).unwrap();
        let mut editor_entity = None;
        let mut hierarchy_entity = None;
        let (_, cx) = cx.add_window_view(|w, cx| {
            let editor = cx.new(|cx| SkeletalAnimEditorPanel::new_with_path(path, w, cx).unwrap());
            editor.update(cx, |e, _| e.expanded_bones.clear());
            let hierarchy = cx.new(|cx| BoneHierarchyPanel::new(editor.downgrade(), cx));
            editor_entity = Some(editor);
            hierarchy_entity = Some(hierarchy.clone());
            ui::Root::new(hierarchy.into(), w, cx)
        });
        let hierarchy = hierarchy_entity.unwrap();
        cx.update(|w, _| w.activate_window());
        let editor = editor_entity.unwrap();
        editor.update(cx, |e, cx| {
            hierarchy.update(cx, |p, cx| p.attach(e, &editor, cx))
        });
        draw(cx);
        click(cx, "hier-collapse-all");
        assert!(cx.debug_bounds("socket-attachment").is_none());
        click(cx, "hier-expand-all");
        assert!(cx.debug_bounds("socket-attachment").is_some());
        click(cx, "socket-attachment");
        editor.read_with(cx, |e, _| {
            assert_eq!(e.selected_socket.as_deref(), Some("attachment"))
        });
        cx.simulate_keystrokes("left");
        draw(cx);
        editor.read_with(cx, |e, _| {
            assert!(e.selected_socket.is_none());
            assert_eq!(e.selected_bone.as_deref(), Some("root"));
        });
        cx.simulate_keystrokes("down");
        draw(cx);
        editor.read_with(cx, |e, _| {
            assert_eq!(e.selected_socket.as_deref(), Some("attachment"))
        });
        cx.simulate_keystrokes("up right");
        draw(cx);
        editor.read_with(cx, |e, _| {
            assert_eq!(e.selected_socket.as_deref(), Some("attachment"))
        });
        click(cx, "bone-root");
        cx.simulate_keystrokes("insert");
        draw(cx);
        let child = editor.read_with(cx, |e, _| {
            assert_eq!(e.skeleton.bones.len(), 2);
            e.selected_bone.clone().unwrap()
        });
        click(cx, "change-parent");
        cx.simulate_keystrokes("down enter");
        draw(cx);
        editor.read_with(cx, |e, _| {
            assert!(e.skeleton.bone(&child).unwrap().parent.is_none())
        });
        let bounds = cx.debug_bounds("bone-root").unwrap();
        cx.simulate_mouse_move(bounds.center(), None, Modifiers::default());
        cx.simulate_mouse_down(
            bounds.center(),
            gpui::MouseButton::Right,
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            bounds.center(),
            gpui::MouseButton::Right,
            Modifiers::default(),
        );
        draw(cx);
        cx.simulate_keystrokes("down enter");
        draw(cx);
        editor.read_with(cx, |e, _| assert_eq!(e.skeleton.bones.len(), 3));
        click(cx, "socket-attachment");
        cx.simulate_keystrokes("delete");
        draw(cx);
        editor.read_with(cx, |e, _| assert!(e.sockets.is_empty()));
    }

    #[gpui::test]
    fn rendered_name_commit_and_text_shortcuts_preserve_the_document(cx: &mut TestAppContext) {
        cx.update(ui::init);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
        let path = dir.path().join("rig.skeleton");
        let mut skeleton = sample::humanoid_skeleton();
        skeleton.bones.retain(|b| b.id == "root");
        let mut asset = assets::SkeletonAsset::new(skeleton);
        asset
            .sockets
            .add(
                &asset.skeleton(),
                skeletal_assets::sockets::Socket {
                    id: "attachment".into(),
                    name: "Attachment".into(),
                    bone_id: "root".into(),
                    local_transform: crate::core::Transform::IDENTITY,
                },
            )
            .unwrap();
        assets::save_skeleton_asset(&path, &asset).unwrap();
        let mut editor_entity = None;
        let mut hierarchy_entity = None;
        let (_, cx) = cx.add_window_view(|w, cx| {
            let editor = cx.new(|cx| SkeletalAnimEditorPanel::new_with_path(path, w, cx).unwrap());
            let hierarchy = cx.new(|cx| BoneHierarchyPanel::new(editor.downgrade(), cx));
            editor_entity = Some(editor);
            hierarchy_entity = Some(hierarchy.clone());
            ui::Root::new(hierarchy.into(), w, cx)
        });
        let hierarchy = hierarchy_entity.unwrap();
        let editor = editor_entity.unwrap();
        editor.update(cx, |e, cx| {
            hierarchy.update(cx, |h, cx| h.attach(e, &editor, cx))
        });
        click(cx, "socket-attachment");
        cx.simulate_keystrokes("f2");
        draw(cx);
        hierarchy.update_in(cx, |h, w, cx| {
            assert!(h
                .tools
                .as_ref()
                .unwrap()
                .read(cx)
                .name_focus(cx)
                .is_focused(w))
        });
        cx.simulate_keystrokes("secondary-a");
        paste_text(cx, "Mount");
        cx.simulate_keystrokes("insert backspace delete");
        editor.read_with(cx, |e, _| {
            assert_eq!(e.skeleton.bones.len(), 1);
            assert!(e.sockets.get("attachment").is_some());
        });
        cx.simulate_keystrokes("secondary-a");
        paste_text(cx, "Mount");
        draw(cx);
        cx.simulate_keystrokes("enter");
        editor.read_with(cx, |e, _| {
            assert_eq!(e.sockets.get("attachment").unwrap().name, "Mount")
        });
    }
}

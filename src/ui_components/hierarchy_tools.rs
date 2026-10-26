//! selection-based hierarchy commands; transforms stay in the shared properties panel.

use gpui::prelude::FluentBuilder;
use gpui::*;
use ui::button::{Button, ButtonVariants};
use ui::input::{InputEvent, InputState, TextInput};
use ui::popup_menu::PopupMenuExt;
use ui::{h_flex, v_flex, ActiveTheme, Disableable, IconName, Sizable};

use crate::core::Skeleton;
use crate::editor::hierarchy_edit::HierarchyCommand;
use crate::editor::hierarchy_history::Selection;
use crate::editor::{EditorMode, SkeletalAnimEditorPanel};
use skeletal_assets::sockets::Sockets;

pub(crate) struct HierarchyTools {
    editor: WeakEntity<SkeletalAnimEditorPanel>,
    skeleton: Skeleton,
    sockets: Sockets,
    selection: Selection,
    revision: u64,
    editable: bool,
    busy: bool,
    name: Entity<InputState>,
    pending_name: Option<String>,
    _observer: Subscription,
    _name_events: Subscription,
}

impl HierarchyTools {
    pub(crate) fn new(
        editor: Entity<SkeletalAnimEditorPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Display name"));
        let e = editor.read(cx);
        let mut this = Self {
            editor: editor.downgrade(),
            skeleton: e.skeleton.clone(),
            sockets: e.sockets.clone(),
            selection: e.hierarchy_selection(),
            revision: e.hierarchy_revision,
            editable: e.mode == EditorMode::Skeleton,
            busy: e.checking_hierarchy,
            _name_events: cx.subscribe_in(&name, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.rename_selected(window, cx);
                }
            }),
            name,
            pending_name: Some(String::new()),
            _observer: cx.observe(&editor, |this, editor, cx| {
                let e = editor.read(cx);
                let selection = e.hierarchy_selection();
                if this.revision != e.hierarchy_revision
                    || this.selection != selection
                    || this.busy != e.checking_hierarchy
                {
                    let previous_name = this.selected_name();
                    this.skeleton = e.skeleton.clone();
                    this.sockets = e.sockets.clone();
                    this.revision = e.hierarchy_revision;
                    let selection_changed = this.selection != selection;
                    this.selection = selection;
                    this.busy = e.checking_hierarchy;
                    if selection_changed || previous_name != this.selected_name() {
                        this.pending_name = Some(this.selected_name());
                    }
                    cx.notify();
                }
            }),
        };
        this.pending_name = Some(this.selected_name());
        this
    }

    pub(super) fn name_focus(&self, cx: &App) -> FocusHandle {
        self.name.focus_handle(cx)
    }

    fn selected_name(&self) -> String {
        if let Some(socket) = self
            .selection
            .socket
            .as_deref()
            .and_then(|id| self.sockets.get(id))
        {
            return socket.name.clone();
        }
        self.selection
            .bone
            .as_deref()
            .and_then(|id| self.skeleton.bone(id))
            .map(|b| b.name.clone())
            .unwrap_or_default()
    }

    fn command_button(
        &self,
        id: &'static str,
        label: &'static str,
        command: Option<HierarchyCommand>,
    ) -> impl IntoElement {
        let editor = self.editor.clone();
        Button::new(id)
            .debug_selector(|| id.into())
            .label(label)
            .small()
            .ghost()
            .disabled(command.is_none() || self.busy)
            .on_click(move |_, window, cx| {
                if let Some(command) = &command {
                    let _ = editor.update(cx, |e, cx| {
                        e.request_hierarchy_command(command.clone(), window, cx)
                    });
                }
            })
    }

    fn rename_selected(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let name = self.name.read(cx).value().trim().to_string();
        let command = if let Some(id) = &self.selection.socket {
            Some(HierarchyCommand::RenameSocket {
                id: id.clone(),
                name,
            })
        } else {
            self.selection
                .bone
                .clone()
                .map(|id| HierarchyCommand::RenameBone { id, name })
        };
        if let Some(command) = command {
            let _ = self
                .editor
                .update(cx, |e, cx| e.request_hierarchy_command(command, window, cx));
        }
    }

    fn render_rename(&self, cx: &Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let rename = Button::new("rename-selection")
            .debug_selector(|| "rename-selection".into())
            .label("Rename")
            .small()
            .ghost()
            .disabled(self.busy)
            .on_click(move |_, window, cx| {
                let _ = this.update(cx, |this, cx| this.rename_selected(window, cx));
            });
        h_flex()
            .gap_1()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(TextInput::new(&self.name).small()),
            )
            .child(rename)
    }

    fn render_parent_menu(&self) -> impl IntoElement {
        let editor = self.editor.clone();
        let bone = self.selection.bone.clone();
        let skeleton = self.skeleton.clone();
        let socket = self
            .selection
            .socket
            .as_deref()
            .and_then(|id| self.sockets.get(id))
            .cloned();
        let parent = socket.as_ref().map(|s| s.bone_id.as_str()).or_else(|| {
            bone.as_deref()
                .and_then(|id| self.skeleton.bone(id))
                .and_then(|b| b.parent.as_deref())
        });
        let parent_name = parent
            .and_then(|id| self.skeleton.bone(id))
            .map_or("Root", |b| b.name.as_str());
        let label = format!(
            "{}: {parent_name}",
            if socket.is_some() { "Bone" } else { "Parent" }
        );
        Button::new("change-parent")
            .debug_selector(|| "change-parent".into())
            .label(label)
            .small()
            .outline()
            .dropdown_caret(true)
            .disabled(self.busy)
            .tooltip(if socket.is_some() {
                "Change the socket's bone, keeping its local offset"
            } else {
                "Preserves the world bind pose. Animated hierarchies require track migration."
            })
            .popup_menu(move |mut menu, _, _| {
                let forbidden = bone
                    .as_deref()
                    .and_then(|id| skeleton.subtree_ids(id).ok())
                    .unwrap_or_default();
                if socket.is_none() {
                    let root_editor = editor.clone();
                    let id = bone.clone();
                    menu = menu
                        .menu_handler("Move to Root", move |window, cx| {
                            if let Some(id) = &id {
                                let command = HierarchyCommand::Reparent {
                                    id: id.clone(),
                                    parent: None,
                                };
                                let _ = root_editor.update(cx, |e, cx| {
                                    e.request_hierarchy_command(command, window, cx)
                                });
                            }
                        })
                        .separator();
                }
                for (target, depth) in skeleton.depth_first() {
                    let editor = editor.clone();
                    let id = bone.clone();
                    let parent = target.id.clone();
                    let socket = socket.clone();
                    let disabled = socket.is_none() && forbidden.contains(&target.id);
                    let label = format!(
                        "{}{} ({})",
                        "  ".repeat(depth.min(8)),
                        target.name,
                        target.id
                    );
                    menu = menu.menu_handler_with_icon_and_disabled(
                        label,
                        IconName::Component,
                        disabled,
                        move |window, cx| {
                            let command = if let Some(socket) = &socket {
                                Some(HierarchyCommand::ReattachSocket {
                                    id: socket.id.clone(),
                                    bone: parent.clone(),
                                })
                            } else {
                                id.clone().map(|id| HierarchyCommand::Reparent {
                                    id,
                                    parent: Some(parent.clone()),
                                })
                            };
                            if let Some(command) = command {
                                let _ = editor.update(cx, |e, cx| {
                                    e.request_hierarchy_command(command, window, cx)
                                });
                            }
                        },
                    );
                }
                menu
            })
    }
}

impl Render for HierarchyTools {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.editable {
            return div().into_any_element();
        }
        if let Some(name) = self.pending_name.take() {
            self.name
                .update(cx, |input, cx| input.set_value(name, window, cx));
        }
        let bone = self.selection.bone.clone();
        let has_children = bone
            .as_deref()
            .is_some_and(|id| !self.skeleton.children_of(id).is_empty());
        let delete = if let Some(socket) = &self.selection.socket {
            Some(HierarchyCommand::DeleteSocket(socket.clone()))
        } else {
            bone.clone()
                .filter(|_| !has_children)
                .map(|id| HierarchyCommand::DeleteBone { id, subtree: false })
        };
        let add = h_flex()
            .gap_1()
            .child(self.command_button(
                "add-root",
                "Add Root",
                Some(HierarchyCommand::AddBone { parent: None }),
            ))
            .child(
                self.command_button(
                    "add-child",
                    "Add Child",
                    bone.clone()
                        .map(|id| HierarchyCommand::AddBone { parent: Some(id) }),
                ),
            )
            .child(
                self.command_button(
                    "add-socket",
                    "Add Socket",
                    bone.clone()
                        .map(|bone| HierarchyCommand::AddSocket { bone }),
                ),
            );
        let delete = h_flex()
            .gap_1()
            .child(self.command_button("delete-selection", "Delete", delete))
            .when(self.selection.socket.is_none() && has_children, |row| {
                row.child(
                    self.command_button(
                        "delete-subtree",
                        "Delete Subtree…",
                        bone.clone()
                            .map(|id| HierarchyCommand::DeleteBone { id, subtree: true }),
                    ),
                )
            });
        v_flex()
            .on_action(cx.listener(|this, _: &ui::input::Enter, window, cx| {
                if this.name.focus_handle(cx).is_focused(window) {
                    // the input emitted PressEnter; prevent fallback newline insertion.
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .w_full()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(add)
            .when(bone.is_some(), |col| {
                col.child(self.render_rename(cx))
                    .child(self.render_parent_menu())
                    .child(delete)
            })
            .when(self.busy, |col| {
                col.child(div().text_xs().child("Checking dependent assets…"))
            })
            .into_any_element()
    }
}

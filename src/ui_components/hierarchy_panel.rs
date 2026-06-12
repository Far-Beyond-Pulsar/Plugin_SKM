//! Left dock panel: a tree view of the skeleton's bone hierarchy.
//!
//! Bones with children are rendered as expandable folders; leaf bones are
//! selectable items. Clicking either selects the bone, which drives the
//! viewport highlight and the properties panel on the right.

use gpui::*;
use ui::dock::PanelEvent;
use ui::hierarchical_tree::{render_tree_folder, render_tree_item, tree_colors};
use ui::scroll::ScrollbarAxis;
use ui::{v_flex, ActiveTheme, IconName, StyledExt};

use crate::editor::SkeletalAnimEditorPanel;

pub struct BoneHierarchyPanel {
    editor: WeakEntity<SkeletalAnimEditorPanel>,
    focus_handle: FocusHandle,
}

impl BoneHierarchyPanel {
    pub fn new(editor: WeakEntity<SkeletalAnimEditorPanel>, cx: &mut Context<Self>) -> Self {
        Self {
            editor,
            focus_handle: cx.focus_handle(),
        }
    }

    fn render_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(editor) = self.editor.upgrade() else {
            return Vec::new();
        };
        let entries: Vec<(String, String, usize, bool, bool, bool)> = {
            let editor = editor.read(cx);

            let mut entries = Vec::new();
            let mut skip_below_depth: Option<usize> = None;

            for (bone, depth) in editor.skeleton.depth_first() {
                if let Some(skip_depth) = skip_below_depth {
                    if depth > skip_depth {
                        continue;
                    }
                    skip_below_depth = None;
                }

                let bone_id = bone.id.clone();
                let name = bone.name.clone();
                let has_children = !editor.skeleton.children_of(&bone.id).is_empty();
                let is_selected = editor.selected_bone.as_deref() == Some(bone.id.as_str());
                let is_expanded = has_children && editor.is_bone_expanded(&bone.id);

                if has_children && !is_expanded {
                    skip_below_depth = Some(depth);
                }

                entries.push((bone_id, name, depth, has_children, is_expanded, is_selected));
            }

            entries
        };

        let mut rows = Vec::new();

        for (bone_id, name, depth, has_children, is_expanded, is_selected) in entries {
            if has_children {
                let editor_weak = self.editor.clone();
                let click_id = bone_id.clone();
                rows.push(render_tree_folder(
                    &format!("bone-{bone_id}"),
                    &name,
                    IconName::Component,
                    tree_colors::CODE_PURPLE,
                    depth,
                    is_expanded,
                    move |_this, _event, window, cx| {
                        let _ = editor_weak.update(cx, |editor, cx| {
                            editor.toggle_bone_expanded(&click_id, cx);
                            editor.select_bone(Some(click_id.clone()), window, cx);
                        });
                    },
                    cx,
                ));
            } else {
                let editor_weak = self.editor.clone();
                let click_id = bone_id.clone();
                rows.push(render_tree_item(
                    &format!("bone-{bone_id}"),
                    &name,
                    tree_colors::CODE_BLUE,
                    depth,
                    is_selected,
                    move |_this, _event, window, cx| {
                        let _ = editor_weak.update(cx, |editor, cx| {
                            editor.select_bone(Some(click_id.clone()), window, cx);
                        });
                    },
                    cx,
                ));
            }
        }

        rows
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.render_rows(cx);

        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .py_1()
            .scrollable(ScrollbarAxis::Vertical)
            .children(rows)
    }
}

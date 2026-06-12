//! Docking workspace layout and the panel's `Render` implementation.
//!
//! Layout: bone hierarchy (left), 3D viewport (center), bone properties
//! (right), keyframe timeline (bottom).

use std::sync::Arc;

use gpui::*;
use ui::dock::{DockItem, PanelEvent};
use ui::workspace::Workspace;
use ui::ActiveTheme;

use crate::rendering::{TimelinePanel, ViewportPanel};
use crate::ui_components::{BoneHierarchyPanel, BonePropertiesPanel};

use super::panel::SkeletalAnimEditorPanel;

impl SkeletalAnimEditorPanel {
    pub fn initialize_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.is_some() {
            return;
        }

        let editor_weak = cx.entity().downgrade();

        let workspace = cx.new(|cx| {
            Workspace::new_with_channel("skeletal-anim-workspace", ui::dock::DockChannel(1), window, cx)
        });

        let viewport_panel = cx.new(|cx| ViewportPanel::new(editor_weak.clone(), cx));
        let timeline_panel = cx.new(|cx| TimelinePanel::new(editor_weak.clone(), cx));
        let hierarchy_panel = cx.new(|cx| BoneHierarchyPanel::new(editor_weak.clone(), cx));
        let properties_panel = cx.new(|cx| BonePropertiesPanel::new(editor_weak.clone(), cx));

        workspace.update(cx, |workspace, cx| {
            let dock_area_weak = workspace.dock_area().downgrade();

            let center = DockItem::tabs(
                vec![Arc::new(viewport_panel.clone()) as Arc<dyn ui::dock::PanelView>],
                Some(0),
                &dock_area_weak,
                window,
                cx,
            );

            let left = DockItem::tabs(
                vec![Arc::new(hierarchy_panel.clone()) as Arc<dyn ui::dock::PanelView>],
                Some(0),
                &dock_area_weak,
                window,
                cx,
            );

            let right = DockItem::tabs(
                vec![Arc::new(properties_panel.clone()) as Arc<dyn ui::dock::PanelView>],
                Some(0),
                &dock_area_weak,
                window,
                cx,
            );

            let bottom = DockItem::tabs(
                vec![Arc::new(timeline_panel.clone()) as Arc<dyn ui::dock::PanelView>],
                Some(0),
                &dock_area_weak,
                window,
                cx,
            );

            workspace.initialize(center, Some(left), Some(right), None, window, cx);

            // `Workspace::initialize` doesn't expose a custom size for the
            // bottom dock; set it directly so the timeline has more room
            // than the default height.
            workspace.dock_area().update(cx, |dock_area, cx| {
                dock_area.set_bottom_dock(bottom, Some(px(260.0)), true, window, cx);
            });
        });

        self.workspace = Some(workspace);
        self.viewport_panel = Some(viewport_panel);
        self.timeline_panel = Some(timeline_panel);
    }
}

impl EventEmitter<PanelEvent> for SkeletalAnimEditorPanel {}

impl Focusable for SkeletalAnimEditorPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ui::dock::Panel for SkeletalAnimEditorPanel {
    fn panel_name(&self) -> &'static str {
        "skeletal-anim-editor"
    }

    fn title(&self, _window: &Window, _cx: &App) -> AnyElement {
        "Skeletal Animation".into_any_element()
    }
}

impl Render for SkeletalAnimEditorPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.workspace.is_none() {
            self.initialize_workspace(window, cx);
        }

        div()
            .size_full()
            .bg(cx.theme().background)
            .map(|el| {
                if let Some(workspace) = &self.workspace {
                    el.child(workspace.clone())
                } else {
                    el.child("Initializing...")
                }
            })
    }
}

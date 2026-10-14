//! Docking workspace layout and the panel's `Render` implementation.
//!
//! Layout: bone hierarchy (left), 3D viewport (center), bone properties
//! (right), keyframe timeline (bottom).

use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use ui::button::{Button, ButtonVariants};
use ui::dock::{DockItem, PanelEvent};
use ui::tag::Tag;
use ui::workspace::Workspace;
use ui::{h_flex, ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt};

use crate::rendering::{TimelinePanel, ViewportPanel};
use crate::ui_components::{BoneHierarchyPanel, BonePropertiesPanel};

use super::panel::{EditorMode, SkeletalAnimEditorPanel};
use crate::ui_components::chrome::{toolbar_divider, MONO};
use plugin_editor_api::{EditorMenu, EditorMenuStrip};

actions!(
    skeletal_anim_editor,
    [
        SaveAnimation,
        TogglePlay,
        GoToStart,
        GoToEnd,
        PreviousKeyframe,
        NextKeyframe,
        InsertKeyframe,
        DeleteKeyframes,
        CutKeyframes,
        CopyKeyframes,
        PasteKeyframes,
        SelectAllKeyframes,
        ToggleLoop,
        CyclePlaybackSpeed,
        ToggleBoneTracers,
        ExpandAllBones,
        CollapseAllBones,
    ]
);

/// The editor's menus (Pulsar-Native#1012): a skeleton gets File and View; a
/// sequence adds its timeline and
/// keyframe tools, dispatched from the strip to the panel's handlers.
fn editor_menus(has_path: bool, mode: EditorMode) -> EditorMenuStrip {
    let strip = EditorMenuStrip::new("skeletal-anim-menus")
        .menu(EditorMenu::new("File").action_if("Save", SaveAnimation, has_path));
    if mode == EditorMode::Skeleton {
        return strip.menu(
            EditorMenu::new("View")
                .action("Expand All Bones", ExpandAllBones)
                .action("Collapse All Bones", CollapseAllBones),
        );
    }
    strip
        .menu(
            EditorMenu::new("Edit")
                .action("Cut Keyframes", CutKeyframes)
                .action("Copy Keyframes", CopyKeyframes)
                .action("Paste Keyframes", PasteKeyframes)
                .separator()
                .action("Select All Keyframes", SelectAllKeyframes)
                .action("Delete Keyframes", DeleteKeyframes),
        )
        .menu(
            EditorMenu::new("Tools")
                .action("Play / Pause", TogglePlay)
                .action("Go to Start", GoToStart)
                .action("Go to End", GoToEnd)
                .action("Previous Keyframe", PreviousKeyframe)
                .action("Next Keyframe", NextKeyframe)
                .separator()
                .action("Insert Keyframe", InsertKeyframe)
                .separator()
                .action("Toggle Loop", ToggleLoop)
                .action("Cycle Playback Speed", CyclePlaybackSpeed),
        )
        .menu(
            EditorMenu::new("View")
                .action("Bone Tracers", ToggleBoneTracers)
                .separator()
                .action("Expand All Bones", ExpandAllBones)
                .action("Collapse All Bones", CollapseAllBones),
        )
}

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
        // A skeleton has no animation, so no timeline.
        let has_timeline = self.mode == EditorMode::Sequence;
        let timeline_panel = has_timeline.then(|| cx.new(|cx| TimelinePanel::new(editor_weak.clone(), cx)));
        let hierarchy_panel = cx.new(|cx| BoneHierarchyPanel::new(editor_weak.clone(), cx));
        let properties_panel = cx.new(|cx| BonePropertiesPanel::new(editor_weak.clone(), cx));

        // The sidebars render from their own snapshots and refresh only when
        // what they show changes, instead of on every editor notify.
        let editor_entity = cx.entity();
        hierarchy_panel.update(cx, |panel, cx| panel.attach(self, &editor_entity, cx));
        properties_panel.update(cx, |panel, cx| panel.attach(self, &editor_entity, cx));
        if let Some(timeline_panel) = &timeline_panel {
            timeline_panel.update(cx, |panel, cx| panel.attach(self, &editor_entity, cx));
        }

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

            let bottom = timeline_panel.as_ref().map(|timeline_panel| {
                DockItem::tabs(
                    vec![Arc::new(timeline_panel.clone()) as Arc<dyn ui::dock::PanelView>],
                    Some(0),
                    &dock_area_weak,
                    window,
                    cx,
                )
            });

            workspace.initialize(center, Some(left), Some(right), None, window, cx);

            // `Workspace::initialize` doesn't expose a custom size for the
            // bottom dock; set it directly so the timeline has more room
            // than the default height.
            //
            // TODO: Modify `Workspace::initialize` to expose a custom size for the bottom dock.
            if let Some(bottom) = bottom {
                workspace.dock_area().update(cx, |dock_area, cx| {
                    dock_area.set_bottom_dock(bottom, Some(px(260.0)), true, window, cx);
                });
            }
        });

        self.workspace = Some(workspace);
        self.viewport_panel = Some(viewport_panel);
        self.timeline_panel = timeline_panel;
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
        match self.mode {
            EditorMode::Skeleton => "skeleton-editor",
            EditorMode::Sequence => "animation-sequence-editor",
        }
    }

    fn title(&self, _window: &Window, _cx: &App) -> AnyElement {
        match self.mode {
            EditorMode::Skeleton => "Skeleton",
            EditorMode::Sequence => "Animation Sequence",
        }
        .into_any_element()
    }
}

impl Render for SkeletalAnimEditorPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.workspace.is_none() {
            self.initialize_workspace(window, cx);
        }

        let entity = cx.entity().clone();
        let theme = cx.theme().clone();
        let is_playing = self.playback.playing;
        let is_dirty = self.is_dirty;
        let has_path = self.current_asset_path.is_some();
        let fps = self.animation.fps_clamped();
        let mode = self.mode;
        let is_sequence = mode == EditorMode::Sequence;
        let skeleton_field = is_sequence.then(|| self.render_skeleton_field(window, cx));
        let banner = self.render_banner(cx);

        let save_entity = entity.clone();
        let play_entity = entity.clone();
        let start_entity = entity.clone();

        let toolbar = h_flex()
            .w_full()
            .flex_shrink_0()
            .h(px(38.0))
            .px_2()
            .gap_1p5()
            .items_center()
            .bg(theme.secondary)
            .border_b_1()
            .border_color(theme.border)
            .child(editor_menus(has_path, mode))
            .child(toolbar_divider(cx))
            .child(
                Button::new("skeletal-save")
                    .icon(IconName::FloppyDisk)
                    .label("Save")
                    .small()
                    .when(is_dirty, |b| b.primary())
                    .when(!is_dirty, |b| b.ghost())
                    .disabled(!has_path)
                    .tooltip("Save (Ctrl+S)")
                    .on_click(move |_, _window, cx| {
                        save_entity.update(cx, |editor, cx| editor.save_and_report(cx));
                    }),
            )
            .child(toolbar_divider(cx))
            .when(is_sequence, |bar| {
                bar.child(
                    Button::new("skeletal-to-start")
                        .icon(IconName::SkipPrev)
                        .ghost()
                        .small()
                        .tooltip("Return to the start of the play range")
                        .on_click(move |_, window, cx| {
                            start_entity.update(cx, |editor, cx| editor.goto_start(window, cx));
                        }),
                )
                .child(
                    Button::new("skeletal-toggle-play")
                        .icon(if is_playing {
                            IconName::Pause
                        } else {
                            IconName::Play
                        })
                        .primary()
                        .small()
                        .tooltip(if is_playing {
                            "Pause (Space)"
                        } else {
                            "Play (Space)"
                        })
                        .on_click(move |_, _window, cx| {
                            play_entity.update(cx, |editor, cx| editor.toggle_play(cx));
                        }),
                )
                .child(
                    div()
                        .px_2()
                        .text_sm()
                        .font_family(MONO)
                        .text_color(theme.foreground)
                        .child(format!("{:.2}s", self.playback.time))
                        .child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child(format!(" / {:.2}s", self.animation.duration)),
                        ),
                )
                .child(toolbar_divider(cx))
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(
                            Icon::new(IconName::Keyframes)
                                .size(px(14.0))
                                .text_color(theme.info),
                        )
                        .child(
                            div()
                                .text_sm()
                                .font_semibold()
                                .text_color(theme.foreground)
                                .child(self.animation.name.clone()),
                        )
                        .child(Tag::secondary().small().child(format!("{fps:.0} fps")))
                        .child(
                            Tag::secondary()
                                .small()
                                .child(format!("{} frames", self.animation.last_frame() + 1)),
                        ),
                )
                .child(toolbar_divider(cx))
                .children(skeleton_field)
            })
            .when(!is_sequence, |bar| {
                bar.child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(
                            Icon::new(IconName::Accessibility)
                                .size(px(14.0))
                                .text_color(theme.info),
                        )
                        .child(
                            div()
                                .text_sm()
                                .font_semibold()
                                .text_color(theme.foreground)
                                .child("Skeleton"),
                        )
                        .child(
                            Tag::secondary()
                                .small()
                                .child(format!("{} bones", self.skeleton.bones.len())),
                        )
                        .when_some(self.asset_id.as_ref(), |row, id| {
                            row.child(Tag::secondary().small().child(format!("id {}", id.short())))
                        }),
                )
            })
            .child(div().flex_1())
            .child(if is_dirty {
                Tag::warning().small().child("Unsaved changes").into_any_element()
            } else if has_path {
                Tag::success().small().child("Saved").into_any_element()
            } else {
                Tag::secondary()
                    .small()
                    .child("Sample (not saved)")
                    .into_any_element()
            });

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if event.keystroke.modifiers.secondary() && event.keystroke.key == "s" {
                    this.save_and_report(cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &SaveAnimation, _, cx| this.save_and_report(cx)))
            .on_action(cx.listener(|this, _: &TogglePlay, _, cx| this.toggle_play(cx)))
            .on_action(cx.listener(|this, _: &GoToStart, window, cx| this.goto_start(window, cx)))
            .on_action(cx.listener(|this, _: &GoToEnd, window, cx| this.goto_end(window, cx)))
            .on_action(cx.listener(|this, _: &PreviousKeyframe, window, cx| {
                this.goto_prev_keyframe(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &NextKeyframe, window, cx| {
                    this.goto_next_keyframe(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &InsertKeyframe, window, cx| {
                    this.insert_keyframe(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &DeleteKeyframes, window, cx| {
                this.delete_selected_keyframes(window, cx)
            }))
            .on_action(cx.listener(|this, _: &CutKeyframes, window, cx| {
                this.cut_selected_keyframes(window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyKeyframes, _, _| this.copy_selected_keyframes()))
            .on_action(
                cx.listener(|this, _: &PasteKeyframes, window, cx| {
                    this.paste_keyframes(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &SelectAllKeyframes, window, cx| {
                this.select_all_keyframes(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleLoop, _, cx| this.toggle_loop(cx)))
            .on_action(cx.listener(|this, _: &CyclePlaybackSpeed, _, cx| this.cycle_speed(cx)))
            .on_action(cx.listener(|this, _: &ToggleBoneTracers, _, cx| this.toggle_tracers(cx)))
            .on_action(cx.listener(|this, _: &ExpandAllBones, _, cx| this.expand_all_bones(cx)))
            .on_action(cx.listener(|this, _: &CollapseAllBones, _, cx| this.collapse_all_bones(cx)))
            .child(toolbar)
            .children(banner)
            .child(div().flex_1().min_h_0().map(|el| {
                if let Some(workspace) = &self.workspace {
                    el.child(workspace.clone())
                } else {
                    el.child("Initializing...")
                }
            }))
    }
}

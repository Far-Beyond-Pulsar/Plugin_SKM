//! The timeline's track list (left gutter) as its own view.
//!
//! It renders from a snapshot of what it shows and refreshes only when that
//! changes. A view that reads an entity while rendering is rebuilt on every
//! notify of it, and the editor notifies on every playback frame; keeping the
//! gutter off the editor means it only rebuilds when a key is passed, the
//! selection changes, or the list scrolls.

use std::hash::{Hash, Hasher};

use gpui::prelude::FluentBuilder;
use gpui::*;
use ui::button::{Button, ButtonVariants};
use ui::tag::Tag;
use ui::{h_flex, v_flex, ActiveTheme, Icon, IconName, PixelsExt, Selectable, Sizable, StyledExt};

use crate::editor::panel::SkeletalAnimEditorPanel;
use crate::ui_components::chrome::MONO;

use super::layout::*;
use super::panel::{TimelinePanel, GUTTER_WIDTH};

struct GutterRow {
    bone_id: String,
    name: String,
    selected: bool,
    keyed_here: bool,
    key_count: usize,
}

pub struct TimelineGutter {
    editor: WeakEntity<SkeletalAnimEditorPanel>,
    panel: Option<WeakEntity<TimelinePanel>>,
    rows: Vec<GutterRow>,
    total_keys: usize,
    notify_count: usize,
    scroll_y: f32,
    only_selected: bool,
    signature: u64,
    _observer: Option<Subscription>,
}

impl TimelineGutter {
    pub fn new(editor: WeakEntity<SkeletalAnimEditorPanel>) -> Self {
        Self {
            editor,
            panel: None,
            rows: Vec::new(),
            total_keys: 0,
            notify_count: 0,
            scroll_y: 0.0,
            only_selected: false,
            signature: 0,
            _observer: None,
        }
    }

    /// Start following the editor. `state` is the editor's current state (the
    /// editor can't be read through its entity while it is rendering).
    pub fn attach(
        &mut self,
        state: &SkeletalAnimEditorPanel,
        editor: &Entity<SkeletalAnimEditorPanel>,
        panel: WeakEntity<TimelinePanel>,
        cx: &mut Context<Self>,
    ) {
        self.panel = Some(panel);
        self.capture(state);
        self._observer = Some(cx.observe(editor, |this, editor, cx| {
            let e = editor.read(cx);
            if this.signature_of(e) != this.signature {
                this.capture(e);
                cx.notify();
            }
        }));
    }

    fn signature_of(&self, e: &SkeletalAnimEditorPanel) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        e.selected_bone.hash(&mut h);
        e.animation_features.notifies.len().hash(&mut h);
        e.animation_features.notify_states.len().hash(&mut h);
        e.selected_notify.hash(&mut h);
        self.only_selected.hash(&mut h);
        e.skeleton_revision.hash(&mut h);
        for t in &e.animation.tracks {
            t.bone_id.hash(&mut h);
            t.keyframes.len().hash(&mut h);
            e.keyframe_at_playhead(&t.bone_id).is_some().hash(&mut h);
        }
        h.finish()
    }

    fn capture(&mut self, e: &SkeletalAnimEditorPanel) {
        self.signature = self.signature_of(e);
        self.total_keys = e.animation.key_count();
        self.notify_count = e.animation_features.notifies.len() + e.animation_features.notify_states.len();
        self.rows = e
            .animation
            .tracks
            .iter()
            .filter(|t| {
                !self.only_selected || e.selected_bone.as_deref() == Some(t.bone_id.as_str())
            })
            .map(|t| GutterRow {
                bone_id: t.bone_id.clone(),
                name: e
                    .skeleton
                    .bone(&t.bone_id)
                    .map_or_else(|| t.bone_id.clone(), |b| b.name.clone()),
                selected: e.selected_bone.as_deref() == Some(t.bone_id.as_str()),
                keyed_here: e.keyframe_at_playhead(&t.bone_id).is_some(),
                key_count: t.keyframes.len(),
            })
            .collect();
    }

    /// Mirror the canvas's vertical scroll. Cheap and idempotent, so the panel
    /// can call it every render.
    pub fn set_scroll_y(&mut self, scroll_y: f32, cx: &mut Context<Self>) {
        if (self.scroll_y - scroll_y).abs() > 0.01 {
            self.scroll_y = scroll_y;
            cx.notify();
        }
    }

    /// Switch the "selected bone only" filter. Call from an event handler, not
    /// from render (this reads the editor).
    pub fn set_only_selected(&mut self, only_selected: bool, cx: &mut Context<Self>) {
        if self.only_selected == only_selected {
            return;
        }
        self.only_selected = only_selected;
        if let Some(editor) = self.editor.upgrade() {
            self.capture(editor.read(cx));
        }
        cx.notify();
    }
}

impl Render for TimelineGutter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();

        let header = h_flex()
            .h(px(RULER_HEIGHT))
            .w_full()
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .items_center()
            .bg(theme.secondary)
            .border_b_1()
            .border_color(theme.border)
            .child(
                Icon::new(IconName::List)
                    .size(px(13.0))
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child("TRACKS"),
            )
            .child(div().flex_1())
            .child(
                Tag::secondary()
                    .small()
                    .rounded_full()
                    .child(self.rows.len().to_string()),
            );

        let summary = h_flex()
            .h(px(SUMMARY_HEIGHT))
            .w_full()
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .items_center()
            .bg(theme.sidebar)
            .border_b_1()
            .border_color(theme.border)
            .child(
                Icon::new(IconName::Keyframes)
                    .size(px(13.0))
                    .text_color(theme.info),
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.foreground)
                    .child("Summary"),
            )
            .child(
                div()
                    .text_xs()
                    .font_family(MONO)
                    .text_color(theme.muted_foreground)
                    .child(self.total_keys.to_string()),
            );

        let notify_count = self.notify_count;
        let editor_add = self.editor.clone();
        let editor_add_state = self.editor.clone();
        let notifies = h_flex()
            .h(px(NOTIFY_HEIGHT))
            .w_full()
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .items_center()
            .bg(theme.warning.opacity(0.06))
            .border_b_1()
            .border_color(theme.border)
            .child(
                Icon::new(IconName::Keyframes)
                    .size(px(12.0))
                    .text_color(theme.warning),
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.foreground)
                    .child("Notifies"),
            )
            .child(
                Tag::secondary()
                    .small()
                    .rounded_full()
                    .child(notify_count.to_string()),
            )
            .child(
                Button::new("timeline-add-notify")
                    .icon(IconName::Plus)
                    .ghost()
                    .xsmall()
                    .tooltip("Add notify at the playhead")
                    .on_click(move |_, _, cx| {
                        let editor = editor_add.clone();
                        let _ = editor.update(cx, |e, cx| {
                            e.add_notify(e.playback.time);
                            cx.notify();
                        });
                    }),
            )
            .child(
                Button::new("timeline-add-notify-state")
                    .icon(IconName::Timer)
                    .ghost()
                    .xsmall()
                    .tooltip("Add a notify state (begin / tick / end) at the playhead")
                    .on_click(move |_, _, cx| {
                        let editor = editor_add_state.clone();
                        let _ = editor.update(cx, |e, cx| {
                            e.add_notify_state(e.playback.time);
                            cx.notify();
                        });
                    }),
            );

        let mut list = div()
            .absolute()
            .left_0()
            .right_0()
            .top(px(-self.scroll_y))
            .flex()
            .flex_col();
        for (i, row) in self.rows.iter().enumerate() {
            let (selected, keyed_here) = (row.selected, row.keyed_here);
            let bone_id = row.bone_id.clone();
            let key_bone = row.bone_id.clone();
            let editor_row = self.editor.clone();
            let editor_key = self.editor.clone();
            let stripe = i % 2 == 1;

            list = list.child(
                h_flex()
                    .id(SharedString::from(format!(
                        "timeline-track-{}",
                        row.bone_id
                    )))
                    .relative()
                    .h(px(ROW_HEIGHT))
                    .w_full()
                    .flex_shrink_0()
                    .pl_3()
                    .pr_1()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border.opacity(0.45))
                    .bg(if selected {
                        theme.primary.opacity(0.16)
                    } else if stripe {
                        theme.foreground.opacity(0.022)
                    } else {
                        theme.transparent
                    })
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.list_hover))
                    .when(selected, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .bottom_0()
                                .w(px(3.0))
                                .bg(theme.primary),
                        )
                    })
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        let bone_id = bone_id.clone();
                        let _ =
                            editor_row.update(cx, |e, cx| e.select_bone(Some(bone_id), window, cx));
                    })
                    .child(
                        Icon::new(IconName::Component)
                            .size(px(12.0))
                            .text_color(if selected {
                                theme.primary
                            } else {
                                theme.info.opacity(0.8)
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_weight(if selected {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_color(theme.foreground)
                            .child(row.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .font_family(MONO)
                            .text_color(theme.muted_foreground)
                            .child(row.key_count.to_string()),
                    )
                    .child(
                        Button::new(SharedString::from(format!("tl-key-{key_bone}")))
                            .icon(if keyed_here {
                                IconName::Keyframe
                            } else {
                                IconName::KeyframePlus
                            })
                            .ghost()
                            .xsmall()
                            .selected(keyed_here)
                            .stop_propagation(true)
                            .tooltip(if keyed_here {
                                "Remove this track's key at the playhead"
                            } else {
                                "Key this track at the playhead"
                            })
                            .on_click(move |_, window, cx| {
                                let bone = key_bone.clone();
                                let _ = editor_key
                                    .update(cx, |e, cx| e.toggle_key_for_bone(&bone, window, cx));
                            }),
                    ),
            );
        }

        let panel = self.panel.clone();
        v_flex()
            .w(px(GUTTER_WIDTH))
            .h_full()
            .flex_shrink_0()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(header)
            .child(summary)
            .child(notifies)
            .child(
                div()
                    .id("timeline-gutter-scroll")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .on_scroll_wheel(move |event: &ScrollWheelEvent, _window, cx| {
                        let dy = match event.delta {
                            ScrollDelta::Lines(p) => p.y * 16.0,
                            ScrollDelta::Pixels(p) => p.y.as_f32(),
                        };
                        if let Some(panel) = panel.as_ref().and_then(|p| p.upgrade()) {
                            panel.update(cx, |p, cx| p.scroll_tracks(-dy, cx));
                        }
                    })
                    .child(list),
            )
            // Matches the canvas's scrollbar strip so the two stay aligned.
            .child(
                div()
                    .h(px(SCROLLBAR_HEIGHT))
                    .w_full()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(theme.border),
            )
    }
}

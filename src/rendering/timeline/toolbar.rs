//! Chrome around the timeline canvas: transport toolbar, track gutter and the
//! status bar. Everything here is stock engine components.

use gpui::prelude::FluentBuilder;
use gpui::*;
use ui::button::{Button, ButtonVariants};
use ui::input::NumberInput;
use ui::status_bar::StatusBar;
use ui::tag::Tag;
use ui::{h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, PixelsExt, Selectable, Sizable, StyledExt};

use crate::editor::panel::SkeletalAnimEditorPanel;
use crate::ui_components::chrome::{caption, toolbar_divider, MONO};

use super::layout::*;
use super::panel::{TimelinePanel, GUTTER_WIDTH};

type EditorCx<'a> = Context<'a, SkeletalAnimEditorPanel>;

impl TimelinePanel {
    /// Click handler that runs `f` against the editor.
    fn editor_click(
        &self,
        f: impl Fn(&mut SkeletalAnimEditorPanel, &mut Window, &mut EditorCx) + 'static,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        let editor = self.editor.clone();
        move |_, window, cx| {
            if let Some(editor) = editor.upgrade() {
                editor.update(cx, |e, cx| f(e, window, cx));
            }
        }
    }

    pub(super) fn render_toolbar(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(editor) = self.editor.upgrade() else {
            return div().into_any_element();
        };
        let (playing, looping, speed, has_keys, has_bone, can_paste, last_frame) = {
            let e = editor.read(cx);
            (
                e.playback.playing,
                e.playback.looping,
                e.playback.speed,
                !e.selected_keyframes.is_empty(),
                e.selected_bone.is_some(),
                !e.clipboard.is_empty(),
                e.animation.last_frame(),
            )
        };
        let theme = cx.theme().clone();

        let transport = h_flex()
            .gap_0p5()
            .items_center()
            .child(
                Button::new("tl-start")
                    .icon(IconName::SkipPrev)
                    .ghost()
                    .small()
                    .tooltip("Go to start (Home)")
                    .on_click(self.editor_click(|e, w, cx| e.goto_start(w, cx))),
            )
            .child(
                Button::new("tl-prev-key")
                    .icon(IconName::FastArrowLeft)
                    .ghost()
                    .small()
                    .tooltip("Previous keyframe ([)")
                    .on_click(self.editor_click(|e, w, cx| e.goto_prev_keyframe(w, cx))),
            )
            .child(
                Button::new("tl-prev-frame")
                    .icon(IconName::ChevronLeft)
                    .ghost()
                    .small()
                    .tooltip("Previous frame (←)")
                    .on_click(self.editor_click(|e, w, cx| e.step_frame(-1, w, cx))),
            )
            .child(
                Button::new("tl-play")
                    .icon(if playing { IconName::Pause } else { IconName::Play })
                    .primary()
                    .small()
                    .tooltip(if playing { "Pause (Space)" } else { "Play (Space)" })
                    .on_click(self.editor_click(|e, _w, cx| e.toggle_play(cx))),
            )
            .child(
                Button::new("tl-next-frame")
                    .icon(IconName::ChevronRight)
                    .ghost()
                    .small()
                    .tooltip("Next frame (→)")
                    .on_click(self.editor_click(|e, w, cx| e.step_frame(1, w, cx))),
            )
            .child(
                Button::new("tl-next-key")
                    .icon(IconName::FastArrowRight)
                    .ghost()
                    .small()
                    .tooltip("Next keyframe (])")
                    .on_click(self.editor_click(|e, w, cx| e.goto_next_keyframe(w, cx))),
            )
            .child(
                Button::new("tl-end")
                    .icon(IconName::SkipNext)
                    .ghost()
                    .small()
                    .tooltip("Go to end (End)")
                    .on_click(self.editor_click(|e, w, cx| e.goto_end(w, cx))),
            );

        let playback_options = h_flex()
            .gap_0p5()
            .items_center()
            .child(
                Button::new("tl-loop")
                    .icon(IconName::Repeat)
                    .ghost()
                    .small()
                    .selected(looping)
                    .tooltip(if looping { "Looping — click for one-shot" } else { "One-shot — click to loop" })
                    .on_click(self.editor_click(|e, _w, cx| e.toggle_loop(cx))),
            )
            .child(
                Button::new("tl-speed")
                    .label(format!("{}×", format_speed(speed)))
                    .ghost()
                    .small()
                    .tooltip("Playback speed")
                    .on_click(self.editor_click(|e, _w, cx| e.cycle_speed(cx))),
            );

        let timecode = self.inputs.as_ref().map(|inputs| {
            h_flex()
                .gap_1p5()
                .items_center()
                .child(caption("Frame", cx))
                .child(
                    div().w(px(92.0)).child(
                        NumberInput::new(&inputs.frame)
                            .small()
                            .suffix(
                                div()
                                    .pr_1()
                                    .text_xs()
                                    .font_family(MONO)
                                    .text_color(theme.muted_foreground)
                                    .child(format!("/ {last_frame}")),
                            ),
                    ),
                )
        });

        let editing = h_flex()
            .gap_0p5()
            .items_center()
            .child(
                Button::new("tl-key")
                    .icon(IconName::KeyframePlus)
                    .ghost()
                    .small()
                    .disabled(!has_bone)
                    .tooltip("Key the selected bone at the playhead (K)")
                    .on_click(self.editor_click(|e, w, cx| e.insert_keyframe(w, cx))),
            )
            .child(
                Button::new("tl-delete-key")
                    .icon(IconName::KeyframeMinus)
                    .ghost()
                    .small()
                    .disabled(!has_keys)
                    .tooltip("Delete selected keyframes (Del)")
                    .on_click(self.editor_click(|e, w, cx| e.delete_selected_keyframes(w, cx))),
            )
            .child(
                Button::new("tl-copy")
                    .icon(IconName::Copy)
                    .ghost()
                    .small()
                    .disabled(!has_keys)
                    .tooltip("Copy selected keyframes (Ctrl+C)")
                    .on_click(self.editor_click(|e, _w, _cx| e.copy_selected_keyframes())),
            )
            .child(
                Button::new("tl-paste")
                    .icon(IconName::PasteClipboard)
                    .ghost()
                    .small()
                    .disabled(!can_paste)
                    .tooltip("Paste keyframes at the playhead (Ctrl+V)")
                    .on_click(self.editor_click(|e, w, cx| e.paste_keyframes(w, cx))),
            );

        let view_options = h_flex()
            .gap_0p5()
            .items_center()
            .child(
                Button::new("tl-snap")
                    .icon(IconName::Magnet)
                    .ghost()
                    .small()
                    .selected(self.snap)
                    .tooltip("Snap scrubbing and dragging to whole frames")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.snap = !this.snap;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("tl-filter")
                    .icon(IconName::Filter)
                    .ghost()
                    .small()
                    .selected(self.only_selected)
                    .tooltip("Show only the selected bone's track")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.only_selected = !this.only_selected;
                        this.view.scroll_y = 0.0;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("tl-ruler")
                    .label(match self.ruler {
                        RulerMode::Frames => "Frames",
                        RulerMode::Seconds => "Seconds",
                    })
                    .ghost()
                    .small()
                    .tooltip("Switch the ruler between frames and seconds")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ruler = this.ruler.toggled();
                        cx.notify();
                    })),
            );

        let range = self.inputs.as_ref().map(|inputs| {
            h_flex()
                .gap_1p5()
                .items_center()
                .child(caption("Range", cx))
                .child(div().w(px(84.0)).child(NumberInput::new(&inputs.start).small()))
                .child(caption("to", cx))
                .child(div().w(px(84.0)).child(NumberInput::new(&inputs.end).small()))
                .child(
                    Button::new("tl-range-reset")
                        .icon(IconName::Refresh)
                        .ghost()
                        .small()
                        .tooltip("Reset the play range to the whole clip")
                        .on_click(self.editor_click(|e, _w, cx| {
                            let last = e.animation.last_frame();
                            e.set_play_range_start(0, cx);
                            e.set_play_range_end(last, cx);
                        })),
                )
        });

        let zoom = h_flex()
            .gap_0p5()
            .items_center()
            .child(
                Button::new("tl-zoom-out")
                    .icon(IconName::ZoomOut)
                    .ghost()
                    .small()
                    .tooltip("Zoom out")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let anchor = this.canvas_size.width * 0.5;
                        this.view.zoom_at(1.0 / 1.4, anchor);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("tl-zoom-in")
                    .icon(IconName::ZoomIn)
                    .ghost()
                    .small()
                    .tooltip("Zoom in")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let anchor = this.canvas_size.width * 0.5;
                        this.view.zoom_at(1.4, anchor);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("tl-fit")
                    .icon(IconName::Maximize)
                    .ghost()
                    .small()
                    .tooltip("Fit the play range (F)")
                    .on_click(cx.listener(|this, _, _, cx| this.fit_to_range(cx))),
            );

        h_flex()
            .w_full()
            .flex_shrink_0()
            .h(px(38.0))
            .px_2()
            .gap_1p5()
            .items_center()
            .overflow_hidden()
            .bg(theme.secondary)
            .border_b_1()
            .border_color(theme.border)
            .child(transport)
            .child(playback_options)
            .child(toolbar_divider(cx))
            .children(timecode)
            .child(toolbar_divider(cx))
            .child(editing)
            .child(div().flex_1().min_w_0())
            .child(view_options)
            .child(toolbar_divider(cx))
            .children(range)
            .child(toolbar_divider(cx))
            .child(zoom)
            .into_any_element()
    }

    pub(super) fn render_gutter(&self, rows: &[usize], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(editor) = self.editor.upgrade() else {
            return div().into_any_element();
        };
        let e = editor.read(cx);
        let total_keys = e.animation.key_count();

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
            .child(Tag::secondary().small().rounded_full().child(rows.len().to_string()));

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
                    .child(total_keys.to_string()),
            );

        let mut list = div()
            .absolute()
            .left_0()
            .right_0()
            .top(px(-self.view.scroll_y))
            .flex()
            .flex_col();
        for (i, &ti) in rows.iter().enumerate() {
            let Some(track) = e.animation.tracks.get(ti) else { continue };
            let name = e
                .skeleton
                .bone(&track.bone_id)
                .map_or_else(|| track.bone_id.clone(), |b| b.name.clone());
            let selected = e.selected_bone.as_deref() == Some(track.bone_id.as_str());
            let keyed_here = e.keyframe_at_playhead(&track.bone_id).is_some();
            let key_count = track.keyframes.len();
            let bone_id = track.bone_id.clone();
            let key_bone = track.bone_id.clone();
            let editor_row = self.editor.clone();
            let editor_key = self.editor.clone();
            let stripe = i % 2 == 1;

            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("timeline-track-{}", track.bone_id)))
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
                        let _ = editor_row.update(cx, |e, cx| e.select_bone(Some(bone_id), window, cx));
                    })
                    .child(
                        Icon::new(IconName::Component)
                            .size(px(12.0))
                            .text_color(if selected { theme.primary } else { theme.info.opacity(0.8) }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                            .text_color(theme.foreground)
                            .child(name),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .font_family(MONO)
                            .text_color(theme.muted_foreground)
                            .child(key_count.to_string()),
                    )
                    .child(
                        Button::new(SharedString::from(format!("tl-key-{key_bone}")))
                            .icon(if keyed_here { IconName::Keyframe } else { IconName::KeyframePlus })
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
                                let _ = editor_key.update(cx, |e, cx| e.toggle_key_for_bone(&bone, window, cx));
                            }),
                    ),
            );
        }

        v_flex()
            .w(px(GUTTER_WIDTH))
            .h_full()
            .flex_shrink_0()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(header)
            .child(summary)
            .child(
                div()
                    .id("timeline-gutter-scroll")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _w, cx| {
                        let dy = match event.delta {
                            ScrollDelta::Lines(p) => p.y * 16.0,
                            ScrollDelta::Pixels(p) => p.y.as_f32(),
                        };
                        this.view.scroll_y -= dy;
                        if let Some(editor) = this.editor.upgrade() {
                            let rows = this.visible_rows(editor.read(cx)).len();
                            this.clamp_scroll(rows);
                        }
                        cx.notify();
                    }))
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
            .into_any_element()
    }

    pub(super) fn render_status_bar(&self, rows: &[usize], cx: &mut Context<Self>) -> AnyElement {
        let Some(editor) = self.editor.upgrade() else {
            return div().into_any_element();
        };
        let e = editor.read(cx);
        let selected = e.selected_keyframes.len();
        let fps = e.animation.fps_clamped();
        let mono = |text: String| div().font_family(MONO).child(text);

        StatusBar::new()
            .left(format!(
                "{} {} · {} keys{}",
                rows.len(),
                if rows.len() == 1 { "track" } else { "tracks" },
                e.animation.key_count(),
                if selected > 0 { format!(" · {selected} selected") } else { String::new() },
            ))
            .child(mono(format!(
                "Frame {} / {}   {:.3}s   {:.0} fps",
                e.current_frame(),
                e.animation.last_frame(),
                e.playback.time,
                fps,
            )))
            .right(
                Tag::secondary()
                    .small()
                    .child(if self.snap { "Snap on" } else { "Snap off" }),
            )
            .right(
                Tag::secondary()
                    .small()
                    .child(if e.playback.looping { "Loop" } else { "One-shot" }),
            )
            .right(mono(format!("Zoom {:.0}%", self.view.zoom * 100.0)))
            .into_any_element()
    }
}

fn format_speed(speed: f32) -> String {
    if (speed - speed.round()).abs() < 1e-3 {
        format!("{}", speed.round() as i32)
    } else {
        format!("{speed}")
    }
}

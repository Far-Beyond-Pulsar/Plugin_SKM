//! Chrome around the timeline canvas: transport toolbar and the status bar (the track gutter is its own
//! view, see `gutter.rs`). Everything here is stock engine components.

use gpui::*;
use ui::button::{Button, ButtonVariants};
use ui::input::NumberInput;
use ui::status_bar::StatusBar;
use ui::tag::Tag;
use ui::{h_flex, ActiveTheme, Disableable, IconName, Selectable, Sizable};

use crate::editor::panel::SkeletalAnimEditorPanel;
use crate::ui_components::chrome::{caption, toolbar_divider, MONO};

use super::layout::*;
use super::panel::TimelinePanel;

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
                    .tooltip(if looping {
                        "Looping — click for one-shot"
                    } else {
                        "One-shot — click to loop"
                    })
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
                        NumberInput::new(&inputs.frame).small().suffix(
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
                        let (gutter, on) = (this.gutter.clone(), this.only_selected);
                        gutter.update(cx, |g, cx| g.set_only_selected(on, cx));
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
                if selected > 0 {
                    format!(" · {selected} selected")
                } else {
                    String::new()
                },
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

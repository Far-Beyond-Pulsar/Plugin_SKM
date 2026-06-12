//! `TimelinePanel` — dock panel hosting the keyframe timeline.
//!
//! A GPUI gutter on the left lists animated bone tracks; a WGPU canvas to
//! the right draws the time ruler, track row backgrounds, keyframe
//! diamonds, and the playhead. Click-drag in the canvas scrubs the
//! playhead; clicking a diamond selects that keyframe.

use crate::editor::panel::SkeletalAnimEditorPanel;
use gpui::prelude::FluentBuilder;
use gpui::*;
use ui::PixelsExt;
use ui::{dock::PanelEvent, ActiveTheme};

use super::renderer::TimelineRenderer;
use super::types::{RectInstance, TimelineUniforms};

const GUTTER_WIDTH: f32 = 160.0;
const ROW_HEIGHT: f32 = 28.0;
const RULER_HEIGHT: f32 = 24.0;
const PX_PER_SEC: f32 = 150.0;
const KEYFRAME_SIZE: f32 = 10.0;
const HIT_RADIUS_PX: f32 = 8.0;

const RULER_BG: [f32; 4] = [0.13, 0.13, 0.15, 1.0];
const ROW_BG_EVEN: [f32; 4] = [0.16, 0.16, 0.18, 1.0];
const ROW_BG_ODD: [f32; 4] = [0.13, 0.13, 0.15, 1.0];
const ROW_BG_SELECTED: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const KEYFRAME_COLOR: [f32; 4] = [0.55, 0.75, 1.0, 1.0];
const KEYFRAME_SELECTED_COLOR: [f32; 4] = [1.0, 0.65, 0.15, 1.0];
const PLAYHEAD_COLOR: [f32; 4] = [0.95, 0.30, 0.30, 1.0];

pub struct TimelinePanel {
    editor: WeakEntity<SkeletalAnimEditorPanel>,
    focus_handle: FocusHandle,
    renderer: TimelineRenderer,
    surface: Option<WgpuSurfaceHandle>,
    scroll_x: f32,
    dragging_playhead: bool,
    last_origin: Point<f32>,
    last_size: Size<f32>,
}

impl TimelinePanel {
    pub fn new(editor: WeakEntity<SkeletalAnimEditorPanel>, cx: &mut Context<Self>) -> Self {
        Self {
            editor,
            focus_handle: cx.focus_handle(),
            renderer: TimelineRenderer::new(),
            surface: None,
            scroll_x: 0.0,
            dragging_playhead: false,
            last_origin: Point::new(0.0, 0.0),
            last_size: Size::new(1.0, 1.0),
        }
    }

    /// Convert a canvas-relative x coordinate (pixels) to a clip time.
    fn x_to_time(&self, x: f32) -> f32 {
        ((x + self.scroll_x) / PX_PER_SEC).max(0.0)
    }

    fn handle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            return;
        }
        let Some(editor) = self.editor.upgrade() else {
            return;
        };

        let x = event.position.x.as_f32() - self.last_origin.x - GUTTER_WIDTH;
        let y = event.position.y.as_f32() - self.last_origin.y;
        if x < 0.0 {
            return;
        }

        // Hit-test keyframe diamonds before falling back to playhead scrub.
        let scroll_x = self.scroll_x;
        let hit = editor.update(cx, |editor, _cx| {
            let mut found = None;
            for (i, track) in editor.animation.tracks.iter().enumerate() {
                let row_y = RULER_HEIGHT + i as f32 * ROW_HEIGHT;
                if y < row_y || y > row_y + ROW_HEIGHT {
                    continue;
                }
                for (k, kf) in track.keyframes.iter().enumerate() {
                    let kf_x = kf.time * PX_PER_SEC - scroll_x;
                    let kf_y = row_y + ROW_HEIGHT * 0.5;
                    let dx = kf_x - x;
                    let dy = kf_y - (y);
                    if (dx * dx + dy * dy).sqrt() < HIT_RADIUS_PX + KEYFRAME_SIZE * 0.5 {
                        found = Some((track.bone_id.clone(), k));
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            found
        });

        if let Some((bone_id, index)) = hit {
            editor.update(cx, |editor, cx| {
                editor.select_bone(Some(bone_id.clone()), window, cx);
                editor.select_keyframe(Some((bone_id, index)), window, cx);
            });
            return;
        }

        // Otherwise scrub the playhead to the clicked time.
        self.dragging_playhead = true;
        let time = self.x_to_time(x);
        editor.update(cx, |editor, cx| editor.seek(time, cx));
        cx.notify();
    }

    fn handle_mouse_up(&mut self) {
        self.dragging_playhead = false;
    }

    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.dragging_playhead {
            return;
        }
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let x = event.position.x.as_f32() - self.last_origin.x - GUTTER_WIDTH;
        let time = self.x_to_time(x);
        editor.update(cx, |editor, cx| editor.seek(time, cx));
        cx.notify();
    }

    fn handle_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = match event.delta {
            ScrollDelta::Lines(p) => {
                if p.x.abs() > p.y.abs() {
                    p.x
                } else {
                    p.y
                }
            }
            ScrollDelta::Pixels(p) => {
                if p.x.abs() > p.y.abs() {
                    p.x.as_f32()
                } else {
                    p.y.as_f32()
                }
            }
        };
        self.scroll_x = (self.scroll_x - delta * 16.0).max(0.0);
        cx.notify();
    }

    /// Build the instanced rectangles for ruler, row backgrounds, keyframe
    /// diamonds, and the playhead.
    fn build_rects(
        &self,
        editor: &SkeletalAnimEditorPanel,
        canvas_width: f32,
    ) -> Vec<RectInstance> {
        let mut rects = Vec::new();

        rects.push(RectInstance {
            pos: [0.0, 0.0],
            size: [canvas_width, RULER_HEIGHT],
            color: RULER_BG,
            kind: 2,
            _pad: [0; 3],
        });

        let selected_bone = editor.selected_bone.as_deref();
        let selected_kf = editor.selected_keyframe.as_ref();

        for (i, track) in editor.animation.tracks.iter().enumerate() {
            let row_y = RULER_HEIGHT + i as f32 * ROW_HEIGHT;
            let is_selected_row = selected_bone == Some(track.bone_id.as_str());
            let bg = if is_selected_row {
                ROW_BG_SELECTED
            } else if i % 2 == 0 {
                ROW_BG_EVEN
            } else {
                ROW_BG_ODD
            };
            rects.push(RectInstance {
                pos: [0.0, row_y],
                size: [canvas_width, ROW_HEIGHT],
                color: bg,
                kind: 2,
                _pad: [0; 3],
            });

            for (k, kf) in track.keyframes.iter().enumerate() {
                let is_selected = selected_kf == Some(&(track.bone_id.clone(), k));
                let cx_ = kf.time * PX_PER_SEC;
                let cy_ = row_y + ROW_HEIGHT * 0.5;
                rects.push(RectInstance {
                    pos: [cx_ - KEYFRAME_SIZE * 0.5, cy_ - KEYFRAME_SIZE * 0.5],
                    size: [KEYFRAME_SIZE, KEYFRAME_SIZE],
                    color: if is_selected {
                        KEYFRAME_SELECTED_COLOR
                    } else {
                        KEYFRAME_COLOR
                    },
                    kind: 1,
                    _pad: [0; 3],
                });
            }
        }

        let total_height = RULER_HEIGHT + editor.animation.tracks.len() as f32 * ROW_HEIGHT;
        let playhead_x = editor.playback.time * PX_PER_SEC;
        rects.push(RectInstance {
            pos: [playhead_x - 1.0, 0.0],
            size: [2.0, total_height],
            color: PLAYHEAD_COLOR,
            kind: 0,
            _pad: [0; 3],
        });

        rects
    }
}

impl EventEmitter<PanelEvent> for TimelinePanel {}

impl Focusable for TimelinePanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ui::dock::Panel for TimelinePanel {
    fn panel_name(&self) -> &'static str {
        "skeletal-timeline"
    }

    fn title(&self, _window: &Window, _cx: &App) -> AnyElement {
        "Timeline".into_any_element()
    }
}

impl Render for TimelinePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(editor_entity) = self.editor.upgrade() else {
            return div().size_full().child("No editor").into_any_element();
        };

        let canvas_width = (self.last_size.width - GUTTER_WIDTH).max(1.0);
        let rects = {
            let editor = editor_entity.read(cx);
            self.build_rects(editor, canvas_width)
        };

        // Gutter: ruler spacer + one row per animated track, naming the bone.
        let editor_ref = editor_entity.read(cx);
        let theme = cx.theme().clone();
        let mut gutter = div()
            .flex()
            .flex_col()
            .w(px(GUTTER_WIDTH))
            .h_full()
            .flex_shrink_0()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border);
        gutter = gutter.child(
            div()
                .h(px(RULER_HEIGHT))
                .w_full()
                .border_b_1()
                .border_color(theme.border),
        );
        for track in &editor_ref.animation.tracks {
            let name = editor_ref
                .skeleton
                .bone(&track.bone_id)
                .map(|b| b.name.clone())
                .unwrap_or_else(|| track.bone_id.clone());
            let is_selected = editor_ref.selected_bone.as_deref() == Some(track.bone_id.as_str());
            let bone_id = track.bone_id.clone();
            let editor_weak = self.editor.clone();
            gutter = gutter.child(
                div()
                    .id(SharedString::from(format!(
                        "timeline-track-{}",
                        track.bone_id
                    )))
                    .h(px(ROW_HEIGHT))
                    .w_full()
                    .flex()
                    .items_center()
                    .px_2()
                    .text_sm()
                    .when(is_selected, |d| {
                        d.bg(theme.accent).text_color(theme.accent_foreground)
                    })
                    .when(!is_selected, |d| d.text_color(theme.foreground))
                    .border_b_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        let bone_id = bone_id.clone();
                        let _ = editor_weak.update(cx, |editor, cx| {
                            editor.select_bone(Some(bone_id), window, cx)
                        });
                    })
                    .child(name),
            );
        }
        drop(editor_ref);

        let gpu_display: AnyElement = if let Some(ref s) = self.surface {
            wgpu_surface(s.clone())
                .defer_resize_until_mouse_up(true)
                .absolute()
                .inset_0()
                .into_any_element()
        } else {
            div()
                .absolute()
                .inset_0()
                .bg(cx.theme().background)
                .into_any_element()
        };

        let entity = cx.entity().clone();
        let driver = {
            let pre = entity.clone();
            let paint = entity.clone();
            let scroll_x = self.scroll_x;
            gpui::canvas(
                move |bounds, window, cx| {
                    let sw = bounds.size.width.as_f32().max(1.0) as u32;
                    let sh = bounds.size.height.as_f32().max(1.0) as u32;
                    pre.update(cx, |panel, cx| {
                        panel.last_origin = Point::new(
                            bounds.origin.x.as_f32() - GUTTER_WIDTH,
                            bounds.origin.y.as_f32(),
                        );
                        panel.last_size = Size::new(
                            bounds.size.width.as_f32() + GUTTER_WIDTH,
                            bounds.size.height.as_f32(),
                        );
                        if panel.surface.is_none() {
                            if let Some(s) = window.create_wgpu_surface(
                                sw.max(64),
                                sh.max(64),
                                wgpu::TextureFormat::Bgra8UnormSrgb,
                            ) {
                                panel.surface = Some(s);
                                cx.notify();
                            }
                        }
                    });
                },
                move |_bounds, _pre, _window, cx| {
                    paint.update(cx, |panel, cx| {
                        let Some(ref surface) = panel.surface else {
                            return;
                        };
                        if surface.is_resize_pending() {
                            return;
                        }
                        let Some((view, (w, h))) = surface.back_view_with_size() else {
                            return;
                        };

                        let uniforms = TimelineUniforms {
                            viewport: [w as f32, h as f32],
                            scroll_x,
                            px_per_sec: PX_PER_SEC,
                        };

                        panel.renderer.render_frame(
                            surface.device(),
                            surface.queue(),
                            &view,
                            w,
                            h,
                            surface.format(),
                            &uniforms,
                            &rects,
                        );
                        drop(view);
                        surface.swap_buffers();
                        let _ = cx;
                    });
                },
            )
            .absolute()
            .inset_0()
            .size_full()
        };

        let entity_down = entity.clone();
        let entity_up = entity.clone();
        let entity_move = entity.clone();
        let entity_scroll = entity.clone();

        let canvas_area = div()
            .id("skeletal-timeline-canvas")
            .flex_1()
            .h_full()
            .relative()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                move |event: &MouseDownEvent, window, cx| {
                    entity_down.update(cx, |panel, cx| panel.handle_mouse_down(event, window, cx));
                },
            )
            .on_mouse_up(
                MouseButton::Left,
                move |_event: &MouseUpEvent, _window, cx| {
                    entity_up.update(cx, |panel, _cx| panel.handle_mouse_up());
                },
            )
            .on_mouse_move(move |event: &MouseMoveEvent, _window, cx| {
                entity_move.update(cx, |panel, cx| panel.handle_mouse_move(event, cx));
            })
            .on_scroll_wheel(move |event: &ScrollWheelEvent, _window, cx| {
                entity_scroll.update(cx, |panel, cx| panel.handle_scroll(event, cx));
            })
            .child(gpu_display)
            .child(driver);

        div()
            .size_full()
            .flex()
            .flex_row()
            .child(gutter)
            .child(canvas_area)
            .into_any_element()
    }
}

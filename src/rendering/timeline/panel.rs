//! `TimelinePanel` — dock panel hosting the keyframe timeline.
//!
//! A GPUI gutter on the left lists animated bone tracks; a WGPU canvas to
//! the right draws the time ruler, track row backgrounds, keyframe
//! diamonds, and the playhead. Click-drag in the canvas scrubs the
//! playhead; clicking a diamond selects that keyframe.

use std::collections::HashSet;

use crate::editor::panel::SkeletalAnimEditorPanel;
use gpui::prelude::FluentBuilder;
use gpui::*;
use ui::input::{InputEvent, InputState, NumberInput};
use ui::PixelsExt;
use ui::{dock::PanelEvent, h_flex, ActiveTheme};

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
const TICK_LINE_COLOR: [f32; 4] = [0.40, 0.40, 0.45, 1.0];
const TICK_LABEL_COLOR: [f32; 4] = [0.75, 0.75, 0.78, 1.0];
const CURRENT_FRAME_BG: [f32; 4] = [0.95, 0.30, 0.30, 1.0];
const CURRENT_FRAME_LABEL_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const BOX_SELECT_COLOR: [f32; 4] = [0.35, 0.55, 0.95, 0.25];
const BOX_SELECT_BORDER: [f32; 4] = [0.55, 0.75, 1.0, 0.9];
const BOX_SELECT_BORDER_PX: f32 = 1.0;
/// Fill color for the diagonally-hatched, out-of-play-range track area.
const HATCH_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.35];
/// How far the hatch overlay extends past the play range, in screen px, to
/// cover any amount of scroll/zoom.
const HATCH_EXTENT: f32 = 50_000.0;

/// Width/height of a single "pixel" in the bitmap digit font, in screen px.
const DIGIT_CELL: f32 = 2.0;
/// Digits are laid out on a 3 (wide) x 5 (tall) grid, MSB-first per row.
const DIGIT_PATTERNS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111], // 0
    [0b010, 0b110, 0b010, 0b010, 0b111], // 1
    [0b111, 0b001, 0b111, 0b100, 0b111], // 2
    [0b111, 0b001, 0b111, 0b001, 0b111], // 3
    [0b101, 0b101, 0b111, 0b001, 0b001], // 4
    [0b111, 0b100, 0b111, 0b001, 0b111], // 5
    [0b111, 0b100, 0b111, 0b101, 0b111], // 6
    [0b111, 0b001, 0b001, 0b001, 0b001], // 7
    [0b111, 0b101, 0b111, 0b101, 0b111], // 8
    [0b111, 0b101, 0b111, 0b001, 0b111], // 9
];
/// Width of a single digit glyph, in screen px.
const DIGIT_WIDTH: f32 = 3.0 * DIGIT_CELL;
/// Height of a single digit glyph, in screen px.
const DIGIT_HEIGHT: f32 = 5.0 * DIGIT_CELL;
/// Horizontal gap between digits of a multi-digit number, in screen px.
const DIGIT_GAP: f32 = DIGIT_CELL;

/// Push the rects for a single digit glyph (0-9) at `(x, y)`.
fn push_digit(rects: &mut Vec<RectInstance>, x: f32, y: f32, digit: usize, color: [f32; 4], kind: u32) {
    let pattern = DIGIT_PATTERNS[digit];
    for (row, bits) in pattern.iter().enumerate() {
        for col in 0..3 {
            if bits & (1 << (2 - col)) != 0 {
                rects.push(RectInstance {
                    pos: [x + col as f32 * DIGIT_CELL, y + row as f32 * DIGIT_CELL],
                    size: [DIGIT_CELL, DIGIT_CELL],
                    color,
                    kind,
                    _pad: [0; 3],
                });
            }
        }
    }
}

/// Push the rect for a minus-sign glyph (a single bar on the middle row) at
/// `(x, y)`, matching the digit grid's dimensions.
fn push_minus(rects: &mut Vec<RectInstance>, x: f32, y: f32, color: [f32; 4], kind: u32) {
    rects.push(RectInstance {
        pos: [x, y + 2.0 * DIGIT_CELL],
        size: [DIGIT_WIDTH, DIGIT_CELL],
        color,
        kind,
        _pad: [0; 3],
    });
}

/// Push the rects for an integer (negative numbers get a leading minus
/// sign) at `(x, y)`. Returns the total width drawn, in screen px.
fn push_number(rects: &mut Vec<RectInstance>, x: f32, y: f32, number: i64, color: [f32; 4], kind: u32) -> f32 {
    let mut cursor = x;
    if number < 0 {
        push_minus(rects, cursor, y, color, kind);
        cursor += DIGIT_WIDTH + DIGIT_GAP;
    }
    let digits: Vec<usize> = number
        .unsigned_abs()
        .to_string()
        .chars()
        .map(|c| c.to_digit(10).unwrap() as usize)
        .collect();
    for d in digits {
        push_digit(rects, cursor, y, d, color, kind);
        cursor += DIGIT_WIDTH + DIGIT_GAP;
    }
    (cursor - DIGIT_GAP - x).max(0.0)
}

/// Width, in screen px, that [`push_number`] would draw for `number`.
fn number_width(number: i64) -> f32 {
    let digit_count = number.unsigned_abs().to_string().len() as f32;
    let sign_count = if number < 0 { 1.0 } else { 0.0 };
    let glyph_count = digit_count + sign_count;
    glyph_count * DIGIT_WIDTH + (glyph_count - 1.0).max(0.0) * DIGIT_GAP
}

/// Lazily-created inputs for the playback range top bar, plus the
/// subscriptions that push edits back into the editor's `play_range`.
struct RangeInputs {
    start: Entity<InputState>,
    end: Entity<InputState>,
    _subscriptions: [Subscription; 2],
}

pub struct TimelinePanel {
    editor: WeakEntity<SkeletalAnimEditorPanel>,
    focus_handle: FocusHandle,
    renderer: TimelineRenderer,
    surface: Option<WgpuSurfaceHandle>,
    scroll_x: f32,
    zoom: f32,
    dragging_playhead: bool,
    panning: bool,
    pan_last_x: f32,
    /// Anchor point (canvas-relative) of an in-progress box-select drag.
    box_select_start: Option<(f32, f32)>,
    /// Current point (canvas-relative) of an in-progress box-select drag.
    box_select_current: Option<(f32, f32)>,
    /// Playback-range start/end frame inputs shown in the top bar,
    /// initialized lazily on first render once a `Window` is available.
    range_inputs: Option<RangeInputs>,
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
            zoom: 1.0,
            dragging_playhead: false,
            panning: false,
            pan_last_x: 0.0,
            box_select_start: None,
            box_select_current: None,
            range_inputs: None,
            last_origin: Point::new(0.0, 0.0),
            last_size: Size::new(1.0, 1.0),
        }
    }

    /// Create the playback-range start/end inputs and wire them up to push
    /// edits back into `editor.play_range`. Called once a `Window` is
    /// available, on first render.
    fn init_range_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let (start, end) = editor.read(cx).play_range;

        let start_input =
            cx.new(|cx| InputState::new(window, cx).default_value(start.to_string()));
        let end_input = cx.new(|cx| InputState::new(window, cx).default_value(end.to_string()));

        let editor_weak = self.editor.clone();
        let sub_start = cx.subscribe_in(&start_input, window, {
            let editor_weak = editor_weak.clone();
            move |_this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    if let Ok(value) = input.read(cx).value().parse::<i32>() {
                        if let Some(editor) = editor_weak.upgrade() {
                            editor.update(cx, |editor, cx| {
                                editor.set_play_range_start(value, cx)
                            });
                        }
                        window.refresh();
                    }
                }
            }
        });
        let sub_end = cx.subscribe_in(&end_input, window, move |_this, input, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Change) {
                if let Ok(value) = input.read(cx).value().parse::<i32>() {
                    if let Some(editor) = editor_weak.upgrade() {
                        editor.update(cx, |editor, cx| editor.set_play_range_end(value, cx));
                    }
                    window.refresh();
                }
            }
        });

        self.range_inputs = Some(RangeInputs {
            start: start_input,
            end: end_input,
            _subscriptions: [sub_start, sub_end],
        });
    }

    /// Current horizontal scale, in pixels per second of clip time.
    fn px_per_sec(&self) -> f32 {
        PX_PER_SEC * self.zoom
    }

    /// Convert a canvas-relative x coordinate (pixels) to a clip time. May
    /// be negative if the view has been panned to show time before 0;
    /// `Editor::seek` clamps this back into the clip's valid range.
    fn x_to_time(&self, x: f32) -> f32 {
        (x + self.scroll_x) / self.px_per_sec()
    }

    /// Frame-spacing steps tried (in frames) when picking ruler tick
    /// intervals, smallest first.
    const FRAME_STEPS: [f32; 12] = [
        1.0, 2.0, 5.0, 10.0, 15.0, 24.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1500.0,
    ];

    /// Tick spacing (in frames) that keeps ruler labels at least ~60px apart.
    fn tick_interval_frames(px_per_frame: f32) -> f32 {
        for &step in &Self::FRAME_STEPS {
            if step * px_per_frame >= 60.0 {
                return step;
            }
        }
        *Self::FRAME_STEPS.last().unwrap()
    }

    /// Compute ruler tick positions visible across `canvas_width`, as
    /// `(world-space x in px, frame number)`. `fps` is the clip's frame rate.
    fn build_frame_ticks(&self, fps: f32, canvas_width: f32) -> Vec<(f32, i32)> {
        let px_per_sec = self.px_per_sec();
        let fps = fps.max(1.0);
        let px_per_frame = px_per_sec / fps;
        let step = Self::tick_interval_frames(px_per_frame);

        let start_frame = (self.scroll_x / px_per_frame / step).floor() * step;
        let end_frame = (self.scroll_x + canvas_width) / px_per_frame;

        let mut ticks = Vec::new();
        let mut frame = start_frame;
        while frame <= end_frame + step {
            let x = frame / fps * px_per_sec;
            ticks.push((x, frame.round() as i32));
            frame += step;
        }
        ticks
    }

    fn handle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button == MouseButton::Middle {
            self.panning = true;
            self.pan_last_x = event.position.x.as_f32();
            return;
        }
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

        // Clicking anywhere in the ruler moves the playhead to that time.
        if y < RULER_HEIGHT {
            self.dragging_playhead = true;
            let time = self.x_to_time(x);
            editor.update(cx, |editor, cx| editor.seek(time, cx));
            cx.notify();
            return;
        }

        // Hit-test keyframe diamonds.
        let scroll_x = self.scroll_x;
        let px_per_sec = self.px_per_sec();
        let hit = editor.update(cx, |editor, _cx| {
            let mut found = None;
            for (i, track) in editor.animation.tracks.iter().enumerate() {
                let row_y = RULER_HEIGHT + i as f32 * ROW_HEIGHT;
                if y < row_y || y > row_y + ROW_HEIGHT {
                    continue;
                }
                for (k, kf) in track.keyframes.iter().enumerate() {
                    let kf_x = kf.time * px_per_sec - scroll_x;
                    let kf_y = row_y + ROW_HEIGHT * 0.5;
                    let dx = kf_x - x;
                    let dy = kf_y - y;
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
                if event.modifiers.shift {
                    editor.toggle_keyframe_selection((bone_id, index), window, cx);
                } else {
                    editor.select_bone(Some(bone_id.clone()), window, cx);
                    editor.select_keyframe(Some((bone_id, index)), window, cx);
                }
            });
            return;
        }

        // Otherwise, start a box-select drag over the track area. A plain
        // click with no drag (handled on mouse-up) clears the selection.
        self.box_select_start = Some((x, y));
        self.box_select_current = Some((x, y));
        cx.notify();
    }

    fn handle_mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.dragging_playhead = false;
        self.panning = false;

        let Some(start) = self.box_select_start.take() else {
            return;
        };
        let current = self.box_select_current.take().unwrap_or(start);

        let dragged = (current.0 - start.0).abs() > 2.0 || (current.1 - start.1).abs() > 2.0;
        let additive = event.modifiers.shift;

        let Some(editor) = self.editor.upgrade() else {
            return;
        };

        if !dragged {
            if !additive {
                editor.update(cx, |editor, cx| {
                    editor.set_selected_keyframes(HashSet::new(), false, window, cx);
                });
            }
            cx.notify();
            return;
        }

        let min_x = start.0.min(current.0);
        let max_x = start.0.max(current.0);
        let min_y = start.1.min(current.1);
        let max_y = start.1.max(current.1);
        let scroll_x = self.scroll_x;
        let px_per_sec = self.px_per_sec();

        let hits: HashSet<(String, usize)> = editor
            .read(cx)
            .animation
            .tracks
            .iter()
            .enumerate()
            .flat_map(|(i, track)| {
                let row_y = RULER_HEIGHT + i as f32 * ROW_HEIGHT;
                let kf_y = row_y + ROW_HEIGHT * 0.5;
                let bone_id = track.bone_id.clone();
                track
                    .keyframes
                    .iter()
                    .enumerate()
                    .filter(move |(_, kf)| {
                        let kf_x = kf.time * px_per_sec - scroll_x;
                        kf_x >= min_x && kf_x <= max_x && kf_y >= min_y && kf_y <= max_y
                    })
                    .map(move |(k, _)| (bone_id.clone(), k))
                    .collect::<Vec<_>>()
            })
            .collect();

        editor.update(cx, |editor, cx| {
            editor.set_selected_keyframes(hits, additive, window, cx);
        });
        cx.notify();
    }

    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.panning {
            let x = event.position.x.as_f32();
            let delta_x = x - self.pan_last_x;
            self.pan_last_x = x;
            self.scroll_x -= delta_x;
            cx.notify();
            return;
        }

        if self.box_select_start.is_some() {
            let x = event.position.x.as_f32() - self.last_origin.x - GUTTER_WIDTH;
            let y = event.position.y.as_f32() - self.last_origin.y;
            self.box_select_current = Some((x, y));
            cx.notify();
            return;
        }

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

        if event.modifiers.control || event.modifiers.platform {
            // Ctrl/cmd+scroll pans horizontally.
            self.scroll_x -= delta * 16.0;
        } else {
            // Plain scroll zooms, anchored at the cursor so the time under
            // it stays put.
            let cursor_x = event.position.x.as_f32() - self.last_origin.x - GUTTER_WIDTH;
            let time_at_cursor = self.x_to_time(cursor_x);
            let factor = if delta > 0.0 { 1.1 } else { 1.0 / 1.1 };
            self.zoom = (self.zoom * factor).clamp(0.1, 8.0);
            self.scroll_x = time_at_cursor * self.px_per_sec() - cursor_x;
        }
        cx.notify();
    }

    /// Build the instanced rectangles for ruler, row backgrounds, keyframe
    /// diamonds, and the playhead.
    fn build_rects(
        &self,
        editor: &SkeletalAnimEditorPanel,
        canvas_width: f32,
    ) -> Vec<RectInstance> {
        let px_per_sec = self.px_per_sec();
        let fps = editor.animation.fps.max(1.0);
        let mut rects = Vec::new();

        rects.push(RectInstance {
            pos: [0.0, 0.0],
            size: [canvas_width, RULER_HEIGHT],
            color: RULER_BG,
            kind: 2,
            _pad: [0; 3],
        });

        let selected_bone = editor.selected_bone.as_deref();
        let selected_keyframes = &editor.selected_keyframes;

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
                let is_selected = selected_keyframes.contains(&(track.bone_id.clone(), k));
                let cx_ = kf.time * px_per_sec;
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

        // Grey out + diagonally hatch the visible track area outside the
        // playback range, drawn over the row backgrounds/keyframes. Computed
        // in screen space (kind 2 = fixed) so it stays within the canvas
        // bounds regardless of scroll/zoom.
        let track_area_height = editor.animation.tracks.len() as f32 * ROW_HEIGHT;
        if track_area_height > 0.0 {
            let (range_start, range_end) = editor.play_range;
            let x_range_start = range_start as f32 / fps * px_per_sec;
            let x_range_end_excl = (range_end as f32 + 1.0) / fps * px_per_sec;
            let screen_range_start = x_range_start - self.scroll_x;
            let screen_range_end_excl = x_range_end_excl - self.scroll_x;

            let left_w = screen_range_start.clamp(0.0, canvas_width);
            if left_w > 0.0 {
                rects.push(RectInstance {
                    pos: [0.0, RULER_HEIGHT],
                    size: [left_w, track_area_height],
                    color: HATCH_COLOR,
                    kind: 3,
                    _pad: [0; 3],
                });
            }

            let right_start = screen_range_end_excl.clamp(0.0, canvas_width);
            let right_w = canvas_width - right_start;
            if right_w > 0.0 {
                rects.push(RectInstance {
                    pos: [right_start, RULER_HEIGHT],
                    size: [right_w, track_area_height],
                    color: HATCH_COLOR,
                    kind: 3,
                    _pad: [0; 3],
                });
            }
        }

        let total_height = RULER_HEIGHT + editor.animation.tracks.len() as f32 * ROW_HEIGHT;
        let playhead_x = editor.playback.time * px_per_sec;
        rects.push(RectInstance {
            pos: [playhead_x - 1.0, 0.0],
            size: [2.0, total_height],
            color: PLAYHEAD_COLOR,
            kind: 0,
            _pad: [0; 3],
        });

        // Frame ruler: tick marks + frame-number labels, drawn directly with
        // the bitmap digit font so they scroll/zoom with the timeline.
        let current_frame = (editor.playback.time * fps).round() as i32;

        for (x, frame) in self.build_frame_ticks(fps, canvas_width) {
            if frame == current_frame {
                continue;
            }
            rects.push(RectInstance {
                pos: [x, RULER_HEIGHT - 6.0],
                size: [1.0, 6.0],
                color: TICK_LINE_COLOR,
                kind: 0,
                _pad: [0; 3],
            });
            push_number(&mut rects, x + 3.0, 4.0, frame as i64, TICK_LABEL_COLOR, 0);
        }

        // Highlight the current frame, Blender-Dope-Sheet style.
        {
            let label_w = number_width(current_frame as i64).max(DIGIT_WIDTH);
            let box_w = label_w + 6.0;
            let cf_x = current_frame as f32 / fps * px_per_sec;
            rects.push(RectInstance {
                pos: [cf_x - box_w * 0.5, 2.0],
                size: [box_w, RULER_HEIGHT - 4.0],
                color: CURRENT_FRAME_BG,
                kind: 0,
                _pad: [0; 3],
            });
            push_number(
                &mut rects,
                cf_x - label_w * 0.5,
                (RULER_HEIGHT - DIGIT_HEIGHT) * 0.5,
                current_frame as i64,
                CURRENT_FRAME_LABEL_COLOR,
                0,
            );
        }

        // In-progress box-select marquee, drawn in screen space so it
        // tracks the mouse regardless of scroll.
        if let (Some(start), Some(current)) = (self.box_select_start, self.box_select_current) {
            let min_x = start.0.min(current.0);
            let max_x = start.0.max(current.0);
            let min_y = start.1.min(current.1);
            let max_y = start.1.max(current.1);
            let w = (max_x - min_x).max(1.0);
            let h = (max_y - min_y).max(1.0);

            rects.push(RectInstance {
                pos: [min_x, min_y],
                size: [w, h],
                color: BOX_SELECT_COLOR,
                kind: 2,
                _pad: [0; 3],
            });
            for border in [
                ([min_x, min_y], [w, BOX_SELECT_BORDER_PX]),
                ([min_x, max_y - BOX_SELECT_BORDER_PX], [w, BOX_SELECT_BORDER_PX]),
                ([min_x, min_y], [BOX_SELECT_BORDER_PX, h]),
                ([max_x - BOX_SELECT_BORDER_PX, min_y], [BOX_SELECT_BORDER_PX, h]),
            ] {
                rects.push(RectInstance {
                    pos: border.0,
                    size: border.1,
                    color: BOX_SELECT_BORDER,
                    kind: 2,
                    _pad: [0; 3],
                });
            }
        }

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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(editor_entity) = self.editor.upgrade() else {
            return div().size_full().child("No editor").into_any_element();
        };

        if self.range_inputs.is_none() {
            self.init_range_inputs(window, cx);
        }

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
        let _ = editor_ref;

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
            let px_per_sec = self.px_per_sec();
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
                            px_per_sec,
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
        let entity_down_mid = entity.clone();
        let entity_up = entity.clone();
        let entity_up_mid = entity.clone();
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
            .on_mouse_down(
                MouseButton::Middle,
                move |event: &MouseDownEvent, window, cx| {
                    entity_down_mid
                        .update(cx, |panel, cx| panel.handle_mouse_down(event, window, cx));
                },
            )
            .on_mouse_up(
                MouseButton::Left,
                move |event: &MouseUpEvent, window, cx| {
                    entity_up.update(cx, |panel, cx| panel.handle_mouse_up(event, window, cx));
                },
            )
            .on_mouse_up(
                MouseButton::Middle,
                move |event: &MouseUpEvent, window, cx| {
                    entity_up_mid.update(cx, |panel, cx| panel.handle_mouse_up(event, window, cx));
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

        // Top bar (outside the custom renderer) for editing the inclusive
        // playback frame range, e.g. `-1` to `10` for an 11-frame clip.
        let range_bar: AnyElement = if let Some(range_inputs) = &self.range_inputs {
            h_flex()
                .w_full()
                .flex_shrink_0()
                .items_center()
                .gap_2()
                .px_2()
                .h(px(RULER_HEIGHT))
                .bg(theme.secondary)
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Playback Range"),
                )
                .child(
                    div()
                        .w(px(64.0))
                        .child(NumberInput::new(&range_inputs.start).w_full()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("to"),
                )
                .child(
                    div()
                        .w(px(64.0))
                        .child(NumberInput::new(&range_inputs.end).w_full()),
                )
                .into_any_element()
        } else {
            div().w_full().flex_shrink_0().h(px(RULER_HEIGHT)).into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(range_bar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(gutter)
                    .child(canvas_area),
            )
            .into_any_element()
    }
}

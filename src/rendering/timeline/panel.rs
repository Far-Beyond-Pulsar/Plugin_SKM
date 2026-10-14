//! `TimelinePanel` — the keyframe dope sheet.
//!
//! GPUI draws the chrome (transport toolbar, track gutter, ruler labels,
//! status bar) from the engine's shared components, and one WGPU canvas draws
//! the time grid, track rows, keyframes and playhead. All geometry comes from
//! [`super::layout::Scene`], which is also what hit testing runs against.
//!
//! | Input                     | Action                                   |
//! |---------------------------|------------------------------------------|
//! | Click / drag ruler        | Scrub the playhead (snaps to frames)     |
//! | Click key                 | Select key and park the playhead on it   |
//! | Drag key                  | Move the selected keys (snaps to frames) |
//! | Shift+click key           | Add / remove from the selection          |
//! | Drag empty space          | Marquee select                           |
//! | Scroll                    | Zoom around the cursor                   |
//! | Ctrl+Scroll / MMB drag    | Pan                                      |
//! | Shift+Scroll              | Scroll the track list                    |
//! | Space                     | Play / pause                             |
//! | Left / Right              | Step a frame (Shift = 10)                |
//! | `[` / `]`                 | Previous / next keyframe                 |
//! | K                         | Key the selected bone at the playhead    |
//! | Ctrl+A / C / X / V        | Select all / copy / cut / paste          |
//! | Del                       | Delete selected keys                     |
//! | F                         | Fit the play range                       |
//! | Esc                       | Cancel a key drag                        |

use std::collections::HashSet;

use gpui::*;
use ui::input::{InputEvent, InputState};
use ui::{dock::PanelEvent, ActiveTheme, IconName, PixelsExt, StyledExt};

use crate::editor::panel::{KeyDrag, SkeletalAnimEditorPanel};
use crate::style::Palette;
use crate::ui_components::chrome::{self, MONO};

use super::gutter::TimelineGutter;
use super::layout::*;
use super::renderer::TimelineRenderer;
use super::types::TimelineUniforms;

pub(super) const GUTTER_WIDTH: f32 = 208.0;

/// What a pointer drag started on the canvas is doing.
pub(super) enum DragState {
    None,
    Playhead,
    Pan {
        last_x: f32,
    },
    Marquee {
        start: (f32, f32),
        current: (f32, f32),
        additive: bool,
        /// Plain click on a row (no drag) selects that row's bone.
        row_click: Option<String>,
    },
    Keys {
        drag: KeyDrag,
        anchor_time: f32,
        /// Offset between the grab point and the anchor key, so keys don't jump.
        grab_dt: f32,
        clicked: (String, usize),
        delta_frames: i32,
    },
    Scrollbar {
        grab: f32,
    },
}

/// Inputs in the toolbar, created on first render once a `Window` exists.
pub(super) struct TimelineInputs {
    pub start: Entity<InputState>,
    pub end: Entity<InputState>,
    pub frame: Entity<InputState>,
    pub _subscriptions: Vec<Subscription>,
}

pub struct TimelinePanel {
    pub(super) editor: WeakEntity<SkeletalAnimEditorPanel>,
    pub(super) focus_handle: FocusHandle,
    renderer: TimelineRenderer,
    surface: Option<WgpuSurfaceHandle>,
    pub(super) view: TimelineView,
    pub(super) ruler: RulerMode,
    pub(super) snap: bool,
    pub(super) only_selected: bool,
    pub(super) drag: DragState,
    pub(super) hover: Option<HoverTarget>,
    /// Pointer position over the canvas, for the hover tooltip.
    pointer: Option<(f32, f32)>,
    pub(super) inputs: Option<TimelineInputs>,
    /// Track list: a separate view that refreshes only when what it shows changes.
    pub(super) gutter: Entity<TimelineGutter>,
    pub(super) palette: Palette,
    fit_pending: bool,
    canvas_origin: Point<f32>,
    pub(super) canvas_size: Size<f32>,
}

impl TimelinePanel {
    pub fn new(editor: WeakEntity<SkeletalAnimEditorPanel>, cx: &mut Context<Self>) -> Self {
        let gutter = cx.new(|_| TimelineGutter::new(editor.clone()));
        Self {
            editor,
            focus_handle: cx.focus_handle(),
            renderer: TimelineRenderer::new(),
            surface: None,
            view: TimelineView::default(),
            ruler: RulerMode::Frames,
            snap: true,
            only_selected: false,
            drag: DragState::None,
            hover: None,
            pointer: None,
            inputs: None,
            gutter,
            palette: Palette::from_theme(cx.theme()),
            fit_pending: true,
            canvas_origin: Point::new(0.0, 0.0),
            canvas_size: Size::new(1.0, 1.0),
        }
    }

    // ── Model plumbing ─────────────────────────────────────────────────────

    /// Start the track list following the editor. `state` is the editor's
    /// current state: the editor can't be read through its entity while it is
    /// rendering, which is when its workspace (and this panel) is set up.
    pub fn attach(
        &mut self,
        state: &SkeletalAnimEditorPanel,
        editor: &Entity<SkeletalAnimEditorPanel>,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.weak_entity();
        self.gutter
            .update(cx, |gutter, cx| gutter.attach(state, editor, panel, cx));
    }

    /// Scroll the track rows by `dy` px (from the gutter's wheel handler).
    pub fn scroll_tracks(&mut self, dy: f32, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let rows = self.visible_rows(editor.read(cx)).len();
        self.view.scroll_y += dy;
        self.clamp_scroll(rows);
        cx.notify();
    }

    /// Track indices shown, honoring the "selected bone only" filter.
    pub(super) fn visible_rows(&self, editor: &SkeletalAnimEditorPanel) -> Vec<usize> {
        editor
            .animation
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                !self.only_selected || editor.selected_bone.as_deref() == Some(t.bone_id.as_str())
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn scene<'a>(
        &self,
        editor: &'a SkeletalAnimEditorPanel,
        rows: &'a [usize],
        size: (f32, f32),
    ) -> Scene<'a> {
        let marquee = match &self.drag {
            DragState::Marquee { start, current, .. } => Some(Marquee {
                a: *start,
                b: *current,
            }),
            _ => None,
        };
        let drag_guide = match &self.drag {
            DragState::Keys {
                drag,
                anchor_time,
                delta_frames,
                ..
            } if drag.moved => {
                Some(anchor_time + *delta_frames as f32 / editor.animation.fps_clamped())
                    .map(|t| t.max(0.0))
            }
            _ => None,
        };
        Scene {
            view: self.view,
            size,
            clip: &editor.animation,
            rows,
            selected_bone: editor.selected_bone.as_deref(),
            selected_keys: &editor.selected_keyframes,
            time: editor.playback.time,
            play_range: editor.play_range,
            hover: self.hover,
            marquee,
            ruler: self.ruler,
            drag_guide,
            palette: self.palette,
        }
    }

    fn local(&self, position: Point<Pixels>) -> (f32, f32) {
        (
            position.x.as_f32() - self.canvas_origin.x,
            position.y.as_f32() - self.canvas_origin.y,
        )
    }

    fn snapped(&self, editor: &SkeletalAnimEditorPanel, time: f32) -> f32 {
        if self.snap {
            editor.animation.snap_time(time)
        } else {
            time
        }
    }

    pub(super) fn clamp_scroll(&mut self, row_count: usize) {
        let max_y = TimelineView::max_scroll_y(row_count, self.canvas_size.height);
        self.view.scroll_y = self.view.scroll_y.clamp(0.0, max_y);
        // Allow a little room left of zero so the first frame isn't glued to
        // the gutter, but never scroll into emptiness.
        self.view.scroll_x = self.view.scroll_x.max(-80.0);
    }

    pub(super) fn fit_to_range(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let (start, end) = {
            let e = editor.read(cx);
            (
                e.animation.frame_to_time(e.play_range.0),
                e.animation.frame_to_time(e.play_range.1 + 1),
            )
        };
        self.view.fit(start, end, self.canvas_size.width);
        cx.notify();
    }

    fn scrub(&mut self, x: f32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let time = self.view.x_to_time(x);
        let snapped = self.snapped(editor.read(cx), time);
        editor.update(cx, |editor, cx| editor.seek(snapped, window, cx));
        cx.notify();
    }

    // ── Pointer input ──────────────────────────────────────────────────────

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        let (x, y) = self.local(event.position);
        match event.button {
            MouseButton::Middle => {
                self.drag = DragState::Pan {
                    last_x: event.position.x.as_f32(),
                };
            }
            MouseButton::Left => self.left_down(x, y, event.modifiers, window, cx),
            _ => {}
        }
    }

    fn left_down(
        &mut self,
        x: f32,
        y: f32,
        mods: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let (hit, thumb, bone_for_track) = {
            let e = editor.read(cx);
            let rows = self.visible_rows(e);
            let scene = self.scene(e, &rows, (self.canvas_size.width, self.canvas_size.height));
            let hit = scene.hit_test(x, y);
            let bone = match hit {
                Hit::Key { track, .. } | Hit::Row { track } => {
                    e.animation.tracks.get(track).map(|t| t.bone_id.clone())
                }
                _ => None,
            };
            (hit, scene.scrollbar(), bone)
        };

        match hit {
            Hit::Ruler | Hit::SummaryRow => {
                self.drag = DragState::Playhead;
                self.scrub(x, window, cx);
            }
            Hit::Scrollbar => {
                let (_, _, thumb_x, thumb_w) = thumb;
                let grab = if x >= thumb_x && x <= thumb_x + thumb_w {
                    x - thumb_x
                } else {
                    thumb_w * 0.5
                };
                self.drag = DragState::Scrollbar { grab };
                self.drag_scrollbar(x, cx);
            }
            Hit::SummaryKey { time } => {
                editor.update(cx, |e, cx| {
                    e.select_keys_at_time(time, mods.shift, window, cx)
                });
                if mods.shift {
                    return;
                }
                let anchor = editor.read(cx).selected_keyframes.iter().next().cloned();
                if let Some(anchor) = anchor {
                    self.begin_key_drag(&editor, anchor, x, cx);
                }
            }
            Hit::Key { index, .. } => {
                let Some(bone_id) = bone_for_track else {
                    return;
                };
                let key = (bone_id.clone(), index);
                if mods.shift {
                    editor.update(cx, |e, cx| e.toggle_keyframe_selection(key, window, cx));
                    return;
                }
                let already_selected = editor.read(cx).selected_keyframes.contains(&key);
                if !already_selected {
                    editor.update(cx, |e, cx| {
                        e.select_bone(Some(bone_id.clone()), window, cx);
                        e.select_keyframe(Some(key.clone()), window, cx);
                    });
                }
                self.begin_key_drag(&editor, key, x, cx);
            }
            Hit::Row { .. } | Hit::Empty => {
                self.drag = DragState::Marquee {
                    start: (x, y),
                    current: (x, y),
                    additive: mods.shift,
                    row_click: bone_for_track,
                };
                cx.notify();
            }
        }
    }

    fn begin_key_drag(
        &mut self,
        editor: &Entity<SkeletalAnimEditorPanel>,
        anchor: (String, usize),
        x: f32,
        cx: &mut Context<Self>,
    ) {
        let e = editor.read(cx);
        let Some(drag) = e.begin_key_drag(anchor.clone()) else {
            return;
        };
        let anchor_time = e.key_drag_anchor_time(&drag);
        let grab_dt = self.view.x_to_time(x) - anchor_time;
        self.drag = DragState::Keys {
            drag,
            anchor_time,
            grab_dt,
            clicked: anchor,
            delta_frames: 0,
        };
        cx.notify();
    }

    fn drag_scrollbar(&mut self, x: f32, cx: &mut Context<Self>) {
        let DragState::Scrollbar { grab } = self.drag else {
            return;
        };
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let scroll = {
            let e = editor.read(cx);
            let rows = self.visible_rows(e);
            let scene = self.scene(e, &rows, (self.canvas_size.width, self.canvas_size.height));
            scene.scroll_for_thumb(x - grab)
        };
        self.view.scroll_x = scroll;
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (x, y) = self.local(event.position);

        // The button was released outside the canvas: finish the gesture.
        if !matches!(self.drag, DragState::None) && event.pressed_button.is_none() {
            self.end_drag(event.modifiers, window, cx);
            return;
        }

        match &mut self.drag {
            DragState::None => {
                self.pointer = Some((x, y));
                let Some(editor) = self.editor.upgrade() else {
                    return;
                };
                let hover = {
                    let e = editor.read(cx);
                    let rows = self.visible_rows(e);
                    let scene =
                        self.scene(e, &rows, (self.canvas_size.width, self.canvas_size.height));
                    match scene.hit_test(x, y) {
                        Hit::Key { track, index } => Some(HoverTarget::Key { track, index }),
                        Hit::SummaryKey { time } => Some(HoverTarget::SummaryKey { time }),
                        _ => None,
                    }
                };
                // The tooltip follows the pointer, so repaint while it shows.
                if hover != self.hover || hover.is_some() {
                    self.hover = hover;
                    cx.notify();
                }
            }
            DragState::Pan { last_x } => {
                let px = event.position.x.as_f32();
                self.view.scroll_x -= px - *last_x;
                *last_x = px;
                cx.notify();
            }
            DragState::Marquee { current, .. } => {
                *current = (x, y);
                cx.notify();
            }
            DragState::Playhead => {
                self.autoscroll(x);
                self.scrub(x, window, cx);
            }
            DragState::Keys { .. } => {
                self.autoscroll(x);
                self.drag_keys(x, window, cx);
            }
            DragState::Scrollbar { .. } => self.drag_scrollbar(x, cx),
        }
    }

    /// Nudge the view while dragging near the left/right edge.
    fn autoscroll(&mut self, x: f32) {
        let edge = 28.0;
        if x < edge {
            self.view.scroll_x -= (edge - x).min(24.0) * 0.6;
        } else if x > self.canvas_size.width - edge {
            self.view.scroll_x += (x - (self.canvas_size.width - edge)).min(24.0) * 0.6;
        }
    }

    fn drag_keys(&mut self, x: f32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let mouse_time = self.view.x_to_time(x);
        let snap = self.snap;
        let DragState::Keys {
            drag,
            anchor_time,
            grab_dt,
            delta_frames,
            ..
        } = &mut self.drag
        else {
            return;
        };
        let mut target = mouse_time - *grab_dt;
        let fps = editor.read(cx).animation.fps_clamped();
        if snap {
            target = (target * fps).round() / fps;
        }
        let delta = target - *anchor_time;
        *delta_frames = (delta * fps).round() as i32;
        editor.update(cx, |e, cx| e.apply_key_drag(drag, delta, window, cx));
        cx.notify();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.end_drag(event.modifiers, window, cx);
    }

    fn end_drag(&mut self, mods: Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            self.drag = DragState::None;
            return;
        };
        match std::mem::replace(&mut self.drag, DragState::None) {
            DragState::Keys { drag, clicked, .. } => {
                if drag.moved {
                    editor.update(cx, |e, cx| e.finish_key_drag(drag, cx));
                } else {
                    // A click, not a drag: park the playhead on the key.
                    let (bone_id, index) = clicked;
                    editor.update(cx, |e, cx| {
                        let time = e
                            .animation
                            .track(&bone_id)
                            .and_then(|t| t.keyframes.get(index))
                            .map(|k| k.time);
                        e.select_bone(Some(bone_id.clone()), window, cx);
                        if let Some(time) = time {
                            e.seek(time, window, cx);
                        }
                        e.select_keyframe(Some((bone_id, index)), window, cx);
                    });
                }
            }
            DragState::Marquee {
                start,
                current,
                additive,
                row_click,
            } => {
                let dragged =
                    (current.0 - start.0).abs() > 3.0 || (current.1 - start.1).abs() > 3.0;
                if dragged {
                    let keys = {
                        let e = editor.read(cx);
                        let rows = self.visible_rows(e);
                        let scene =
                            self.scene(e, &rows, (self.canvas_size.width, self.canvas_size.height));
                        scene.keys_in_rect(
                            Marquee {
                                a: start,
                                b: current,
                            }
                            .rect(),
                        )
                    };
                    editor.update(cx, |e, cx| {
                        e.set_selected_keyframes(keys, additive || mods.shift, window, cx)
                    });
                } else {
                    editor.update(cx, |e, cx| {
                        if !(additive || mods.shift) {
                            e.set_selected_keyframes(HashSet::new(), false, window, cx);
                        }
                        if let Some(bone) = row_click {
                            e.select_bone(Some(bone), window, cx);
                        }
                    });
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let (dx, dy, pixels) = match event.delta {
            ScrollDelta::Lines(p) => (p.x, p.y, false),
            ScrollDelta::Pixels(p) => (p.x.as_f32(), p.y.as_f32(), true),
        };
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let row_count = self.visible_rows(editor.read(cx)).len();
        let step = if pixels { 1.0 } else { 16.0 };

        if event.modifiers.shift {
            self.view.scroll_y -= (if dy.abs() > dx.abs() { dy } else { dx }) * step;
            self.clamp_scroll(row_count);
        } else if event.modifiers.secondary() || dx.abs() > dy.abs() {
            // Ctrl+wheel and trackpad sideways swipes pan.
            self.view.scroll_x -= (if dx.abs() > dy.abs() { dx } else { dy }) * step;
        } else {
            let (x, _) = self.local(event.position);
            let factor = if pixels {
                (dy * 0.0025).exp()
            } else {
                1.1_f32.powf(dy)
            };
            self.view.zoom_at(factor, x);
        }
        self.clamp_scroll(row_count);
        cx.notify();
    }

    // ── Keyboard ───────────────────────────────────────────────────────────

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let mods = event.keystroke.modifiers;
        let secondary = mods.secondary();
        let step = if mods.shift { 10 } else { 1 };

        match (event.keystroke.key.as_str(), secondary) {
            ("space", false) | (" ", false) => editor.update(cx, |e, cx| e.toggle_play(cx)),
            ("left", false) => editor.update(cx, |e, cx| e.step_frame(-step, window, cx)),
            ("right", false) => editor.update(cx, |e, cx| e.step_frame(step, window, cx)),
            ("home", false) => editor.update(cx, |e, cx| e.goto_start(window, cx)),
            ("end", false) => editor.update(cx, |e, cx| e.goto_end(window, cx)),
            ("[", false) => editor.update(cx, |e, cx| e.goto_prev_keyframe(window, cx)),
            ("]", false) => editor.update(cx, |e, cx| e.goto_next_keyframe(window, cx)),
            ("k", false) => editor.update(cx, |e, cx| e.insert_keyframe(window, cx)),
            ("delete", false) | ("backspace", false) => {
                editor.update(cx, |e, cx| e.delete_selected_keyframes(window, cx))
            }
            ("a", true) => editor.update(cx, |e, cx| e.select_all_keyframes(window, cx)),
            ("c", true) => editor.update(cx, |e, _| e.copy_selected_keyframes()),
            ("x", true) => editor.update(cx, |e, cx| e.cut_selected_keyframes(window, cx)),
            ("v", true) => editor.update(cx, |e, cx| e.paste_keyframes(window, cx)),
            ("f", false) => self.fit_to_range(cx),
            ("escape", _) => {
                if let DragState::Keys { .. } = self.drag {
                    if let DragState::Keys { drag, .. } =
                        std::mem::replace(&mut self.drag, DragState::None)
                    {
                        editor.update(cx, |e, cx| e.cancel_key_drag(drag, window, cx));
                    }
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    // ── Rendering helpers ──────────────────────────────────────────────────

    /// Ruler labels, the current-frame badge, hover tooltip and drag readout,
    /// drawn with GPUI text so they stay crisp and use the theme font.
    fn render_overlays(
        &self,
        editor: &SkeletalAnimEditorPanel,
        rows: &[usize],
        cx: &App,
    ) -> Vec<AnyElement> {
        let theme = cx.theme();
        let fps = editor.animation.fps_clamped();
        let (w, h) = (self.canvas_size.width, self.canvas_size.height);
        let mut out: Vec<AnyElement> = Vec::new();

        // Current-frame badge.
        let playhead_x = self.view.time_to_x(editor.playback.time);
        let label = match self.ruler {
            RulerMode::Frames => editor.current_frame().to_string(),
            RulerMode::Seconds => format!("{:.2}s", editor.playback.time),
        };
        let badge_w = (label.chars().count() as f32 * 7.4 + 14.0).max(30.0);
        let badge_l = playhead_x - badge_w * 0.5;

        for tick in ruler_ticks(&self.view, fps, w, self.ruler) {
            let Some(text) = tick.label else { continue };
            let x = tick.x - self.view.scroll_x;
            if x < -40.0 || x > w + 4.0 {
                continue;
            }
            // Don't draw a label underneath the playhead badge.
            if x + 4.0 < badge_l + badge_w + 6.0
                && x + 4.0 + text.len() as f32 * 7.0 > badge_l - 6.0
            {
                continue;
            }
            out.push(
                div()
                    .absolute()
                    .left(px(x + 5.0))
                    .top(px(5.0))
                    .text_size(px(10.5))
                    .font_family(MONO)
                    .text_color(theme.muted_foreground)
                    .child(text)
                    .into_any_element(),
            );
        }

        if playhead_x > -badge_w && playhead_x < w + badge_w {
            out.push(
                div()
                    .absolute()
                    .left(px(badge_l))
                    .top(px(3.0))
                    .w(px(badge_w))
                    .h(px(17.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .bg(theme.danger)
                    .text_color(theme.danger_foreground)
                    .text_size(px(10.5))
                    .font_family(MONO)
                    .font_semibold()
                    .child(label)
                    .into_any_element(),
            );
        }

        // Key drag readout.
        if let DragState::Keys {
            drag,
            anchor_time,
            delta_frames,
            ..
        } = &self.drag
        {
            if drag.moved {
                let t = (anchor_time + *delta_frames as f32 / fps).max(0.0);
                let x = self.view.time_to_x(t);
                let sign = if *delta_frames >= 0 { "+" } else { "" };
                out.push(
                    div()
                        .absolute()
                        .left(px((x + 10.0).clamp(4.0, (w - 110.0).max(4.0))))
                        .top(px(HEADER_HEIGHT + 6.0))
                        .px_2()
                        .py_0p5()
                        .rounded(px(4.0))
                        .bg(theme.popover)
                        .border_1()
                        .border_color(theme.border)
                        .shadow_md()
                        .text_size(px(10.5))
                        .font_family(MONO)
                        .text_color(theme.warning)
                        .child(format!(
                            "{sign}{delta_frames}f  →  frame {}",
                            editor.animation.time_to_frame(t)
                        ))
                        .into_any_element(),
                );
            }
        }

        // Hover tooltip.
        if matches!(self.drag, DragState::None) {
            if let (Some(hover), Some((px_, py_))) = (self.hover, self.pointer) {
                let text = match hover {
                    HoverTarget::Key { track, index } => {
                        editor.animation.tracks.get(track).and_then(|t| {
                            let kf = t.keyframes.get(index)?;
                            let name = editor
                                .skeleton
                                .bone(&t.bone_id)
                                .map_or(t.bone_id.clone(), |b| b.name.clone());
                            Some(format!(
                                "{name}  ·  frame {}  ·  {:.3}s",
                                editor.animation.time_to_frame(kf.time),
                                kf.time
                            ))
                        })
                    }
                    HoverTarget::SummaryKey { time } => Some(format!(
                        "All tracks  ·  frame {}  ·  {:.3}s",
                        editor.animation.time_to_frame(time),
                        time
                    )),
                };
                if let Some(text) = text {
                    let tip_w = text.chars().count() as f32 * 6.4 + 18.0;
                    out.push(
                        div()
                            .absolute()
                            .left(px((px_ + 14.0).min((w - tip_w - 4.0).max(4.0))))
                            .top(px((py_ + 18.0).min((h - 30.0).max(4.0))))
                            .px_2()
                            .py_0p5()
                            .rounded(px(4.0))
                            .bg(theme.popover)
                            .border_1()
                            .border_color(theme.border)
                            .shadow_md()
                            .text_size(px(10.5))
                            .font_family(MONO)
                            .text_color(theme.popover_foreground)
                            .child(text)
                            .into_any_element(),
                    );
                }
            }
        }

        // Empty state.
        if rows.is_empty() {
            let (title, hint) = if editor.animation.tracks.is_empty() {
                (
                    "No animation tracks",
                    "Select a bone and press K to add a keyframe",
                )
            } else {
                (
                    "No tracks to show",
                    "The selected-bone filter hides every track",
                )
            };
            out.push(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(HEADER_HEIGHT))
                    .bottom(px(SCROLLBAR_HEIGHT))
                    .child(chrome::empty_message(IconName::Keyframes, title, hint, cx))
                    .into_any_element(),
            );
        }

        out
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

        self.palette = Palette::from_theme(cx.theme());
        if self.inputs.is_none() {
            self.init_inputs(window, cx);
        }
        self.sync_inputs(window, cx);
        if self.fit_pending && self.canvas_size.width > 100.0 {
            self.fit_pending = false;
            self.fit_to_range(cx);
        }

        let (rows, overlays) = {
            let e = editor_entity.read(cx);
            let rows = self.visible_rows(e);
            let overlays = self.render_overlays(e, &rows, cx);
            (rows, overlays)
        };
        self.clamp_scroll(rows.len());

        let theme = cx.theme().clone();
        let toolbar = self.render_toolbar(window, cx);
        // The gutter is its own cached view; it is only told where the list is
        // scrolled to (a no-op unless that moved).
        let scroll_y = self.view.scroll_y;
        self.gutter
            .update(cx, |gutter, cx| gutter.set_scroll_y(scroll_y, cx));
        let gutter = AnyView::from(self.gutter.clone()).cached(
            StyleRefinement::default()
                .w(px(GUTTER_WIDTH))
                .h_full()
                .flex_shrink_0(),
        );
        let status = self.render_status_bar(&rows, cx);

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
                .bg(theme.background)
                .into_any_element()
        };

        let entity = cx.entity().clone();
        let driver = {
            let pre = entity.clone();
            let paint = entity.clone();
            gpui::canvas(
                move |bounds, window, cx| {
                    let sw = bounds.size.width.as_f32().max(1.0) as u32;
                    let sh = bounds.size.height.as_f32().max(1.0) as u32;
                    pre.update(cx, |panel, cx| {
                        let origin = Point::new(bounds.origin.x.as_f32(), bounds.origin.y.as_f32());
                        let size =
                            Size::new(bounds.size.width.as_f32(), bounds.size.height.as_f32());
                        let changed = (panel.canvas_size.width - size.width).abs() > 0.5
                            || (panel.canvas_size.height - size.height).abs() > 0.5;
                        panel.canvas_origin = origin;
                        panel.canvas_size = size;
                        // The GPU device was lost: the surface and the
                        // renderer's buffers are on it. Start over on the
                        // window's new device.
                        if panel
                            .surface
                            .as_ref()
                            .is_some_and(WgpuSurfaceHandle::is_device_lost)
                        {
                            panel.surface = None;
                            panel.renderer = TimelineRenderer::new();
                        }
                        if panel.surface.is_none() {
                            // Until the window has a new device, a new surface
                            // is on the lost one too; it renders again once it has.
                            if let Some(s) = window
                                .create_wgpu_surface(
                                    sw.max(64),
                                    sh.max(64),
                                    wgpu::TextureFormat::Bgra8UnormSrgb,
                                )
                                .filter(|s| !s.is_device_lost())
                            {
                                panel.surface = Some(s);
                                cx.notify();
                            }
                        } else if changed {
                            // Overlays are laid out from the canvas size.
                            cx.notify();
                        }
                    });
                },
                move |_bounds, _pre, _window, cx| {
                    paint.update(cx, |panel, cx| {
                        let Some(editor) = panel.editor.upgrade() else {
                            return;
                        };
                        let Some(ref surface) = panel.surface else {
                            return;
                        };
                        if surface.is_resize_pending() {
                            return;
                        }
                        let Some((view, (w, h))) = surface.back_view_with_size() else {
                            return;
                        };

                        let size = (
                            panel.canvas_size.width.max(1.0),
                            panel.canvas_size.height.max(1.0),
                        );
                        let rects = {
                            let e = editor.read(cx);
                            let rows = panel.visible_rows(e);
                            panel.scene(e, &rows, size).build_rects()
                        };
                        let uniforms = TimelineUniforms {
                            viewport: [size.0, size.1],
                            scroll_x: panel.view.scroll_x,
                            _pad: 0.0,
                        };
                        let clear = panel.palette.background;
                        let Some(ref surface) = panel.surface else {
                            return;
                        };
                        let (device, queue, fmt) = (
                            surface.device().clone(),
                            surface.queue().clone(),
                            surface.format(),
                        );
                        panel.renderer.render_frame(
                            &device, &queue, &view, w, h, fmt, &uniforms, clear, &rects,
                        );
                        drop(view);
                        if let Some(ref surface) = panel.surface {
                            surface.swap_buffers();
                        }
                        let _ = cx;
                    });
                },
            )
            .absolute()
            .inset_0()
            .size_full()
        };

        let canvas_area = div()
            .id("skeletal-timeline-canvas")
            .flex_1()
            .min_w_0()
            .h_full()
            .relative()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, w, cx| this.on_mouse_down(e, w, cx)),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, e: &MouseDownEvent, w, cx| this.on_mouse_down(e, w, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, w, cx| this.on_mouse_up(e, w, cx)),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, e: &MouseUpEvent, w, cx| this.on_mouse_up(e, w, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, w, cx| this.on_mouse_up(e, w, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Middle,
                cx.listener(|this, e: &MouseUpEvent, w, cx| this.on_mouse_up(e, w, cx)),
            )
            .on_mouse_move(
                cx.listener(|this, e: &MouseMoveEvent, w, cx| this.on_mouse_move(e, w, cx)),
            )
            .on_scroll_wheel(
                cx.listener(|this, e: &ScrollWheelEvent, _w, cx| this.on_scroll(e, cx)),
            )
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, w, cx| this.on_key_down(e, w, cx)))
            .child(gpu_display)
            .child(driver)
            .child(div().absolute().inset_0().children(overlays));

        v_flex_root(&theme)
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(gutter)
                    .child(canvas_area),
            )
            .child(status)
            .into_any_element()
    }
}

fn v_flex_root(theme: &ui::Theme) -> Div {
    div().size_full().flex().flex_col().bg(theme.background)
}

impl TimelinePanel {
    /// Create the toolbar inputs and wire edits back to the editor.
    fn init_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let (start, end, frame) = {
            let e = editor.read(cx);
            (e.play_range.0, e.play_range.1, e.current_frame())
        };

        let start_input = cx.new(|cx| InputState::new(window, cx).default_value(start.to_string()));
        let end_input = cx.new(|cx| InputState::new(window, cx).default_value(end.to_string()));
        let frame_input = cx.new(|cx| InputState::new(window, cx).default_value(frame.to_string()));

        let mut subs = Vec::new();
        let editor_weak = self.editor.clone();

        macro_rules! range_input {
            ($input:expr, $setter:ident) => {{
                let ew = editor_weak.clone();
                subs.push(cx.subscribe_in(
                    &$input,
                    window,
                    move |_this, input, event: &InputEvent, _w, cx| {
                        if matches!(event, InputEvent::Change) {
                            if let (Ok(value), Some(editor)) =
                                (input.read(cx).value().trim().parse::<i32>(), ew.upgrade())
                            {
                                editor.update(cx, |e, cx| e.$setter(value, cx));
                            }
                        }
                    },
                ));
                let ew = editor_weak.clone();
                subs.push(cx.subscribe_in(
                    &$input,
                    window,
                    move |_this, input, event: &ui::input::NumberInputEvent, window, cx| {
                        let ui::input::NumberInputEvent::Step { action, .. } = event;
                        let current = input.read(cx).value().trim().parse::<i32>().unwrap_or(0);
                        let value = match action {
                            ui::input::StepAction::Increment => current + 1,
                            ui::input::StepAction::Decrement => current - 1,
                        };
                        input.update(cx, |i, cx| i.set_value(value.to_string(), window, cx));
                        if let Some(editor) = ew.upgrade() {
                            editor.update(cx, |e, cx| e.$setter(value, cx));
                        }
                    },
                ));
            }};
        }
        range_input!(start_input, set_play_range_start);
        range_input!(end_input, set_play_range_end);

        // Current-frame field: typing a number jumps the playhead.
        let ew = editor_weak.clone();
        subs.push(cx.subscribe_in(
            &frame_input,
            window,
            move |_this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    if let (Ok(value), Some(editor)) =
                        (input.read(cx).value().trim().parse::<i32>(), ew.upgrade())
                    {
                        editor.update(cx, |e, cx| {
                            if e.current_frame() != value {
                                e.goto_frame(value, window, cx);
                            }
                        });
                    }
                }
            },
        ));
        let ew = editor_weak.clone();
        subs.push(cx.subscribe_in(
            &frame_input,
            window,
            move |_this, _input, event: &ui::input::NumberInputEvent, window, cx| {
                let ui::input::NumberInputEvent::Step { action, .. } = event;
                let delta = match action {
                    ui::input::StepAction::Increment => 1,
                    ui::input::StepAction::Decrement => -1,
                };
                if let Some(editor) = ew.upgrade() {
                    editor.update(cx, |e, cx| e.step_frame(delta, window, cx));
                }
            },
        ));

        self.inputs = Some(TimelineInputs {
            start: start_input,
            end: end_input,
            frame: frame_input,
            _subscriptions: subs,
        });
    }

    /// Keep the toolbar fields in step with the editor unless being typed in.
    fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(inputs) = &self.inputs else { return };
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let (start, end, frame) = {
            let e = editor.read(cx);
            (e.play_range.0, e.play_range.1, e.current_frame())
        };
        for (input, value) in [
            (&inputs.start, start),
            (&inputs.end, end),
            (&inputs.frame, frame),
        ] {
            let text = value.to_string();
            let focused = input.focus_handle(cx).contains_focused(window, cx);
            if !focused && input.read(cx).value().as_ref() != text.as_str() {
                input.update(cx, |i, cx| i.set_value(text, window, cx));
            }
        }
    }
}

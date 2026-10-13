//! Pure timeline layout: view math, ruler ticks, draw-list construction and
//! hit testing.
//!
//! Nothing here touches GPUI or wgpu, which keeps the panel code thin and lets
//! the layout be tested (and rendered headlessly) on its own.

use std::collections::HashSet;

use crate::core::AnimationClip;
use crate::style::{mix, with_alpha, Palette};

use super::types::*;

pub const RULER_HEIGHT: f32 = 30.0;
pub const SUMMARY_HEIGHT: f32 = 26.0;
pub const ROW_HEIGHT: f32 = 26.0;
/// Ruler plus the pinned summary row; track rows scroll underneath.
pub const HEADER_HEIGHT: f32 = RULER_HEIGHT + SUMMARY_HEIGHT;
pub const SCROLLBAR_HEIGHT: f32 = 12.0;
pub const KEY_SIZE: f32 = 12.0;
pub const SUMMARY_KEY_SIZE: f32 = 10.0;
pub const KEY_HIT_RADIUS: f32 = 9.0;
pub const BASE_PX_PER_SEC: f32 = 150.0;
pub const MIN_ZOOM: f32 = 0.05;
pub const MAX_ZOOM: f32 = 40.0;

/// Tolerance, in seconds, for treating two keyframe times as the same instant.
pub const TIME_EPS: f32 = 1e-4;

#[derive(Clone, Copy, Debug)]
pub struct TimelineView {
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub zoom: f32,
}

impl Default for TimelineView {
    fn default() -> Self {
        Self {
            scroll_x: 0.0,
            scroll_y: 0.0,
            zoom: 1.0,
        }
    }
}

impl TimelineView {
    pub fn px_per_sec(&self) -> f32 {
        BASE_PX_PER_SEC * self.zoom
    }

    /// Canvas-relative x of `time`.
    pub fn time_to_x(&self, time: f32) -> f32 {
        time * self.px_per_sec() - self.scroll_x
    }

    /// Time at a canvas-relative x. May be negative when panned left of zero.
    pub fn x_to_time(&self, x: f32) -> f32 {
        (x + self.scroll_x) / self.px_per_sec()
    }

    /// Largest valid `scroll_y` for `row_count` track rows in a canvas of
    /// height `canvas_h`.
    pub fn max_scroll_y(row_count: usize, canvas_h: f32) -> f32 {
        let content = row_count as f32 * ROW_HEIGHT;
        let visible = (canvas_h - HEADER_HEIGHT - SCROLLBAR_HEIGHT).max(0.0);
        (content - visible).max(0.0)
    }

    /// Zoom by `factor`, keeping the time under canvas x `anchor_x` fixed.
    pub fn zoom_at(&mut self, factor: f32, anchor_x: f32) {
        let anchor_time = self.x_to_time(anchor_x);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.scroll_x = anchor_time * self.px_per_sec() - anchor_x;
    }

    /// Fit `[start, end]` seconds into a canvas `width` px wide.
    pub fn fit(&mut self, start: f32, end: f32, width: f32) {
        let span = (end - start).max(0.05);
        let usable = (width - 48.0).max(32.0);
        self.zoom = (usable / span / BASE_PX_PER_SEC).clamp(MIN_ZOOM, MAX_ZOOM);
        self.scroll_x = start * self.px_per_sec() - 24.0;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RulerMode {
    #[default]
    Frames,
    Seconds,
}

impl RulerMode {
    pub fn toggled(self) -> Self {
        match self {
            RulerMode::Frames => RulerMode::Seconds,
            RulerMode::Seconds => RulerMode::Frames,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Tick {
    /// Position in world pixels (add `-scroll_x` for canvas space).
    pub x: f32,
    pub label: Option<String>,
    pub major: bool,
    /// Lands on a whole second; drawn a little stronger in the grid.
    pub second: bool,
}

const FRAME_STEPS: [i32; 14] = [1, 2, 5, 10, 20, 25, 50, 100, 200, 500, 1000, 2000, 5000, 10000];
const SECOND_STEPS: [f32; 15] = [
    0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0,
];

/// Format `seconds` as a compact ruler label.
fn seconds_label(seconds: f32, step: f32) -> String {
    if seconds.abs() >= 60.0 && step >= 1.0 {
        let total = seconds.round() as i32;
        let sign = if total < 0 { "-" } else { "" };
        let total = total.abs();
        return format!("{sign}{}:{:02}", total / 60, total % 60);
    }
    if step >= 1.0 {
        format!("{}s", seconds.round() as i32)
    } else if step >= 0.1 {
        format!("{seconds:.1}s")
    } else {
        format!("{seconds:.2}s")
    }
}

/// Ruler ticks visible across `canvas_w` px.
pub fn ruler_ticks(view: &TimelineView, fps: f32, canvas_w: f32, mode: RulerMode) -> Vec<Tick> {
    let pps = view.px_per_sec();
    let fps = fps.max(1.0);
    let mut ticks = Vec::new();
    let first_t = view.scroll_x / pps;
    let last_t = (view.scroll_x + canvas_w) / pps;

    match mode {
        RulerMode::Frames => {
            let ppf = pps / fps;
            let major = FRAME_STEPS
                .iter()
                .copied()
                .find(|s| *s as f32 * ppf >= 64.0)
                .unwrap_or(*FRAME_STEPS.last().unwrap());
            let minor = [1, major / 5, major / 2]
                .into_iter()
                .filter(|m| *m >= 1 && major % *m == 0)
                .find(|m| *m as f32 * ppf >= 6.0)
                .unwrap_or(major);

            let start = ((first_t * fps) / minor as f32).floor() as i32 - 1;
            let end = ((last_t * fps) / minor as f32).ceil() as i32 + 1;
            let fps_i = fps.round().max(1.0) as i32;
            for i in start..=end {
                let frame = i * minor;
                let is_major = frame % major == 0;
                ticks.push(Tick {
                    x: frame as f32 / fps * pps,
                    label: is_major.then(|| frame.to_string()),
                    major: is_major,
                    second: frame % fps_i == 0,
                });
            }
        }
        RulerMode::Seconds => {
            let major = SECOND_STEPS
                .iter()
                .copied()
                .find(|s| *s * pps >= 72.0)
                .unwrap_or(*SECOND_STEPS.last().unwrap());
            let minor = [major / 5.0, major / 2.0]
                .into_iter()
                .find(|m| *m * pps >= 7.0)
                .unwrap_or(major);
            let start = (first_t / minor).floor() as i32 - 1;
            let end = (last_t / minor).ceil() as i32 + 1;
            for i in start..=end {
                let t = i as f32 * minor;
                let ratio = t / major;
                let is_major = (ratio - ratio.round()).abs() < 1e-3;
                ticks.push(Tick {
                    x: t * pps,
                    label: is_major.then(|| seconds_label(t, major)),
                    major: is_major,
                    second: (t - t.round()).abs() < 1e-3,
                });
            }
        }
    }
    ticks
}

/// Which element of the timeline the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Ruler,
    Key { track: usize, index: usize },
    SummaryKey { time: f32 },
    SummaryRow,
    Scrollbar,
    Row { track: usize },
    Empty,
}

/// What the pointer hovers, for highlighting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HoverTarget {
    Key { track: usize, index: usize },
    SummaryKey { time: f32 },
}

#[derive(Clone, Copy, Debug)]
pub struct Marquee {
    pub a: (f32, f32),
    pub b: (f32, f32),
}

impl Marquee {
    pub fn rect(&self) -> (f32, f32, f32, f32) {
        (
            self.a.0.min(self.b.0),
            self.a.1.min(self.b.1),
            self.a.0.max(self.b.0),
            self.a.1.max(self.b.1),
        )
    }
}

/// Everything needed to draw and hit-test one frame of the timeline.
pub struct Scene<'a> {
    pub view: TimelineView,
    /// Canvas size in px.
    pub size: (f32, f32),
    pub clip: &'a AnimationClip,
    /// Indices into `clip.tracks`, in display order.
    pub rows: &'a [usize],
    pub selected_bone: Option<&'a str>,
    pub selected_keys: &'a HashSet<(String, usize)>,
    pub time: f32,
    pub play_range: (i32, i32),
    pub hover: Option<HoverTarget>,
    pub marquee: Option<Marquee>,
    pub ruler: RulerMode,
    /// Time of the key being dragged, to draw a guide line.
    pub drag_guide: Option<f32>,
    pub palette: Palette,
}

#[derive(Clone, Copy, Debug)]
pub struct SummaryKey {
    pub time: f32,
    pub selected: bool,
}

impl<'a> Scene<'a> {
    fn fps(&self) -> f32 {
        self.clip.fps.max(1.0)
    }

    pub fn row_top(&self, row: usize) -> f32 {
        HEADER_HEIGHT + row as f32 * ROW_HEIGHT - self.view.scroll_y
    }

    fn key_selected(&self, bone_id: &str, index: usize, probe: &mut (String, usize)) -> bool {
        if self.selected_keys.is_empty() {
            return false;
        }
        if probe.0 != bone_id {
            probe.0.clear();
            probe.0.push_str(bone_id);
        }
        probe.1 = index;
        self.selected_keys.contains(probe)
    }

    /// Merged key times across the visible rows (the summary row).
    pub fn summary_keys(&self) -> Vec<SummaryKey> {
        let mut raw: Vec<(f32, bool)> = Vec::new();
        let mut probe = (String::new(), 0usize);
        for &ti in self.rows {
            let Some(track) = self.clip.tracks.get(ti) else {
                continue;
            };
            for (k, kf) in track.keyframes.iter().enumerate() {
                raw.push((kf.time, self.key_selected(&track.bone_id, k, &mut probe)));
            }
        }
        raw.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut out: Vec<SummaryKey> = Vec::new();
        let mut all_selected = true;
        for (time, selected) in raw {
            match out.last_mut() {
                Some(last) if (time - last.time).abs() < TIME_EPS => {
                    all_selected &= selected;
                    last.selected = all_selected;
                }
                _ => {
                    all_selected = selected;
                    out.push(SummaryKey { time, selected });
                }
            }
        }
        out
    }

    /// Width in px of the content the scrollbar represents.
    fn content_seconds(&self) -> f32 {
        let fps = self.fps();
        let last_key = self
            .clip
            .tracks
            .iter()
            .flat_map(|t| t.keyframes.last().map(|k| k.time))
            .fold(0.0_f32, f32::max);
        let range_end = (self.play_range.1 + 1) as f32 / fps;
        self.clip.duration.max(last_key).max(range_end).max(0.1) + 0.5
    }

    /// Scrollbar track and thumb as `(track_x, track_w, thumb_x, thumb_w)`.
    pub fn scrollbar(&self) -> (f32, f32, f32, f32) {
        let pad = 8.0;
        let track_x = pad;
        let track_w = (self.size.0 - pad * 2.0).max(1.0);
        let pps = self.view.px_per_sec();
        let content_l = (self.view.scroll_x / pps).min(0.0);
        let content_r = self
            .content_seconds()
            .max((self.view.scroll_x + self.size.0) / pps);
        let total = (content_r - content_l).max(0.001);
        let view_l = self.view.scroll_x / pps;
        let view_w = self.size.0 / pps;
        let thumb_w = (view_w / total * track_w).clamp(24.0, track_w);
        let thumb_x = track_x + ((view_l - content_l) / total * track_w).clamp(0.0, track_w - thumb_w);
        (track_x, track_w, thumb_x, thumb_w)
    }

    /// `scroll_x` that puts the scrollbar thumb's left edge at `thumb_x`.
    pub fn scroll_for_thumb(&self, thumb_x: f32) -> f32 {
        let (track_x, track_w, _, thumb_w) = self.scrollbar();
        let pps = self.view.px_per_sec();
        let content_l = (self.view.scroll_x / pps).min(0.0);
        let content_r = self
            .content_seconds()
            .max((self.view.scroll_x + self.size.0) / pps);
        let total = (content_r - content_l).max(0.001);
        let frac = ((thumb_x - track_x) / (track_w - thumb_w).max(1.0)).clamp(0.0, 1.0);
        let view_w = self.size.0 / pps;
        (content_l + frac * (total - view_w).max(0.0)) * pps
    }

    pub fn hit_test(&self, x: f32, y: f32) -> Hit {
        if y < 0.0 {
            return Hit::Empty;
        }
        if y < RULER_HEIGHT {
            return Hit::Ruler;
        }
        if y < HEADER_HEIGHT {
            let mut best: Option<(f32, f32)> = None;
            for key in self.summary_keys() {
                let dx = (self.view.time_to_x(key.time) - x).abs();
                if dx <= KEY_HIT_RADIUS && best.is_none_or(|(d, _)| dx < d) {
                    best = Some((dx, key.time));
                }
            }
            return best.map_or(Hit::SummaryRow, |(_, time)| Hit::SummaryKey { time });
        }
        if y >= self.size.1 - SCROLLBAR_HEIGHT {
            return Hit::Scrollbar;
        }

        let local_row = ((y - HEADER_HEIGHT + self.view.scroll_y) / ROW_HEIGHT).floor();
        if local_row < 0.0 || local_row as usize >= self.rows.len() {
            return Hit::Empty;
        }
        let row = local_row as usize;
        let track_index = self.rows[row];
        let Some(track) = self.clip.tracks.get(track_index) else {
            return Hit::Empty;
        };
        let mut best: Option<(f32, usize)> = None;
        for (k, kf) in track.keyframes.iter().enumerate() {
            let dx = (self.view.time_to_x(kf.time) - x).abs();
            if dx <= KEY_HIT_RADIUS && best.is_none_or(|(d, _)| dx < d) {
                best = Some((dx, k));
            }
        }
        match best {
            Some((_, index)) => Hit::Key {
                track: track_index,
                index,
            },
            None => Hit::Row { track: track_index },
        }
    }

    /// Keys whose marker center lies inside the canvas rectangle.
    pub fn keys_in_rect(&self, rect: (f32, f32, f32, f32)) -> HashSet<(String, usize)> {
        let (min_x, min_y, max_x, max_y) = rect;
        let mut out = HashSet::new();
        for (row, &ti) in self.rows.iter().enumerate() {
            let Some(track) = self.clip.tracks.get(ti) else {
                continue;
            };
            let cy = self.row_top(row) + ROW_HEIGHT * 0.5;
            if cy < min_y || cy > max_y || cy < HEADER_HEIGHT {
                continue;
            }
            for (k, kf) in track.keyframes.iter().enumerate() {
                let cx = self.view.time_to_x(kf.time);
                if cx >= min_x && cx <= max_x {
                    out.insert((track.bone_id.clone(), k));
                }
            }
        }
        out
    }

    /// Build the draw list for this frame, back to front.
    pub fn build_rects(&self) -> Vec<RectInstance> {
        let p = &self.palette;
        let (w, h) = self.size;
        let pps = self.view.px_per_sec();
        let fps = self.fps();
        let mut r: Vec<RectInstance> = Vec::with_capacity(512);

        let key_color = mix(p.info, p.text, 0.25);
        let key_edge = mix(p.background, p.info, 0.35);
        let sel_color = p.warning;
        let sel_edge = mix(p.warning, [1.0, 1.0, 1.0, 1.0], 0.55);

        let ticks = ruler_ticks(&self.view, fps, w, self.ruler);

        // ── Track rows (scrolled vertically; the header is drawn over them) ──
        let area_bottom = h - SCROLLBAR_HEIGHT;
        for (row, &ti) in self.rows.iter().enumerate() {
            let Some(track) = self.clip.tracks.get(ti) else {
                continue;
            };
            let top = self.row_top(row);
            if top + ROW_HEIGHT < HEADER_HEIGHT || top > area_bottom {
                continue;
            }
            let selected_row = self.selected_bone == Some(track.bone_id.as_str());
            let base = if row % 2 == 0 {
                p.background
            } else {
                mix(p.background, p.text, 0.022)
            };
            let bg = if selected_row {
                mix(base, p.primary, 0.16)
            } else {
                base
            };
            r.push(RectInstance::new([0.0, top], [w, ROW_HEIGHT], bg).fixed());
            if selected_row {
                r.push(RectInstance::new([0.0, top], [3.0, ROW_HEIGHT], p.primary).fixed());
            }
            r.push(
                RectInstance::new(
                    [0.0, top + ROW_HEIGHT - 1.0],
                    [w, 1.0],
                    with_alpha(p.border, 0.45),
                )
                .fixed(),
            );
        }
        // Empty area below the rows keeps the striping going, so the canvas
        // doesn't look cut off.
        let mut filler_row = self.rows.len();
        loop {
            let top = self.row_top(filler_row);
            if top >= area_bottom {
                break;
            }
            if top + ROW_HEIGHT > HEADER_HEIGHT {
                let base = if filler_row.is_multiple_of(2) {
                    p.background
                } else {
                    mix(p.background, p.text, 0.022)
                };
                r.push(RectInstance::new([0.0, top], [w, ROW_HEIGHT], mix(base, p.background, 0.4)).fixed());
            }
            filler_row += 1;
        }

        // Vertical frame grid.
        for t in &ticks {
            let sx = t.x - self.view.scroll_x;
            if sx < -2.0 || sx > w + 2.0 {
                continue;
            }
            let alpha = if t.second {
                0.16
            } else if t.major {
                0.10
            } else {
                0.04
            };
            r.push(RectInstance::new(
                [t.x, HEADER_HEIGHT],
                [1.0, area_bottom - HEADER_HEIGHT],
                with_alpha(p.text, alpha),
            ));
        }

        // Spans and keys.
        let mut probe = (String::new(), 0usize);
        let hover_key = match self.hover {
            Some(HoverTarget::Key { track, index }) => Some((track, index)),
            _ => None,
        };
        for (row, &ti) in self.rows.iter().enumerate() {
            let Some(track) = self.clip.tracks.get(ti) else {
                continue;
            };
            let top = self.row_top(row);
            if top + ROW_HEIGHT < HEADER_HEIGHT || top > area_bottom {
                continue;
            }
            let cy = top + ROW_HEIGHT * 0.5;

            for pair in track.keyframes.windows(2) {
                let (a, b) = (&pair[0], &pair[1]);
                let (xa, xb) = (a.time * pps, b.time * pps);
                if xb - self.view.scroll_x < -4.0 || xa - self.view.scroll_x > w + 4.0 {
                    continue;
                }
                if a.transform == b.transform {
                    // Hold: the pose doesn't change between these keys.
                    r.push(
                        RectInstance::new([xa, cy - 3.0], [xb - xa, 6.0], with_alpha(p.info, 0.34))
                            .radius(3.0)
                            .outline(with_alpha(p.info, 0.55), 1.0),
                    );
                } else {
                    r.push(RectInstance::new(
                        [xa, cy - 0.5],
                        [xb - xa, 1.0],
                        with_alpha(p.text_muted, 0.32),
                    ));
                }
            }

            for (k, kf) in track.keyframes.iter().enumerate() {
                let sx = self.view.time_to_x(kf.time);
                if sx < -KEY_SIZE || sx > w + KEY_SIZE {
                    continue;
                }
                let selected = self.key_selected(&track.bone_id, k, &mut probe);
                let hovered = hover_key == Some((ti, k));
                let x = kf.time * pps;
                let s = if selected { KEY_SIZE + 1.0 } else { KEY_SIZE };
                let (fill, edge) = if selected {
                    (sel_color, sel_edge)
                } else if hovered {
                    (mix(key_color, [1.0, 1.0, 1.0, 1.0], 0.35), [1.0, 1.0, 1.0, 0.9])
                } else {
                    (key_color, key_edge)
                };
                let mut inst = RectInstance::new([x - s * 0.5, cy - s * 0.5], [s, s], fill)
                    .shape(SHAPE_DIAMOND)
                    .radius(1.5)
                    .outline(edge, 1.25);
                if selected {
                    inst.glow = 6.0;
                    inst.border_color = edge;
                }
                r.push(inst);
            }
        }

        // ── Out-of-range hatch ──
        let range_l = self.view.time_to_x(self.play_range.0 as f32 / fps);
        let range_r = self.view.time_to_x((self.play_range.1 + 1) as f32 / fps);
        let hatch_fill = with_alpha(mix(p.background, [0.0, 0.0, 0.0, 1.0], 0.5), 0.55);
        let hatch_line = with_alpha(p.text, 0.07);
        let hatch_h = area_bottom - HEADER_HEIGHT;
        let left_w = range_l.clamp(0.0, w);
        if left_w > 0.0 {
            r.push(
                RectInstance::new([0.0, HEADER_HEIGHT], [left_w, hatch_h], hatch_fill)
                    .shape(SHAPE_HATCH)
                    .outline(hatch_line, 0.0)
                    .fixed(),
            );
        }
        let right_x = range_r.clamp(0.0, w);
        if w - right_x > 0.0 {
            r.push(
                RectInstance::new([right_x, HEADER_HEIGHT], [w - right_x, hatch_h], hatch_fill)
                    .shape(SHAPE_HATCH)
                    .outline(hatch_line, 0.0)
                    .fixed(),
            );
        }
        // Crisp edges where the range begins and ends.
        for edge_x in [range_l, range_r] {
            if (0.0..=w).contains(&edge_x) {
                r.push(
                    RectInstance::new(
                        [edge_x - 0.5, HEADER_HEIGHT],
                        [1.0, hatch_h],
                        with_alpha(p.primary, 0.55),
                    )
                    .fixed(),
                );
            }
        }

        // ── Header: ruler + summary row, opaque over scrolled rows ──
        r.push(
            RectInstance::new([0.0, 0.0], [w, RULER_HEIGHT], mix(p.surface, p.panel, 0.25))
                .shape(SHAPE_GRADIENT)
                .outline(mix(p.surface, p.background, 0.3), 0.0)
                .fixed(),
        );
        r.push(
            RectInstance::new(
                [0.0, RULER_HEIGHT],
                [w, SUMMARY_HEIGHT],
                mix(p.panel, p.background, 0.45),
            )
            .fixed(),
        );

        // Play range band on the ruler.
        if range_r > 0.0 && range_l < w {
            let l = range_l.max(0.0);
            let rr = range_r.min(w);
            r.push(RectInstance::new([l, 0.0], [rr - l, RULER_HEIGHT], with_alpha(p.primary, 0.10)).fixed());
            r.push(
                RectInstance::new([l, RULER_HEIGHT - 3.0], [rr - l, 3.0], with_alpha(p.primary, 0.9)).fixed(),
            );
        }
        // Range handles.
        for (edge_x, left) in [(range_l, true), (range_r, false)] {
            if (-6.0..=w + 6.0).contains(&edge_x) {
                let x = if left { edge_x } else { edge_x - 4.0 };
                r.push(
                    RectInstance::new([x, RULER_HEIGHT - 13.0], [4.0, 13.0], p.primary)
                        .radius(1.5)
                        .fixed(),
                );
            }
        }

        // Tick marks.
        for t in &ticks {
            let sx = t.x - self.view.scroll_x;
            if sx < -2.0 || sx > w + 2.0 {
                continue;
            }
            let (len, alpha) = if t.major { (11.0, 0.75) } else { (5.0, 0.38) };
            r.push(
                RectInstance::new(
                    [sx - 0.5, RULER_HEIGHT - len],
                    [1.0, len],
                    with_alpha(p.text_muted, alpha),
                )
                .fixed(),
            );
        }

        // Separators.
        r.push(RectInstance::new([0.0, RULER_HEIGHT - 1.0], [w, 1.0], with_alpha(p.border, 0.8)).fixed());
        r.push(RectInstance::new([0.0, HEADER_HEIGHT - 1.0], [w, 1.0], with_alpha(p.border, 0.9)).fixed());

        // Summary diamonds.
        let sum_cy = RULER_HEIGHT + SUMMARY_HEIGHT * 0.5;
        let summary = self.summary_keys();
        let hover_summary = match self.hover {
            Some(HoverTarget::SummaryKey { time }) => Some(time),
            _ => None,
        };
        for pair in summary.windows(2) {
            let (xa, xb) = (
                self.view.time_to_x(pair[0].time),
                self.view.time_to_x(pair[1].time),
            );
            if xb < 0.0 || xa > w {
                continue;
            }
            r.push(
                RectInstance::new([xa, sum_cy - 0.5], [xb - xa, 1.0], with_alpha(p.text_muted, 0.22)).fixed(),
            );
        }
        for key in &summary {
            let sx = self.view.time_to_x(key.time);
            if sx < -KEY_SIZE || sx > w + KEY_SIZE {
                continue;
            }
            let hovered = hover_summary.is_some_and(|t| (t - key.time).abs() < TIME_EPS);
            let (fill, edge) = if key.selected {
                (sel_color, sel_edge)
            } else if hovered {
                (mix(p.text, p.info, 0.3), [1.0, 1.0, 1.0, 0.9])
            } else {
                (
                    mix(p.text_muted, p.text, 0.35),
                    mix(p.background, p.text_muted, 0.4),
                )
            };
            let mut inst = RectInstance::new(
                [sx - SUMMARY_KEY_SIZE * 0.5, sum_cy - SUMMARY_KEY_SIZE * 0.5],
                [SUMMARY_KEY_SIZE, SUMMARY_KEY_SIZE],
                fill,
            )
            .shape(SHAPE_DIAMOND)
            .radius(1.5)
            .outline(edge, 1.0)
            .fixed();
            if key.selected {
                inst.glow = 5.0;
            }
            r.push(inst);
        }

        // ── Drag guide ──
        if let Some(t) = self.drag_guide {
            let gx = self.view.time_to_x(t);
            let mut y = HEADER_HEIGHT;
            while y < area_bottom {
                r.push(RectInstance::new([gx - 0.5, y], [1.0, 4.0], with_alpha(p.warning, 0.75)).fixed());
                y += 8.0;
            }
        }

        // ── Playhead ──
        let px = self.view.time_to_x(self.time);
        if (-8.0..=w + 8.0).contains(&px) {
            r.push(
                RectInstance::new(
                    [px - 3.0, RULER_HEIGHT - 2.0],
                    [6.0, area_bottom - RULER_HEIGHT + 2.0],
                    with_alpha(p.danger, 0.10),
                )
                .fixed(),
            );
            r.push(
                RectInstance::new(
                    [px - 1.0, RULER_HEIGHT - 2.0],
                    [2.0, area_bottom - RULER_HEIGHT + 2.0],
                    p.danger,
                )
                .fixed(),
            );
            r.push(
                RectInstance::new([px - 6.0, RULER_HEIGHT - 9.0], [12.0, 9.0], p.danger)
                    .shape(SHAPE_TRIANGLE_DOWN)
                    .fixed(),
            );
        }

        // ── Marquee ──
        if let Some(m) = self.marquee {
            let (x0, y0, x1, y1) = m.rect();
            let (bw, bh) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
            r.push(
                RectInstance::new([x0, y0], [bw, bh], with_alpha(p.primary, 0.14))
                    .radius(2.0)
                    .outline(with_alpha(mix(p.primary, [1.0, 1.0, 1.0, 1.0], 0.3), 0.95), 1.0)
                    .fixed(),
            );
        }

        // ── Horizontal scrollbar ──
        let (track_x, track_w, thumb_x, thumb_w) = self.scrollbar();
        let sb_y = h - SCROLLBAR_HEIGHT + 3.0;
        r.push(
            RectInstance::new(
                [0.0, h - SCROLLBAR_HEIGHT],
                [w, SCROLLBAR_HEIGHT],
                mix(p.panel, p.background, 0.3),
            )
            .fixed(),
        );
        r.push(RectInstance::new([0.0, h - SCROLLBAR_HEIGHT], [w, 1.0], with_alpha(p.border, 0.8)).fixed());
        r.push(
            RectInstance::new([track_x, sb_y], [track_w, 6.0], with_alpha(p.text, 0.05))
                .radius(3.0)
                .fixed(),
        );
        r.push(
            RectInstance::new([thumb_x, sb_y], [thumb_w, 6.0], with_alpha(p.text_muted, 0.45))
                .radius(3.0)
                .fixed(),
        );

        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sample;

    fn scene_fixture<'a>(
        clip: &'a AnimationClip,
        rows: &'a [usize],
        keys: &'a HashSet<(String, usize)>,
    ) -> Scene<'a> {
        Scene {
            view: TimelineView {
                scroll_x: 0.0,
                scroll_y: 0.0,
                zoom: 4.0,
            },
            size: (900.0, 320.0),
            clip,
            rows,
            selected_bone: None,
            selected_keys: keys,
            time: 0.2,
            play_range: (0, 18),
            hover: None,
            marquee: None,
            ruler: RulerMode::Frames,
            drag_guide: None,
            palette: Palette::test_dark(),
        }
    }

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let mut v = TimelineView::default();
        v.scroll_x = 40.0;
        let before = v.x_to_time(300.0);
        v.zoom_at(1.7, 300.0);
        assert!((v.x_to_time(300.0) - before).abs() < 1e-4);
    }

    #[test]
    fn ticks_have_labels_on_majors_only() {
        let v = TimelineView {
            zoom: 3.0,
            ..Default::default()
        };
        let ticks = ruler_ticks(&v, 30.0, 800.0, RulerMode::Frames);
        assert!(ticks.iter().any(|t| t.label.is_some()));
        assert!(ticks.iter().all(|t| t.label.is_none() || t.major));
        let secs = ruler_ticks(&v, 30.0, 800.0, RulerMode::Seconds);
        assert!(secs.iter().any(|t| t.label.is_some()));
    }

    #[test]
    fn hit_test_finds_keys_and_regions() {
        let skeleton = sample::humanoid_skeleton();
        let clip = sample::run_animation(&skeleton);
        let rows: Vec<usize> = (0..clip.tracks.len()).collect();
        let keys = HashSet::new();
        let mut scene = scene_fixture(&clip, &rows, &keys);
        // Zoomed in far enough that neighbouring keys are further apart than
        // twice the hit radius.
        scene.view.zoom = 16.0;

        assert_eq!(scene.hit_test(100.0, 10.0), Hit::Ruler);
        assert!(matches!(
            scene.hit_test(100.0, RULER_HEIGHT + 5.0),
            Hit::SummaryRow | Hit::SummaryKey { .. }
        ));
        assert_eq!(scene.hit_test(100.0, 319.0), Hit::Scrollbar);

        // Second key of the first track.
        let kf = &clip.tracks[0].keyframes[1];
        let x = scene.view.time_to_x(kf.time);
        let y = HEADER_HEIGHT + ROW_HEIGHT * 0.5;
        assert_eq!(scene.hit_test(x, y), Hit::Key { track: 0, index: 1 });
        let next = scene.view.time_to_x(clip.tracks[0].keyframes[2].time);
        assert_eq!(
            scene.hit_test((x + next) * 0.5, y),
            Hit::Row { track: 0 },
            "midway between two keys is just the row"
        );
    }

    #[test]
    fn marquee_selects_keys_inside_only() {
        let skeleton = sample::humanoid_skeleton();
        let clip = sample::run_animation(&skeleton);
        let rows: Vec<usize> = (0..clip.tracks.len()).collect();
        let keys = HashSet::new();
        let scene = scene_fixture(&clip, &rows, &keys);
        let x0 = scene.view.time_to_x(clip.tracks[0].keyframes[0].time) - 4.0;
        let x1 = scene.view.time_to_x(clip.tracks[0].keyframes[2].time) + 4.0;
        let picked = scene.keys_in_rect((x0, HEADER_HEIGHT + 2.0, x1, HEADER_HEIGHT + ROW_HEIGHT - 2.0));
        assert_eq!(picked.len(), 3);
        assert!(picked.iter().all(|(bone, _)| *bone == clip.tracks[0].bone_id));
    }

    #[test]
    fn summary_merges_simultaneous_keys() {
        let skeleton = sample::humanoid_skeleton();
        let clip = sample::run_animation(&skeleton);
        let rows: Vec<usize> = (0..clip.tracks.len()).collect();
        let keys = HashSet::new();
        let scene = scene_fixture(&clip, &rows, &keys);
        let per_track = clip.tracks[0].keyframes.len();
        assert_eq!(scene.summary_keys().len(), per_track);
    }

    #[test]
    fn scrollbar_round_trips() {
        let skeleton = sample::humanoid_skeleton();
        let clip = sample::run_animation(&skeleton);
        let rows: Vec<usize> = (0..clip.tracks.len()).collect();
        let keys = HashSet::new();
        let mut scene = scene_fixture(&clip, &rows, &keys);
        scene.view.scroll_x = 120.0;
        let (_, _, thumb_x, _) = scene.scrollbar();
        let back = scene.scroll_for_thumb(thumb_x);
        assert!((back - 120.0).abs() < 0.5, "got {back}");
    }

    #[test]
    fn draw_list_is_not_empty_and_finite() {
        let skeleton = sample::humanoid_skeleton();
        let clip = sample::run_animation(&skeleton);
        let rows: Vec<usize> = (0..clip.tracks.len()).collect();
        let keys = HashSet::new();
        let scene = scene_fixture(&clip, &rows, &keys);
        let rects = scene.build_rects();
        assert!(rects.len() > 100);
        assert!(rects
            .iter()
            .all(|r| r.pos.iter().chain(r.size.iter()).all(|v| v.is_finite())));
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::core::sample;
    use crate::rendering::preview::{self, Gpu};
    use crate::rendering::timeline::renderer::TimelineRenderer;

    /// Render the timeline headlessly: a mix of selected keys, a hovered key,
    /// a marquee, a drag guide and an out-of-range play window.
    #[test]
    fn render_timeline_preview() {
        let Some(gpu) = Gpu::new() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let skeleton = sample::humanoid_skeleton();
        let mut clip = sample::run_animation(&skeleton);
        // A hold (identical consecutive poses) to exercise the hold bar.
        let pose = clip.tracks[2].keyframes[3].transform.clone();
        clip.tracks[2].keyframes[4].transform = pose;

        let rows: Vec<usize> = (0..clip.tracks.len()).collect();
        let mut keys = HashSet::new();
        for k in 2..6 {
            keys.insert((clip.tracks[1].bone_id.clone(), k));
            keys.insert((clip.tracks[4].bone_id.clone(), k));
        }
        let (w, h) = (1100.0_f32, 420.0_f32);
        let scene = Scene {
            view: TimelineView {
                scroll_x: -24.0,
                scroll_y: 0.0,
                zoom: 6.0,
            },
            size: (w, h),
            clip: &clip,
            rows: &rows,
            selected_bone: Some(clip.tracks[1].bone_id.as_str()),
            selected_keys: &keys,
            time: 0.27,
            play_range: (1, 16),
            hover: Some(HoverTarget::Key { track: 6, index: 5 }),
            marquee: Some(Marquee {
                a: (620.0, 200.0),
                b: (760.0, 290.0),
            }),
            ruler: RulerMode::Frames,
            drag_guide: Some(0.35),
            palette: Palette::test_dark(),
        };
        let rects = scene.build_rects();

        let mut renderer = TimelineRenderer::new();
        let rgba = gpu.render(w as u32, h as u32, |device, queue, view| {
            renderer.render_frame(
                device,
                queue,
                view,
                w as u32,
                h as u32,
                preview::TARGET_FORMAT,
                &TimelineUniforms {
                    viewport: [w, h],
                    scroll_x: scene.view.scroll_x,
                    _pad: 0.0,
                },
                scene.palette.background,
                &rects,
            );
        });
        preview::save("timeline", w as u32, h as u32, &rgba);
        assert!(preview::coverage(&rgba) > 0.5, "timeline rendered mostly blank");
    }
}

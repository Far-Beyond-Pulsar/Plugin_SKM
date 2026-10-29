//! The Montage editor: a multi-track timeline with Sequence segments on slot
//! tracks, section markers with next-link arrows, notify and notify-state
//! lanes, a playhead, and an inspector for whatever is selected. Segments
//! drag, trim at their edges, reorder, split and delete; sections and notifies
//! drag along the same time axis.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use plugin_editor_api::PluginError;
use pulsar_reflection::{AnimationSequenceAssetKind, AssetKind, AssetRef, SkeletonAssetKind};
use skeletal_assets::assets::AnimSeqAsset;
use skeletal_assets::montage::{AnimationMontageAsset, BlendCurve, InterruptPolicy, MontageRootMotion};
use ui::button::{Button, ButtonVariants};
use ui::{h_flex, v_flex, ActiveTheme, Icon, IconName, Selectable, Sizable, StyledExt};

use crate::anim_editors::montage_model::MontageModel;
use crate::anim_editors::notify_edit::{LaneMetrics, NotifyKind};
use crate::anim_editors::refs::{self, ProjectRefIndex, RefStatus, SnapshotIndex};
use crate::anim_editors::selection::AnimSelection;
use crate::anim_editors::store::FsStore;
use crate::core::asset_index;

use super::chrome::{self, edit, empty_inspector, fmt_f32, frame, labelled, parse_f32, section, short_id, value_row, Chrome, EditorBody, EditorHost, EditorModel, Widgets};
use super::notify_inspector::{self, NotifyDoc};
use super::paint::{arrow, pt};

const RULER_H: f32 = 22.0;
const SECTION_H: f32 = 24.0;
const NOTIFY_ROW_H: f32 = 20.0;
const TRACK_H: f32 = 42.0;
const EDGE: f32 = 6.0;

#[derive(Clone, Debug)]
enum Drag {
    Segment { id: String, grab: f32 },
    TrimLeft(String),
    TrimRight(String),
    Section { id: String, grab: f32 },
    Notify { id: String, grab: f32 },
    StateEdge { id: String, left: bool },
    Playhead,
}

pub struct MontageEditor {
    model: MontageModel,
    chrome: Chrome,
    widgets: Widgets,
    zoom: f32,
    time: f32,
    playing: bool,
    drag: Option<Drag>,
    origin: Rc<Cell<Point<Pixels>>>,
    statuses: HashMap<String, RefStatus>,
    names: HashMap<String, String>,
    /// Notify markers the playhead crossed during playback, for a brief flash.
    flash: Vec<(String, f32)>,
}

pub fn sequence_duration(root: &std::path::Path, id: &str) -> Option<f32> {
    let path = ProjectRefIndex::new(root).locate_path(AnimationSequenceAssetKind::KIND, id)?;
    let text = String::from_utf8(engine_fs::virtual_fs::read_file(&path).ok()?).ok()?;
    AnimSeqAsset::parse(&text).ok().map(|a| a.clip.duration)
}

impl MontageEditor {
    pub fn open(path: PathBuf, _window: &mut Window, cx: &mut App) -> Result<Entity<Self>, PluginError> {
        let text = AnimationMontageAsset::default().to_text().unwrap_or_default();
        let model = MontageModel::open(Arc::new(FsStore), &path, &text).map_err(|message| PluginError::Other { message })?;
        let chrome = Chrome::new(&path, cx);
        asset_index::record_asset(&chrome.root, &path);
        let mut this = Self {
            model,
            chrome,
            widgets: Widgets::default(),
            zoom: 160.0,
            time: 0.0,
            playing: false,
            drag: None,
            origin: Rc::new(Cell::new(Point::default())),
            statuses: HashMap::new(),
            names: HashMap::new(),
            flash: Vec::new(),
        };
        this.refresh_refs();
        Ok(cx.new(|_| this))
    }

    /// Re-resolve references and sequence durations.
    fn refresh_refs(&mut self) {
        let index = SnapshotIndex::new(&self.chrome.root);
        let skeleton = self.model.asset().skeleton.as_ref().map(|s| s.as_str().to_string());
        self.statuses.clear();
        self.names.clear();
        let segments: Vec<(String, Option<String>)> = self.model.asset().segments().map(|s| (s.id.clone(), s.sequence.as_ref().map(|r| r.as_str().to_string()))).collect();
        for (id, sequence) in segments {
            let status = refs::ref_status(&index, AnimationSequenceAssetKind::KIND, sequence.as_deref(), skeleton.as_deref());
            if let Some(seq) = &sequence {
                if let RefStatus::Ok { name } | RefStatus::Incompatible { name, .. } = &status {
                    self.names.insert(seq.clone(), name.clone());
                }
                if !self.model.durations.contains_key(seq) {
                    if let Some(d) = sequence_duration(&self.chrome.root, seq) {
                        self.model.durations.insert(seq.clone(), d);
                    }
                }
            }
            self.statuses.insert(id, status);
        }
        let skeleton_status = refs::ref_status(&index, SkeletonAssetKind::KIND, skeleton.as_deref(), None);
        self.statuses.insert("skeleton".into(), skeleton_status);
    }

    fn seq_label(&self, sequence: &Option<AssetRef<AnimationSequenceAssetKind>>) -> String {
        match sequence {
            Some(s) => self.names.get(s.as_str()).cloned().unwrap_or_else(|| format!("Sequence {}", short_id(s.as_str()))),
            None => "Empty segment".into(),
        }
    }

    fn time_at(&self, position: Point<Pixels>) -> f32 {
        (f32::from(position.x - self.origin.get().x) / self.zoom).max(0.0)
    }

    fn track_at(&self, position: Point<Pixels>, top: f32) -> Option<String> {
        let y = f32::from(position.y - self.origin.get().y) - top;
        if y < 0.0 {
            return None;
        }
        self.model.asset().tracks.get((y / TRACK_H) as usize).map(|t| t.id.clone())
    }

    fn lane(&self) -> (Vec<crate::anim_editors::notify_edit::LanePlacement>, usize) {
        self.model.lane(&LaneMetrics { px_per_second: self.zoom, ..LaneMetrics::default() })
    }

    fn notify_height(&self) -> f32 {
        let (_, rows) = self.lane();
        (rows.max(1) as f32) * NOTIFY_ROW_H + 6.0
    }

    fn tracks_top(&self) -> f32 {
        RULER_H + SECTION_H + self.notify_height()
    }

    fn start_drag(&mut self, drag: Drag, position: Point<Pixels>, cx: &mut Context<Self>) {
        let _ = position;
        self.drag = Some(drag);
        cx.notify();
    }

    fn drag_to(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.clone() else { return };
        let t = self.time_at(position);
        match drag {
            Drag::Segment { id, grab } => {
                let target = self.track_at(position, self.tracks_top());
                self.model.move_segment(&id, (t - grab).max(0.0), target.as_deref(), true);
            }
            Drag::TrimLeft(id) => {
                let _ = self.model.trim_edge(&id, true, t);
            }
            Drag::TrimRight(id) => {
                let _ = self.model.trim_edge(&id, false, t);
            }
            Drag::Section { id, grab } => {
                self.model.move_section(&id, (t - grab).max(0.0));
            }
            Drag::Notify { id, grab } => {
                self.model.move_notify(&id, (t - grab).max(0.0));
            }
            Drag::StateEdge { id, left } => {
                if left {
                    self.model.resize_notify_state(&id, Some(t), None);
                } else {
                    self.model.resize_notify_state(&id, None, Some(t));
                }
            }
            Drag::Playhead => self.time = t.min(self.model.length().max(0.01)),
        }
        self.after_change(window, cx);
        cx.notify();
    }

    fn end_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            self.model.end_gesture();
            self.refresh_refs();
            self.after_change(window, cx);
            cx.notify();
        }
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        self.playing = !self.playing;
        if self.playing {
            let weak = cx.weak_entity();
            cx.spawn(async move |_, cx| {
                const FRAME: std::time::Duration = std::time::Duration::from_millis(16);
                loop {
                    smol::Timer::after(FRAME).await;
                    let go = weak.update(cx, |this, cx| {
                        if !this.playing {
                            return false;
                        }
                        let end = this.model.length().max(0.01);
                        let previous = this.time;
                        this.time += 0.016;
                        if this.time >= end {
                            this.time = 0.0;
                        }
                        // preview-only flash for notifies the playhead crosses; nothing is dispatched
                        let (a, b) = (previous, this.time);
                        let crossed: Vec<String> = this.model.asset().notifies.iter().filter(|n| a <= n.time && n.time < b).map(|n| n.id.clone()).collect();
                        this.flash.retain(|(_, t)| *t > 0.0);
                        for (_, t) in &mut this.flash {
                            *t -= 0.016;
                        }
                        this.flash.extend(crossed.into_iter().map(|id| (id, 0.25)));
                        cx.notify();
                        true
                    });
                    if !matches!(go, Ok(true)) {
                        break;
                    }
                }
            })
            .detach();
        }
        cx.notify();
    }

    fn ruler_step(&self) -> f32 {
        if self.zoom >= 300.0 {
            0.1
        } else if self.zoom >= 120.0 {
            0.25
        } else if self.zoom >= 50.0 {
            0.5
        } else {
            1.0
        }
    }

    fn timeline(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let pps = self.zoom;
        let length = self.model.length().max(self.model.timeline_limit());
        let width = (length + 1.0) * pps + 40.0;
        let (placements, _rows) = self.lane();
        let notify_h = self.notify_height();
        let tracks_top = self.tracks_top();
        let tracks = self.model.asset().tracks.clone();
        let height = tracks_top + (tracks.len().max(1) as f32) * TRACK_H + 24.0;
        let selected = self.model.selection.clone();
        let origin_cell = self.origin.clone();
        let sections = self.model.asset().sections.clone();
        let links = self.model.section_links();
        let (accent, muted, border, text) = (theme.primary, theme.muted_foreground, theme.border, theme.foreground);

        let mut inner = div().id("mt-inner").relative().w(px(width)).h(px(height));

        // measure where the inner area is painted, for pointer-to-time conversion
        inner = inner.child(
            canvas(
                move |bounds, _, _| {
                    origin_cell.set(bounds.origin);
                    bounds
                },
                {
                    let links = links.clone();
                    move |_, _, window, _| {
                        let _ = (&links, &window);
                    }
                },
            )
            .absolute()
            .size_full(),
        );

        // ruler
        let step = self.ruler_step();
        let ticks = (length / step).ceil() as i32 + 2;
        inner = inner.child(
            div()
                .id("mt-ruler")
                .absolute()
                .left_0()
                .top_0()
                .w_full()
                .h(px(RULER_H))
                .bg(theme.secondary)
                .border_b_1()
                .border_color(border)
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.time = this.time_at(e.position).min(this.model.length().max(0.01));
                    this.start_drag(Drag::Playhead, e.position, cx);
                })),
        );
        for i in 0..ticks {
            let t = i as f32 * step;
            let major = (t / 1.0).fract().abs() < 1e-4 || step >= 1.0;
            inner = inner.child(
                div()
                    .absolute()
                    .left(px(t * pps))
                    .top(px(if major { 4.0 } else { 12.0 }))
                    .h(px(if major { RULER_H - 4.0 } else { RULER_H - 12.0 }))
                    .border_l_1()
                    .border_color(muted.opacity(0.6))
                    .pl_1()
                    .text_xs()
                    .text_color(muted)
                    .when(major, |d| d.child(format!("{t:.1}s"))),
            );
        }

        // section row
        inner = inner.child(div().absolute().left_0().top(px(RULER_H)).w_full().h(px(SECTION_H)).border_b_1().border_color(border).bg(theme.background));
        for s in &sections {
            let sel = matches!(&selected, AnimSelection::MontageSection { id } if *id == s.id);
            let id = s.id.clone();
            let grab_id = s.id.clone();
            inner = inner.child(
                h_flex()
                    .id(SharedString::from(format!("sec-{}", s.id)))
                    .absolute()
                    .left(px(s.start * pps))
                    .top(px(RULER_H + 2.0))
                    .h(px(SECTION_H - 4.0))
                    .px_1()
                    .gap_1()
                    .items_center()
                    .rounded(px(3.0))
                    .bg(if sel { theme.warning.opacity(0.45) } else { theme.warning.opacity(0.22) })
                    .border_l_2()
                    .border_color(theme.warning)
                    .text_xs()
                    .text_color(text)
                    .cursor_pointer()
                    .child(Icon::new(IconName::ChevronRight).size(px(10.0)))
                    .child(s.name.clone())
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        this.model.select_section(&id);
                        let grab = this.time_at(e.position) - this.model.asset().sections.iter().find(|s| s.id == grab_id).map_or(0.0, |s| s.start);
                        this.start_drag(Drag::Section { id: grab_id.clone(), grab }, e.position, cx);
                    })),
            );
        }

        // notify and state lane
        let lane_top = RULER_H + SECTION_H;
        inner = inner.child(div().absolute().left_0().top(px(lane_top)).w_full().h(px(notify_h)).border_b_1().border_color(border).bg(theme.background.opacity(0.6)));
        for p in &placements {
            let top = lane_top + 3.0 + p.row as f32 * NOTIFY_ROW_H;
            match p.kind {
                NotifyKind::Notify => {
                    let Some(n) = self.model.asset().notifies.iter().find(|n| n.id == p.id).cloned() else { continue };
                    let sel = matches!(&selected, AnimSelection::Notify { id } if *id == n.id);
                    let flashing = self.flash.iter().any(|(id, _)| *id == n.id);
                    let (id, grab_id) = (n.id.clone(), n.id.clone());
                    inner = inner.child(
                        h_flex()
                            .id(SharedString::from(format!("nt-{}", n.id)))
                            .absolute()
                            .left(px(p.x0))
                            .top(px(top))
                            .h(px(NOTIFY_ROW_H - 2.0))
                            .px_1()
                            .gap_1()
                            .items_center()
                            .rounded(px(3.0))
                            .border_1()
                            .border_color(if sel { accent } else { theme.info.opacity(0.5) })
                            .bg(if flashing { theme.info.opacity(0.7) } else if sel { theme.info.opacity(0.35) } else { theme.info.opacity(0.18) })
                            .text_xs()
                            .text_color(text)
                            .cursor_pointer()
                            .child(Icon::new(IconName::Circle).size(px(8.0)).text_color(theme.info))
                            .child(div().max_w(px(140.0)).overflow_hidden().child(n.name.clone()))
                            .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                this.model.select_notify(&id);
                                let grab = this.time_at(e.position) - this.model.asset().notifies.iter().find(|n| n.id == grab_id).map_or(0.0, |n| n.time);
                                this.start_drag(Drag::Notify { id: grab_id.clone(), grab }, e.position, cx);
                            })),
                    );
                }
                NotifyKind::State => {
                    let Some(s) = self.model.asset().notify_states.iter().find(|n| n.id == p.id).cloned() else { continue };
                    let sel = matches!(&selected, AnimSelection::NotifyState { id } if *id == s.id);
                    let color = if s.disabled { theme.muted_foreground } else { theme.success };
                    let (id, grab_id, left_id, right_id) = (s.id.clone(), s.id.clone(), s.id.clone(), s.id.clone());
                    let w = ((s.end - s.start) * pps).max(10.0);
                    inner = inner.child(
                        h_flex()
                            .id(SharedString::from(format!("ns-{}", s.id)))
                            .absolute()
                            .left(px(s.start * pps))
                            .top(px(top))
                            .w(px(w))
                            .h(px(NOTIFY_ROW_H - 2.0))
                            .items_center()
                            .rounded(px(3.0))
                            .border_1()
                            .border_color(if sel { accent } else { color.opacity(0.7) })
                            .bg(color.opacity(if s.disabled { 0.12 } else if sel { 0.4 } else { 0.22 }))
                            .text_xs()
                            .text_color(if s.disabled { muted } else { text })
                            .cursor_pointer()
                            .child(div().w(px(EDGE)).h_full().cursor(CursorStyle::ResizeLeftRight).on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                this.model.select_notify(&left_id);
                                this.start_drag(Drag::StateEdge { id: left_id.clone(), left: true }, e.position, cx);
                                cx.stop_propagation();
                            })))
                            .child(div().flex_1().min_w_0().overflow_hidden().child(format!("{}{}", s.name, if s.disabled { " (disabled)" } else { "" })))
                            .child(div().w(px(EDGE)).h_full().cursor(CursorStyle::ResizeLeftRight).on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                this.model.select_notify(&right_id);
                                this.start_drag(Drag::StateEdge { id: right_id.clone(), left: false }, e.position, cx);
                                cx.stop_propagation();
                            })))
                            .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                this.model.select_notify(&id);
                                let grab = this.time_at(e.position) - this.model.asset().notify_states.iter().find(|n| n.id == grab_id).map_or(0.0, |n| n.start);
                                this.start_drag(Drag::Notify { id: grab_id.clone(), grab }, e.position, cx);
                            })),
                    );
                }
            }
        }

        // slot tracks with segments
        for (ti, track) in tracks.iter().enumerate() {
            let top = tracks_top + ti as f32 * TRACK_H;
            let track_selected = matches!(&selected, AnimSelection::MontageSlot { track: t } if *t == track.id);
            inner = inner.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(top))
                    .w_full()
                    .h(px(TRACK_H))
                    .border_b_1()
                    .border_color(border.opacity(0.6))
                    .when(track_selected, |d| d.bg(theme.accent.opacity(0.25)))
                    .when(ti % 2 == 1, |d| d.bg(theme.secondary.opacity(0.35))),
            );
            for seg in &track.segments {
                let (start, end) = self.model.segment_span(seg);
                let known = seg.sequence.as_ref().and_then(|s| self.model.duration_of(s)).is_some();
                let w = if known { ((end - start) * pps).max(14.0) } else { 90.0 };
                let sel = matches!(&selected, AnimSelection::MontageSegment { id, .. } if *id == seg.id);
                let active = self.model.active_segments(self.time).iter().any(|a| a.segment == seg.id);
                let problem = seg.sequence.is_none() || self.statuses.get(&seg.id).is_some_and(RefStatus::is_problem);
                let (track_id, seg_id) = (track.id.clone(), seg.id.clone());
                let (grab_id, left_id, right_id, grab_track) = (seg.id.clone(), seg.id.clone(), seg.id.clone(), track.id.clone());
                let label = self.seq_label(&seg.sequence);
                let color = if problem { theme.danger } else { theme.primary };
                inner = inner.child(
                    h_flex()
                        .id(SharedString::from(format!("seg-{}", seg.id)))
                        .absolute()
                        .left(px(start * pps))
                        .top(px(top + 5.0))
                        .w(px(w))
                        .h(px(TRACK_H - 10.0))
                        .items_center()
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(if sel { text } else { color.opacity(0.7) })
                        .bg(color.opacity(if active { 0.55 } else if sel { 0.42 } else { 0.26 }))
                        .text_xs()
                        .text_color(text)
                        .cursor_pointer()
                        .child(div().w(px(EDGE)).h_full().cursor(CursorStyle::ResizeLeftRight).on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            this.model.select_segment(&grab_track, &left_id);
                            this.start_drag(Drag::TrimLeft(left_id.clone()), e.position, cx);
                            cx.stop_propagation();
                        })))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .child(div().font_semibold().child(label))
                                .child(div().text_color(muted).child(format!("x{} \u{b7} {}\u{d7}{}", fmt_f32(seg.play_rate), seg.loop_count, if seg.clip_start > 0.0 || seg.clip_end.is_some() { " \u{b7} trimmed" } else { "" }))),
                        )
                        .child(div().w(px(EDGE)).h_full().cursor(CursorStyle::ResizeLeftRight).on_mouse_down(MouseButton::Left, {
                            let t = track.id.clone();
                            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                this.model.select_segment(&t, &right_id);
                                this.start_drag(Drag::TrimRight(right_id.clone()), e.position, cx);
                                cx.stop_propagation();
                            })
                        }))
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            this.model.select_segment(&track_id, &seg_id);
                            let start = this.model.asset().segments().find(|s| s.id == grab_id).map_or(0.0, |s| s.start);
                            let grab = this.time_at(e.position) - start;
                            this.start_drag(Drag::Segment { id: grab_id.clone(), grab }, e.position, cx);
                        })),
                );
            }
        }

        // playhead
        inner = inner.child(div().absolute().left(px(self.time * pps)).top_0().h_full().w(px(1.0)).bg(theme.danger));
        inner = inner.child(div().absolute().left(px(self.time * pps - 4.0)).top_0().w(px(9.0)).h(px(8.0)).bg(theme.danger).rounded_b(px(3.0)));

        // section links, drawn over everything
        let link_geometry: Vec<(f32, f32, bool)> = links.iter().map(|l| (l.from_time * pps, l.to_time * pps, l.backwards)).collect();
        inner = inner.child(
            canvas(
                |bounds, _, _| bounds,
                move |bounds, _, window, _| {
                    let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                    for (from, to, backwards) in &link_geometry {
                        let y = oy + RULER_H + SECTION_H - 2.0 + if *backwards { 0.0 } else { 0.0 };
                        let color: Hsla = if *backwards { hsla(0.08, 0.9, 0.6, 0.9) } else { hsla(0.12, 0.9, 0.6, 0.9) };
                        let _ = pt(0.0, 0.0);
                        arrow(window, (ox + from + 12.0, y), (ox + to, y), 1.5, color);
                    }
                },
            )
            .absolute()
            .size_full(),
        );

        div()
            .id("mt-scroll")
            .size_full()
            .overflow_x_scroll()
            .overflow_y_scroll()
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| {
                if e.dragging() {
                    this.drag_to(e.position, window, cx);
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, window, cx| this.end_drag(window, cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, window, cx| this.end_drag(window, cx)))
            .child(inner)
            .into_any_element()
    }

    fn segment_inspector(&mut self, track: &str, id: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(seg) = self.model.asset().segments().find(|s| s.id == id).cloned() else { return empty_inspector(&self.model.selection, "The segment no longer exists.", cx) };
        let status = self.statuses.get(&seg.id).cloned().unwrap_or(RefStatus::Unset);
        let skeleton = self.model.asset().skeleton.as_ref().map(|s| s.as_str().to_string());
        let root = self.chrome.root.clone();
        let (a, b) = self.model.segment_span(&seg);
        let duration = seg.sequence.as_ref().and_then(|s| self.model.duration_of(s));
        let mut body = v_flex().w_full().gap_2().child(div().text_sm().font_semibold().text_color(cx.theme().foreground).child("Segment"));
        body = body.child(self.widgets.ref_field(&format!("seg:{}", seg.id), "Sequence", AnimationSequenceAssetKind::KIND, skeleton, seg.sequence.as_ref().map(|s| s.as_str().to_string()), &status, &root, window, cx));
        if let Some(path) = seg.sequence.as_ref().and_then(|s| ProjectRefIndex::new(&root).locate_path(AnimationSequenceAssetKind::KIND, s.as_str())) {
            body = body.child(Button::new("mt-open-seq").icon(IconName::ExternalLink).label("Open Sequence").small().ghost().on_click(move |_, window, cx| window.dispatch_action(Box::new(plugin_editor_api::OpenAsset { path: path.clone() }), cx)));
        }
        body = body.child(self.widgets.text_field("seg.start", "Start (s)", &fmt_f32(seg.start), window, cx));
        body = body.child(self.widgets.text_field("seg.clip_start", "Trim start (s)", &fmt_f32(seg.clip_start), window, cx));
        body = body.child(self.widgets.text_field("seg.clip_end", "Trim end (s)", &seg.clip_end.map(fmt_f32).unwrap_or_default(), window, cx));
        body = body.child(self.widgets.text_field("seg.rate", "Play rate", &fmt_f32(seg.play_rate), window, cx));
        body = body.child(self.widgets.text_field("seg.loops", "Loops", &seg.loop_count.to_string(), window, cx));
        body = body.child(value_row("Timeline span", format!("{} \u{2192} {}", fmt_f32(a), fmt_f32(b)), cx));
        if let Some(d) = duration {
            body = body.child(value_row("Clip length", format!("{} s", fmt_f32(d)), cx));
        }
        let (t, s1, s2, s3, s4) = (track.to_string(), seg.id.clone(), seg.id.clone(), seg.id.clone(), seg.id.clone());
        let at = self.time;
        body = body.child(
            h_flex()
                .gap_1()
                .child(Button::new("mt-split").label("Split at playhead").small().outline().on_click(cx.listener(move |this, _, window, cx| {
                    let s = s1.clone();
                    edit(this, window, cx, |m| m.split_segment(&s, at).map(|_| ()));
                })))
                .child(Button::new("mt-earlier").icon(IconName::ArrowUp).small().ghost().tooltip("Play earlier in the track").on_click(cx.listener(move |this, _, window, cx| {
                    let s = s2.clone();
                    let idx = this.model.asset().tracks.iter().find_map(|t| t.segments.iter().position(|x| x.id == s)).unwrap_or(0);
                    edit(this, window, cx, |m| {
                        m.reorder_segment(&s, idx.saturating_sub(1));
                        Ok(())
                    });
                })))
                .child(Button::new("mt-later").icon(IconName::ArrowDown).small().ghost().tooltip("Play later in the track").on_click(cx.listener(move |this, _, window, cx| {
                    let s = s3.clone();
                    let idx = this.model.asset().tracks.iter().find_map(|t| t.segments.iter().position(|x| x.id == s)).unwrap_or(0);
                    edit(this, window, cx, |m| {
                        m.reorder_segment(&s, idx + 1);
                        Ok(())
                    });
                }))),
        );
        let _ = t;
        body = body.child(Button::new("mt-del-seg").icon(IconName::Trash).label("Delete Segment").small().danger().on_click(cx.listener(move |this, _, window, cx| {
            let s = s4.clone();
            edit(this, window, cx, |m| {
                m.delete_segment(&s);
                Ok(())
            })
        })));
        body.into_any_element()
    }

    fn slot_inspector(&mut self, track: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(t) = self.model.asset().tracks.iter().find(|t| t.id == track).cloned() else { return empty_inspector(&self.model.selection, "The track no longer exists.", cx) };
        let mut body = v_flex().w_full().gap_2().child(div().text_sm().font_semibold().text_color(cx.theme().foreground).child("Slot Track"));
        body = body.child(self.widgets.text_field("slot.name", "Slot", &t.slot, window, cx));
        body = body.child(self.widgets.text_field("slot.group", "Group", &t.group, window, cx));
        body = body.child(value_row("Segments", t.segments.len().to_string(), cx));
        body = body.child(div().text_xs().text_color(cx.theme().muted_foreground).child("A Slot node with this name in the graph plays this track."));
        let (a, b) = (t.id.clone(), t.id.clone());
        body = body.child(
            h_flex()
                .gap_1()
                .child(Button::new("mt-seg-add").icon(IconName::Plus).label("Add Segment").small().outline().on_click(cx.listener(move |this, _, window, cx| {
                    let a = a.clone();
                    edit(this, window, cx, |m| m.add_segment(&a, None).map(|_| ()));
                })))
                .child(Button::new("mt-track-del").icon(IconName::Trash).label("Delete Track").small().danger().on_click(cx.listener(move |this, _, window, cx| {
                    let b = b.clone();
                    edit(this, window, cx, |m| {
                        m.delete_track(&b);
                        Ok(())
                    })
                }))),
        );
        body.into_any_element()
    }

    fn section_inspector(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(s) = self.model.asset().sections.iter().find(|s| s.id == id).cloned() else { return empty_inspector(&self.model.selection, "The section no longer exists.", cx) };
        let others: Vec<(String, String)> = self.model.asset().sections.iter().map(|o| (o.id.clone(), o.name.clone())).collect();
        let mut body = v_flex().w_full().gap_2().child(div().text_sm().font_semibold().text_color(cx.theme().foreground).child("Section"));
        body = body.child(self.widgets.text_field("sec.name", "Name", &s.name, window, cx));
        body = body.child(self.widgets.text_field("sec.start", "Start (s)", &fmt_f32(s.start), window, cx));
        let next_label = s.next.as_ref().and_then(|n| others.iter().find(|o| &o.0 == n)).map_or("End of montage".to_string(), |o| o.1.clone());
        let (from, current, choices) = (s.id.clone(), s.next.clone(), others.iter().map(|o| o.0.clone()).collect::<Vec<_>>());
        body = body.child(labelled(
            "Next section",
            Button::new("mt-next").label(next_label).small().outline().tooltip("Cycle: end, then each section").on_click(cx.listener(move |this, _, window, cx| {
                let next = match &current {
                    None => choices.first().cloned(),
                    Some(c) => choices.iter().position(|x| x == c).and_then(|p| choices.get(p + 1).cloned()),
                };
                let from = from.clone();
                edit(this, window, cx, |m| m.link_section(&from, next.as_deref()))
            })),
            cx,
        ));
        let order = self.model.play_order(&s.id).join(" \u{2192} ");
        body = body.child(value_row("Plays", order, cx));
        let sid = s.id.clone();
        body = body.child(Button::new("mt-sec-del").icon(IconName::Trash).label("Delete Section").small().danger().on_click(cx.listener(move |this, _, window, cx| {
            let sid = sid.clone();
            edit(this, window, cx, |m| {
                m.delete_section(&sid);
                Ok(())
            })
        })));
        body.into_any_element()
    }

    fn asset_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let a = self.model.asset().clone();
        let status = self.statuses.get("skeleton").cloned().unwrap_or(RefStatus::Unset);
        let root = self.chrome.root.clone();
        let mut body = v_flex().w_full().gap_2().child(div().text_sm().font_semibold().text_color(cx.theme().foreground).child("Montage"));
        body = body.child(self.widgets.text_field("name", "Name", &a.name, window, cx));
        body = body.child(self.widgets.ref_field("skeleton", "Skeleton", SkeletonAssetKind::KIND, None, a.skeleton.as_ref().map(|s| s.as_str().to_string()), &status, &root, window, cx));
        body = body.child(value_row("Length", format!("{} s", fmt_f32(self.model.length())), cx));
        let curve_button = |id: &'static str, blend_in: bool, cur: BlendCurve, d: f32, cx: &mut Context<Self>| {
            Button::new(id).label(format!("{cur:?}")).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                let next = match cur {
                    BlendCurve::Linear => BlendCurve::EaseIn,
                    BlendCurve::EaseIn => BlendCurve::EaseOut,
                    BlendCurve::EaseOut => BlendCurve::EaseInOut,
                    BlendCurve::EaseInOut => BlendCurve::Linear,
                };
                edit(this, window, cx, |m| m.set_blend(blend_in, d, next));
            }))
        };
        body = body.child(section(
            "Blend",
            vec![
                self.widgets.text_field("blend_in", "Blend in (s)", &fmt_f32(a.blend_in.duration), window, cx),
                labelled("In curve", curve_button("mt-curve-in", true, a.blend_in.curve, a.blend_in.duration, cx), cx),
                self.widgets.text_field("blend_out", "Blend out (s)", &fmt_f32(a.blend_out.duration), window, cx),
                labelled("Out curve", curve_button("mt-curve-out", false, a.blend_out.curve, a.blend_out.duration, cx), cx),
                self.widgets.text_field("blend_out_time", "Blend out before end (s)", &a.blend_out_time.map(fmt_f32).unwrap_or_default(), window, cx),
            ],
            cx,
        ));
        let interrupt = a.interrupt;
        let root_motion = a.root_motion;
        body = body.child(section(
            "Playback",
            vec![
                self.widgets.text_field("group", "Group", &a.group, window, cx),
                labelled(
                    "Interrupt",
                    Button::new("mt-interrupt").label(format!("{interrupt:?}")).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                        let next = match interrupt {
                            InterruptPolicy::ReplaceGroup => InterruptPolicy::RejectIfBusy,
                            InterruptPolicy::RejectIfBusy => InterruptPolicy::StartIfHigherPriority,
                            InterruptPolicy::StartIfHigherPriority => InterruptPolicy::Overlap,
                            InterruptPolicy::Overlap => InterruptPolicy::ReplaceGroup,
                        };
                        edit(this, window, cx, |m| {
                            m.set_interrupt(next);
                            Ok(())
                        })
                    })),
                    cx,
                ),
                self.widgets.text_field("priority", "Priority", &a.priority.to_string(), window, cx),
                labelled(
                    "Root motion",
                    Button::new("mt-rootmotion").label(format!("{root_motion:?}")).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                        let next = match root_motion {
                            MontageRootMotion::Montage => MontageRootMotion::Graph,
                            MontageRootMotion::Graph => MontageRootMotion::Ignore,
                            MontageRootMotion::Ignore => MontageRootMotion::Montage,
                        };
                        edit(this, window, cx, |m| {
                            m.set_root_motion(next);
                            Ok(())
                        })
                    })),
                    cx,
                ),
            ],
            cx,
        ));
        body = body.child(notify_inspector::schema_section(self, window, cx));
        // layered preview at the playhead
        let active = self.model.active_segments(self.time);
        let mut rows = vec![value_row("Playhead", format!("{} s", fmt_f32(self.time)), cx)];
        if active.is_empty() {
            rows.push(div().text_xs().text_color(cx.theme().muted_foreground).child("No slot plays at this time.").into_any_element());
        }
        for s in active {
            let name = self.model.asset().segments().find(|g| g.id == s.segment).map(|g| self.seq_label(&g.sequence)).unwrap_or_default();
            rows.push(value_row(&s.slot, format!("{name} @ {}", fmt_f32(s.clip_time)), cx));
        }
        body = body.child(section("Layered slots at playhead (not saved)", rows, cx));
        body.into_any_element()
    }
}

impl EditorHost for MontageEditor {
    fn widgets(&mut self) -> &mut Widgets {
        &mut self.widgets
    }

    fn commit_field(&mut self, key: &str, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let value = parse_f32(&text);
        let selection = self.model.selection.clone();
        edit(self, window, cx, |m| {
            let number = |what: &str| value.ok_or_else(|| format!("{what} must be a number"));
            if let AnimSelection::Notify { id } | AnimSelection::NotifyState { id } = &selection {
                if let Some(result) = notify_inspector::commit(m, id, key, &text) {
                    return result;
                }
            }
            if key.starts_with("ns.") {
                if let Some(result) = notify_inspector::commit(m, "", key, &text) {
                    return result;
                }
            }
            match (key, &selection) {
                ("name", _) => {
                    m.rename(&text);
                    Ok(())
                }
                ("blend_in", _) => {
                    let curve = m.asset().blend_in.curve;
                    m.set_blend(true, number("blend in")?, curve)
                }
                ("blend_out", _) => {
                    let curve = m.asset().blend_out.curve;
                    m.set_blend(false, number("blend out")?, curve)
                }
                ("blend_out_time", _) => m.set_blend_out_time(if text.trim().is_empty() { None } else { Some(number("blend out time")?) }),
                ("group", _) => m.set_group(&text),
                ("priority", _) => text.trim().parse::<i32>().map(|p| {
                    m.set_priority(p);
                }).map_err(|_| "priority must be a whole number".to_string()),
                ("slot.name", AnimSelection::MontageSlot { track }) | ("slot.group", AnimSelection::MontageSlot { track }) => {
                    let t = m.asset().tracks.iter().find(|t| &t.id == track).cloned().ok_or("no such track")?;
                    if key == "slot.name" { m.rename_track(track, &text, &t.group) } else { m.rename_track(track, &t.slot, &text) }
                }
                ("sec.name", AnimSelection::MontageSection { id }) => m.rename_section(id, &text),
                ("sec.start", AnimSelection::MontageSection { id }) => {
                    m.move_section(id, number("start")?);
                    Ok(())
                }
                (k, AnimSelection::MontageSegment { id, .. }) if k.starts_with("seg.") => {
                    let seg = m.asset().segments().find(|s| &s.id == id).cloned().ok_or("no such segment")?;
                    match k {
                        "seg.start" => {
                            let start = number("start")?.max(0.0);
                            m.move_segment(id, start, None, false);
                            Ok(())
                        }
                        "seg.clip_start" => m.trim_segment(id, number("trim start")?, seg.clip_end),
                        "seg.clip_end" => m.trim_segment(id, seg.clip_start, if text.trim().is_empty() { None } else { Some(number("trim end")?) }),
                        "seg.rate" => m.set_segment_rate(id, number("play rate")?),
                        "seg.loops" => text.trim().parse::<u32>().map_err(|_| "loops must be a whole number".to_string()).and_then(|n| m.set_segment_loops(id, n)),
                        _ => Ok(()),
                    }
                }
                _ => Ok(()),
            }
        });
        self.refresh_refs();
    }

    fn pick_ref(&mut self, key: &str, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        edit(self, window, cx, |m| {
            if key == "skeleton" {
                let skeleton = id.as_deref().and_then(AssetRef::<SkeletonAssetKind>::parse);
                m.doc.apply("Set skeleton", |a| a.skeleton = skeleton);
                return Ok(());
            }
            let segment = key.strip_prefix("seg:").ok_or("unknown reference")?;
            m.set_segment_sequence(segment, id.as_deref().and_then(AssetRef::<AnimationSequenceAssetKind>::parse));
            Ok(())
        });
        self.refresh_refs();
    }
}

impl EditorBody for MontageEditor {
    type Model = MontageModel;
    fn model(&self) -> &MontageModel {
        &self.model
    }
    fn model_mut(&mut self) -> &mut MontageModel {
        &mut self.model
    }
    fn chrome(&mut self) -> &mut Chrome {
        &mut self.chrome
    }
    fn chrome_ref(&self) -> &Chrome {
        &self.chrome
    }
    fn icon(&self) -> IconName {
        IconName::Keyframes
    }
    fn kind_title(&self) -> &'static str {
        "Animation Montage"
    }
    fn left_title(&self) -> &'static str {
        "Tracks & Sections"
    }

    fn after_change(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        // durations of newly assigned sequences
        let missing: Vec<String> = self.model.asset().segments().filter_map(|s| s.sequence.as_ref()).map(|s| s.as_str().to_string()).filter(|id| !self.model.durations.contains_key(id)).collect();
        for id in missing {
            if let Some(d) = sequence_duration(&self.chrome.root, &id) {
                self.model.durations.insert(id, d);
            }
        }
    }

    fn select_location(&mut self, location: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let a = self.model.asset().clone();
        if let Some(t) = a.tracks.iter().find(|t| t.segments.iter().any(|s| s.id == location)) {
            self.model.select_segment(&t.id.clone(), location);
        } else if a.sections.iter().any(|s| s.id == location) {
            self.model.select_section(location);
        } else {
            self.model.select_notify(location);
        }
        cx.notify();
    }

    fn toolbar_extra(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let playing = self.playing;
        let btn = |id: &'static str, icon: IconName, label: &'static str| Button::new(id).icon(icon).label(label).small().ghost();
        vec![
            Button::new("mt-play")
                .icon(if playing { IconName::Pause } else { IconName::Play })
                .small()
                .ghost()
                .tooltip(if playing { "Pause the preview playhead" } else { "Play the preview playhead" })
                .on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx)))
                .into_any_element(),
            Button::new("mt-stop").icon(IconName::Square).small().ghost().tooltip("Stop and rewind").on_click(cx.listener(|this, _, _, cx| {
                this.playing = false;
                this.time = 0.0;
                cx.notify();
            })).into_any_element(),
            btn("mt-add-track", IconName::Plus, "Slot Track").tooltip("Add a slot track").on_click(cx.listener(|this, _, window, cx| {
                let n = this.model.asset().tracks.len() + 1;
                edit(this, window, cx, |m| m.add_track(&format!("Slot {n}"), "DefaultGroup").map(|_| ()));
            })).into_any_element(),
            btn("mt-add-section", IconName::Plus, "Section").tooltip("Add a section at the playhead").on_click(cx.listener(|this, _, window, cx| {
                let t = this.time;
                edit(this, window, cx, |m| {
                    m.add_section("Section", t);
                    Ok(())
                });
            })).into_any_element(),
            btn("mt-add-notify", IconName::Plus, "Notify").tooltip("Add a notify at the playhead").on_click(cx.listener(|this, _, window, cx| {
                let t = this.time;
                edit(this, window, cx, |m| {
                    m.add_notify("Notify", t);
                    Ok(())
                });
            })).into_any_element(),
            btn("mt-add-state", IconName::Plus, "State").tooltip("Add a notify state from the playhead").on_click(cx.listener(|this, _, window, cx| {
                let t = this.time;
                edit(this, window, cx, |m| {
                    m.add_notify_state("State", t, t + 0.3);
                    Ok(())
                });
            })).into_any_element(),
            Button::new("mt-zoom-out").icon(IconName::Minus).small().ghost().tooltip("Zoom out").on_click(cx.listener(|this, _, _, cx| {
                this.zoom = (this.zoom / 1.25).max(20.0);
                cx.notify();
            })).into_any_element(),
            Button::new("mt-zoom-in").icon(IconName::Plus).small().ghost().tooltip("Zoom in").on_click(cx.listener(|this, _, _, cx| {
                this.zoom = (this.zoom * 1.25).min(800.0);
                cx.notify();
            })).into_any_element(),
        ]
    }

    fn left(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let a = self.model.asset().clone();
        let selected = self.model.selection.clone();
        let mut list = v_flex().w_full();
        for t in &a.tracks {
            let id = t.id.clone();
            let sel = matches!(&selected, AnimSelection::MontageSlot { track } if *track == t.id);
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("lt-{}", t.id)))
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .when(sel, |r| r.bg(theme.accent))
                    .hover(|r| r.bg(theme.secondary))
                    .child(Icon::new(IconName::Keyframes).size(px(13.0)).text_color(theme.muted_foreground))
                    .child(v_flex().flex_1().child(div().text_xs().text_color(theme.foreground).child(t.slot.clone())).child(div().text_xs().text_color(theme.muted_foreground).child(format!("{} \u{b7} {} segments", t.group, t.segments.len()))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.select_slot(&id);
                        cx.notify();
                    })),
            );
        }
        list = list.child(div().px_2().py_1().text_xs().font_semibold().text_color(theme.muted_foreground).child("SECTIONS"));
        if a.sections.is_empty() {
            list = list.child(div().px_2().text_xs().text_color(theme.muted_foreground).child("No sections. Without one the montage plays straight through."));
        }
        for s in &a.sections {
            let id = s.id.clone();
            let sel = matches!(&selected, AnimSelection::MontageSection { id } if *id == s.id);
            let next = s.next.as_ref().and_then(|n| a.sections.iter().find(|o| &o.id == n)).map(|o| format!("\u{2192} {}", o.name)).unwrap_or_else(|| "\u{2192} end".into());
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("ls-{}", s.id)))
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .when(sel, |r| r.bg(theme.accent))
                    .hover(|r| r.bg(theme.secondary))
                    .child(div().flex_1().text_xs().text_color(theme.foreground).child(s.name.clone()))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(format!("{} s {next}", fmt_f32(s.start))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.select_section(&id);
                        cx.notify();
                    })),
            );
        }
        list.into_any_element()
    }

    fn center(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.timeline(cx)
    }

    fn inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.model.selection.clone() {
            AnimSelection::MontageSegment { track, id } => self.segment_inspector(&track, &id, window, cx),
            AnimSelection::MontageSlot { track } => self.slot_inspector(&track, window, cx),
            AnimSelection::MontageSection { id } => self.section_inspector(&id, window, cx),
            AnimSelection::Notify { id } | AnimSelection::NotifyState { id } => notify_inspector::inspector(self, &id, window, cx),
            _ => self.asset_inspector(window, cx),
        }
    }
}

impl Render for MontageEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame(self, window, cx)
    }
}

crate::editor_model!(MontageModel, <pulsar_reflection::AnimationMontageAssetKind as AssetKind>::KIND);
crate::anim_panel!(MontageEditor, "animation-montage-editor");


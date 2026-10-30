//! The Blend Space editor: a purpose-built 1D strip or 2D plot with axes, a
//! grid, the runtime's triangulation, selectable and draggable sample points,
//! a sample list, a typed Sequence picker, validation, and a live input whose
//! contributing weights come from the runtime solver. It is not a node graph.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use plugin_editor_api::PluginError;
use pulsar_reflection::{AnimationSequenceAssetKind, AssetKind, AssetRef, SkeletonAssetKind};
use skeletal_assets::blend_space::{BlendSpaceAsset, EdgePolicy, SampleLoop, SyncMode};
use ui::button::{Button, ButtonVariants};
use ui::{h_flex, v_flex, ActiveTheme, Icon, IconName, Selectable, Sizable, StyledExt};

use crate::anim_editors::blend_space_model::{BlendSpaceModel, PlotTransform};
use crate::anim_editors::refs::{self, RefStatus, SnapshotIndex};
use crate::anim_editors::graph_preview::ProjectAssets;
use crate::anim_editors::selection::AnimSelection;
use crate::anim_editors::skin_preview::{BlendSpacePreview, OverlaySource};
use crate::anim_editors::store::FsStore;
use crate::core::asset_index;

use super::chrome::{self, edit, EditorModel, empty_inspector, fmt_f32, frame, labelled, parse_f32, section, short_id, value_row, Chrome, EditorBody, EditorHost, Widgets};
use super::paint::{dot, fill_polygon, pt, stroke_polyline};

const PLOT_PAD: f32 = 28.0;
const HIT_RADIUS: f32 = 10.0;

pub struct BlendSpaceEditor {
    model: BlendSpaceModel,
    chrome: Chrome,
    widgets: Widgets,
    /// Where the plot was last painted, in window coordinates.
    plot_bounds: Rc<Cell<Bounds<Pixels>>>,
    drag: Option<String>,
    scrubbing: bool,
    statuses: HashMap<String, RefStatus>,
    /// Sample names for the list and plot labels, by sequence id.
    names: HashMap<String, String>,
    /// The skinned preview: the rig and the last frame.
    skin: super::skin_pane::SkinPane,
    /// The space on a transient runtime instance, with the document generation it was built from.
    preview: Option<(u64, Result<BlendSpacePreview, String>)>,
    /// The preview clock runs (the poses animate under the probe point).
    playing: bool,
    frame: u64,
}

impl BlendSpaceEditor {
    pub fn open(path: PathBuf, _window: &mut Window, cx: &mut App) -> Result<Entity<Self>, PluginError> {
        let text = BlendSpaceAsset::default().to_text().unwrap_or_default();
        let model = BlendSpaceModel::open(Arc::new(FsStore), &path, &text).map_err(|message| PluginError::Other { message })?;
        let chrome = Chrome::new(&path, cx);
        asset_index::record_asset(&chrome.root, &path);
        let mut this = Self { model, chrome, widgets: Widgets::default(), plot_bounds: Rc::new(Cell::new(Bounds::default())), drag: None, scrubbing: false, statuses: HashMap::new(), names: HashMap::new(), skin: super::skin_pane::SkinPane::default(), preview: None, playing: false, frame: 0 };
        this.refresh_refs();
        Ok(cx.new(|_| this))
    }

    /// Re-resolve every reference the document holds.
    fn refresh_refs(&mut self) {
        let index = SnapshotIndex::new(&self.chrome.root);
        let skeleton = self.model.asset().skeleton.as_ref().map(|s| s.as_str().to_string());
        self.statuses.clear();
        self.names.clear();
        let samples: Vec<_> = self.model.asset().samples.iter().map(|s| (s.id.clone(), s.sequence.as_ref().map(|r| r.as_str().to_string()))).collect();
        for (id, sequence) in samples {
            let status = refs::ref_status(&index, AnimationSequenceAssetKind::KIND, sequence.as_deref(), skeleton.as_deref());
            if let (RefStatus::Ok { name } | RefStatus::Incompatible { name, .. }, Some(seq)) = (&status, &sequence) {
                self.names.insert(seq.clone(), name.clone());
            }
            self.statuses.insert(id, status);
        }
        let skeleton_status = refs::ref_status(&index, SkeletonAssetKind::KIND, skeleton.as_deref(), None);
        self.statuses.insert("skeleton".into(), skeleton_status);
    }

    fn sample_label(&self, id: &str) -> String {
        let Some(sample) = self.model.asset().samples.iter().find(|s| s.id == id) else { return "?".into() };
        sample.sequence.as_ref().and_then(|s| self.names.get(s.as_str()).cloned()).unwrap_or_else(|| match &sample.sequence {
            Some(s) => format!("Sequence {}", short_id(s.as_str())),
            None => "Empty sample".into(),
        })
    }

    fn transform(&self, bounds: Bounds<Pixels>) -> PlotTransform {
        PlotTransform::for_asset(self.model.asset(), f32::from(bounds.size.width), f32::from(bounds.size.height), PLOT_PAD)
    }

    fn local(&self, position: Point<Pixels>) -> (f32, f32) {
        let b = self.plot_bounds.get();
        (f32::from(position.x - b.origin.x), f32::from(position.y - b.origin.y))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = self.plot_bounds.get();
        if bounds.size.width <= px(1.0) {
            return;
        }
        let t = self.transform(bounds);
        let (px_, py_) = self.local(event.position);
        match self.model.hit_sample(&t, px_, py_, HIT_RADIUS) {
            Some(id) => {
                self.model.select_sample(&id);
                self.drag = Some(id);
            }
            None => {
                let (x, y) = t.from_plot(px_, py_);
                if event.click_count >= 2 {
                    self.model.add_sample(x, if self.model.asset().is_2d() { y } else { 0.0 }, None);
                    self.refresh_refs();
                } else {
                    self.model.clear_selection();
                    self.model.set_input(x, if self.model.asset().is_2d() { y } else { 0.0 });
                    self.scrubbing = true;
                }
            }
        }
        self.after_change(window, cx);
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !event.dragging() {
            return;
        }
        let bounds = self.plot_bounds.get();
        let t = self.transform(bounds);
        let (px_, py_) = self.local(event.position);
        let (x, y) = t.from_plot(px_, py_);
        let two_d = self.model.asset().is_2d();
        if let Some(id) = self.drag.clone() {
            self.model.move_sample(&id, x, if two_d { y } else { 0.0 });
            cx.notify();
        } else if self.scrubbing {
            self.model.set_input(x, if two_d { y } else { 0.0 });
            cx.notify();
        }
    }

    fn mouse_up(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            self.model.end_gesture();
        }
        self.scrubbing = false;
        cx.notify();
    }

    fn plot(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let asset = self.model.asset().clone();
        let preview = self.model.preview();
        let selected = self.model.selected_sample().map(|s| s.id.clone());
        let invalid: Vec<String> = self.model.validation().iter().filter_map(|d| d.location.clone()).collect();
        let input = preview.clamped_input;
        let bounds_cell = self.plot_bounds.clone();
        let weights: HashMap<String, f32> = preview.weights.iter().cloned().collect();
        let triangles = preview.triangles.clone();
        let (grid, axis, accent, good, bad, text) = (theme.border.opacity(0.7), theme.muted_foreground, theme.primary, theme.success, theme.danger, theme.foreground);
        let two_d = asset.is_2d();
        let ids: HashMap<String, (f32, f32)> = asset.samples.iter().map(|s| (s.id.clone(), (s.x, s.y.unwrap_or(0.5)))).collect();

        div()
            .id("blend-plot")
            .size_full()
            .min_h(px(240.0))
            .bg(theme.background)
            .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, window, cx| this.mouse_down(e, window, cx)))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| this.mouse_move(e, window, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, window, cx| this.mouse_up(window, cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, window, cx| this.mouse_up(window, cx)))
            .child(
                canvas(
                    move |bounds, _, _| {
                        bounds_cell.set(bounds);
                        bounds
                    },
                    move |bounds, _, window, _| {
                        let t = PlotTransform::for_asset(&asset, f32::from(bounds.size.width), f32::from(bounds.size.height), PLOT_PAD);
                        let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                        let at = |x: f32, y: f32| {
                            let (a, b) = t.to_plot(x, y);
                            pt(ox + a, oy + b)
                        };
                        let (y0, y1) = (t.y_min, t.y_max);
                        let x_div = asset.x_axis.grid_divisions.max(1);
                        // grid and axis ticks
                        for i in 0..=x_div {
                            let x = asset.x_axis.min + asset.x_axis.span() * i as f32 / x_div as f32;
                            stroke_polyline(window, &[at(x, y0), at(x, y1)], if i == 0 { 1.5 } else { 1.0 }, if i == 0 { axis } else { grid });
                        }
                        if let Some(y_axis) = &asset.y_axis {
                            for i in 0..=y_axis.grid_divisions.max(1) {
                                let y = y_axis.min + y_axis.span() * i as f32 / y_axis.grid_divisions.max(1) as f32;
                                stroke_polyline(window, &[at(asset.x_axis.min, y), at(asset.x_axis.max, y)], if i == 0 { 1.5 } else { 1.0 }, if i == 0 { axis } else { grid });
                            }
                        } else {
                            let mid = 0.5;
                            stroke_polyline(window, &[at(asset.x_axis.min, mid), at(asset.x_axis.max, mid)], 2.0, grid);
                        }
                        // triangulation
                        for tri in &triangles {
                            let pts: Vec<_> = tri.iter().filter_map(|id| ids.get(id)).map(|(x, y)| at(*x, *y)).collect();
                            if pts.len() == 3 {
                                let mut closed = pts.clone();
                                closed.push(pts[0]);
                                stroke_polyline(window, &closed, 1.0, axis.opacity(0.6));
                            }
                        }
                        // contributing samples joined to the input point
                        let input_at = at(input.0, if two_d { input.1 } else { 0.5 });
                        if two_d && weights.len() >= 3 {
                            let pts: Vec<_> = weights.keys().filter_map(|id| ids.get(id)).map(|(x, y)| at(*x, *y)).collect();
                            if pts.len() >= 3 {
                                fill_polygon(window, &pts, accent.opacity(0.12));
                            }
                        }
                        for (id, w) in &weights {
                            if let Some((x, y)) = ids.get(id) {
                                let p = at(*x, if two_d { *y } else { 0.5 });
                                stroke_polyline(window, &[input_at, p], 1.0 + 3.0 * *w, good.opacity(0.35 + 0.5 * *w));
                            }
                        }
                        // sample points
                        for s in &asset.samples {
                            let p = at(s.x, if two_d { s.y.unwrap_or(0.0) } else { 0.5 });
                            let weight = weights.get(&s.id).copied().unwrap_or(0.0);
                            let is_selected = selected.as_deref() == Some(s.id.as_str());
                            let color = if invalid.contains(&s.id) { bad } else if weight > 0.0 { good } else { text.opacity(0.75) };
                            if is_selected {
                                dot(window, p, 11.0, accent.opacity(0.35));
                            }
                            dot(window, p, 6.5, color);
                            dot(window, p, 3.0, theme.background);
                        }
                        // live input
                        dot(window, input_at, 5.0, accent);
                        stroke_polyline(window, &[pt(f32::from(input_at.x) - 9.0, f32::from(input_at.y)), pt(f32::from(input_at.x) + 9.0, f32::from(input_at.y))], 1.5, accent);
                        stroke_polyline(window, &[pt(f32::from(input_at.x), f32::from(input_at.y) - 9.0), pt(f32::from(input_at.x), f32::from(input_at.y) + 9.0)], 1.5, accent);
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

impl EditorHost for BlendSpaceEditor {
    fn widgets(&mut self) -> &mut Widgets {
        &mut self.widgets
    }

    fn commit_field(&mut self, key: &str, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let value = parse_f32(&text);
        let selected = self.model.selected_sample().map(|s| s.id.clone());
        edit(self, window, cx, |m| {
            let number = |what: &str| value.ok_or_else(|| format!("{what} must be a number"));
            match key {
                "name" => {
                    m.rename(&text);
                    Ok(())
                }
                "input.x" => {
                    let y = m.input.1;
                    m.set_input(number("input")?, y);
                    Ok(())
                }
                "input.y" => {
                    let x = m.input.0;
                    m.set_input(x, number("input")?);
                    Ok(())
                }
                "axis.name" | "axis.min" | "axis.max" | "axis.divisions" | "axis.smoothing" | "axis.v.name" | "axis.v.min" | "axis.v.max" | "axis.v.divisions" | "axis.v.smoothing" => {
                    let vertical = key.starts_with("axis.v.");
                    let field = key.rsplit('.').next().unwrap_or("");
                    let n = if field == "name" { 0.0 } else { number(field)? };
                    m.set_axis(vertical, |a| match field {
                        "name" => a.name = text.trim().to_string(),
                        "min" => a.min = n,
                        "max" => a.max = n,
                        "divisions" => a.grid_divisions = n.max(0.0) as u32,
                        _ => a.smoothing_time = n,
                    })
                }
                k if k.starts_with("sample.") => {
                    let id = selected.clone().ok_or("no sample selected")?;
                    match k {
                        "sample.x" => m.set_sample_position(&id, Some(number("x")?), None),
                        "sample.y" => m.set_sample_position(&id, None, Some(number("y")?)),
                        "sample.rate" => m.set_sample_rate(&id, number("play rate")?),
                        "sample.phase" => m.set_sample_phase(&id, number("phase offset")?),
                        "sample.sync" => {
                            m.set_sample_sync_group(&id, Some(text.clone()));
                            Ok(())
                        }
                        _ => Ok(()),
                    }
                }
                _ => Ok(()),
            }
        });
        self.refresh_refs();
    }

    fn pick_ref(&mut self, key: &str, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let parsed = id.as_deref().and_then(AssetRef::<AnimationSequenceAssetKind>::parse);
        edit(self, window, cx, |m| {
            if key == "skeleton" {
                let skeleton = id.as_deref().and_then(AssetRef::<SkeletonAssetKind>::parse);
                m.doc.apply("Set skeleton", |a| a.skeleton = skeleton);
                return Ok(());
            }
            let sample = key.strip_prefix("seq:").ok_or("unknown reference")?;
            m.set_sample_sequence(sample, parsed);
            Ok(())
        });
        self.refresh_refs();
    }
}

impl EditorBody for BlendSpaceEditor {
    type Model = BlendSpaceModel;
    fn model(&self) -> &BlendSpaceModel {
        &self.model
    }
    fn model_mut(&mut self) -> &mut BlendSpaceModel {
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
        "Blend Space"
    }
    fn left_title(&self) -> &'static str {
        "Samples"
    }

    fn after_change(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        if self.chrome.aux_generation != Some(self.model.generation()) {
            self.refresh_refs();
        }
    }

    fn select_location(&mut self, location: &str, _window: &mut Window, cx: &mut Context<Self>) {
        self.model.select_sample(location);
        cx.notify();
    }

    fn toolbar_extra(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let two_d = self.model.asset().is_2d();
        let snap = self.model.snap_to_grid;
        vec![
            Button::new("bs-add")
                .icon(IconName::Plus)
                .label("Add Sample")
                .small()
                .ghost()
                .tooltip("Add a sample at the live input (or double-click the plot)")
                .on_click(cx.listener(|this, _, window, cx| {
                    let (x, y) = this.model.input;
                    let two_d = this.model.asset().is_2d();
                    edit(this, window, cx, |m| {
                        m.add_sample(x, if two_d { y } else { 0.0 }, None);
                        Ok(())
                    });
                    this.refresh_refs();
                }))
                .into_any_element(),
            Button::new("bs-dim")
                .label(if two_d { "2D" } else { "1D" })
                .small()
                .ghost()
                .tooltip("Switch between a 1D strip and a 2D plot")
                .on_click(cx.listener(move |this, _, window, cx| {
                    edit(this, window, cx, |m| {
                        m.set_two_dimensional(!two_d);
                        Ok(())
                    })
                }))
                .into_any_element(),
            Button::new("bs-snap")
                .label("Snap")
                .small()
                .ghost()
                .selected(snap)
                .tooltip("Snap samples to the grid while dragging")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.model.snap_to_grid = !this.model.snap_to_grid;
                    cx.notify();
                }))
                .into_any_element(),
        ]
    }

    fn left(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let preview = self.model.preview();
        let weights: HashMap<String, f32> = preview.weights.iter().cloned().collect();
        let selected = self.model.selected_sample().map(|s| s.id.clone());
        let two_d = self.model.asset().is_2d();
        let mut list = v_flex().w_full();
        if self.model.asset().samples.is_empty() {
            list = list.child(div().p_3().text_xs().text_color(theme.muted_foreground).child("No samples yet. Double-click the plot or use Add Sample."));
        }
        let samples = self.model.asset().samples.clone();
        for s in samples {
            let id = s.id.clone();
            let w = weights.get(&s.id).copied().unwrap_or(0.0);
            let problem = self.statuses.get(&s.id).is_some_and(RefStatus::is_problem) || s.sequence.is_none();
            let coords = if two_d { format!("{}, {}", fmt_f32(s.x), fmt_f32(s.y.unwrap_or(0.0))) } else { fmt_f32(s.x) };
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("bs-row-{}", s.id)))
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .when(selected.as_deref() == Some(s.id.as_str()), |r| r.bg(theme.accent))
                    .hover(|r| r.bg(theme.secondary))
                    .child(Icon::new(if problem { IconName::TriangleAlert } else { IconName::Keyframes }).size(px(13.0)).text_color(if problem { theme.danger } else { theme.muted_foreground }))
                    .child(v_flex().flex_1().min_w_0().child(div().text_xs().text_color(theme.foreground).child(self.sample_label(&s.id))).child(div().text_xs().text_color(theme.muted_foreground).child(coords)))
                    .child(div().text_xs().text_color(if w > 0.0 { theme.success } else { theme.muted_foreground }).child(format!("{:.0}%", w * 100.0)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.select_sample(&id);
                        cx.notify();
                    })),
            );
        }
        list.into_any_element()
    }

    fn center(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let asset = self.model.asset();
        let x_label = format!("{}  {} \u{2192} {}", asset.x_axis.name, fmt_f32(asset.x_axis.min), fmt_f32(asset.x_axis.max));
        let y_label = asset.y_axis.as_ref().map(|y| format!("{}  {} \u{2192} {}", y.name, fmt_f32(y.min), fmt_f32(y.max)));
        let hint = if asset.is_2d() { "Double-click to add a sample \u{b7} drag a point to move it \u{b7} drag on empty space to set the input" } else { "Double-click to add a sample \u{b7} drag a point to move it \u{b7} drag to set the input" };
        let plot = self.plot(cx);
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .px_3()
                    .h(px(26.0))
                    .flex_shrink_0()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("X: {x_label}"))
                    .children(y_label.map(|l| format!("Y: {l}")))
                    .child(div().flex_1())
                    .child(hint),
            )
            .child(div().flex_1().min_h_0().child(plot))
            .into_any_element()
    }

    fn inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let preview = self.skin_section(cx);
        let inner = self.selection_inspector(window, cx);
        v_flex().w_full().gap_2().child(preview).child(inner).into_any_element()
    }
}

impl BlendSpaceEditor {
    /// Run or pause the preview clock; while it runs the poses animate.
    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        self.playing = !self.playing;
        if self.playing {
            let weak = cx.weak_entity();
            cx.spawn(async move |_, cx| {
                loop {
                    smol::Timer::after(std::time::Duration::from_millis(33)).await;
                    let keep = weak.update(cx, |this, cx| {
                        if !this.playing {
                            return false;
                        }
                        if let Some((_, Ok(preview))) = &mut this.preview {
                            preview.advance(0.033);
                            this.frame += 1;
                        }
                        cx.notify();
                        true
                    });
                    if !matches!(keep, Ok(true)) {
                        break;
                    }
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// The skinned preview at the probe point, driven by the runtime.
    fn skin_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let root = self.chrome.root.clone();
        let skeleton = self.model.asset().skeleton.as_ref().map(|s| s.as_str().to_string());
        let mut problem = self.skin.rig(&root, skeleton.as_deref()).err();
        let generation = self.model.generation();
        if problem.is_none() && self.preview.as_ref().is_none_or(|(g, _)| *g != generation) {
            let assets = ProjectAssets::new(&root);
            let source = OverlaySource { base: &assets, blend_space: Some(Arc::new(self.model.asset().clone())), montage: None };
            let built = match self.skin.rig(&root, skeleton.as_deref()) {
                Ok(rig) => BlendSpacePreview::build(self.model.asset(), rig, &source).map_err(|d| d.first().map(|d| d.message.clone()).unwrap_or_default()),
                Err(e) => Err(e),
            };
            self.preview = Some((generation, built));
        }
        let input = self.model.preview().clamped_input;
        if problem.is_none() {
            let palette = crate::style::Palette::from_theme(cx.theme());
            match &mut self.preview {
                Some((_, Ok(preview))) => {
                    preview.set_input(input.0, input.1);
                    let pose = preview.pose();
                    let key = generation.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ u64::from(input.0.to_bits()) ^ (u64::from(input.1.to_bits()) << 21) ^ self.frame.wrapping_mul(0xA24B_AED4_963E_E407);
                    self.skin.draw(&pose, key, &palette);
                }
                Some((_, Err(e))) => problem = Some(e.clone()),
                None => {}
            }
        }
        let label = if self.playing { "Pause" } else { "Play" };
        let toggle = Button::new("blend-space-preview-play").label(label).ghost().xsmall().on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx))).into_any_element();
        section("Skinned preview (runtime, no events)", vec![toggle, self.skin.element(problem.as_deref(), cx)], cx)
    }

    fn selection_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let selection = self.model.selection.clone();
        let root = self.chrome.root.clone();
        let skeleton_id = self.model.asset().skeleton.as_ref().map(|s| s.as_str().to_string());
        let mut body = v_flex().w_full().gap_2();
        match &selection {
            AnimSelection::BlendSample { id } => {
                let Some(s) = self.model.asset().samples.iter().find(|s| &s.id == id).cloned() else {
                    return empty_inspector(&selection, "The sample no longer exists.", cx);
                };
                let two_d = self.model.asset().is_2d();
                let status = self.statuses.get(&s.id).cloned().unwrap_or(RefStatus::Unset);
                let seq = s.sequence.as_ref().map(|r| r.as_str().to_string());
                body = body.child(div().text_sm().font_semibold().text_color(theme.foreground).child("Blend Sample"));
                body = body.child(self.widgets.ref_field(&format!("seq:{}", s.id), "Sequence", AnimationSequenceAssetKind::KIND, skeleton_id.clone(), seq.clone(), &status, &root, window, cx));
                if let Some(seq_id) = seq.clone() {
                    if let Some(path) = refs::ProjectRefIndex::new(&root).locate_path(AnimationSequenceAssetKind::KIND, &seq_id) {
                        body = body.child(
                            Button::new("bs-open-seq")
                                .icon(IconName::ExternalLink)
                                .label("Open Sequence")
                                .small()
                                .ghost()
                                .on_click(move |_, window, cx| window.dispatch_action(Box::new(plugin_editor_api::OpenAsset { path: path.clone() }), cx)),
                        );
                    }
                }
                body = body.child(self.widgets.text_field("sample.x", if two_d { "X" } else { "Position" }, &fmt_f32(s.x), window, cx));
                if two_d {
                    body = body.child(self.widgets.text_field("sample.y", "Y", &fmt_f32(s.y.unwrap_or(0.0)), window, cx));
                }
                body = body.child(self.widgets.text_field("sample.rate", "Play rate", &fmt_f32(s.play_rate), window, cx));
                body = body.child(self.widgets.text_field("sample.phase", "Phase offset", &fmt_f32(s.phase_offset), window, cx));
                body = body.child(self.widgets.text_field("sample.sync", "Sync group", s.sync_group.as_deref().unwrap_or(""), window, cx));
                let looping = s.looping;
                let id = s.id.clone();
                body = body.child(labelled(
                    "Looping",
                    Button::new("bs-loop").label(format!("{looping:?}")).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                        let next = match looping {
                            SampleLoop::Inherit => SampleLoop::Loop,
                            SampleLoop::Loop => SampleLoop::Once,
                            SampleLoop::Once => SampleLoop::Inherit,
                        };
                        let id = id.clone();
                        edit(this, window, cx, |m| {
                            m.set_sample_loop(&id, next);
                            Ok(())
                        })
                    })),
                    cx,
                ));
                let w = self.model.preview().weights.iter().find(|(i, _)| *i == s.id).map_or(0.0, |(_, w)| *w);
                body = body.child(value_row("Weight at input", format!("{:.1}%", w * 100.0), cx));
                let id = s.id.clone();
                body = body.child(Button::new("bs-delete").icon(IconName::Trash).label("Delete Sample").small().danger().on_click(cx.listener(move |this, _, window, cx| {
                    let id = id.clone();
                    edit(this, window, cx, |m| {
                        m.delete_sample(&id);
                        Ok(())
                    })
                })));
            }
            AnimSelection::BlendAxis { vertical } => {
                let vertical = *vertical;
                let axis = if vertical { self.model.asset().y_axis.clone() } else { Some(self.model.asset().x_axis.clone()) };
                let Some(a) = axis else { return empty_inspector(&selection, "There is no vertical axis in a 1D blend space.", cx) };
                let p = if vertical { "axis.v." } else { "axis." };
                body = body.child(div().text_sm().font_semibold().text_color(theme.foreground).child(if vertical { "Vertical axis" } else { "Horizontal axis" }));
                body = body.child(self.widgets.text_field(&format!("{p}name"), "Name", &a.name, window, cx));
                body = body.child(self.widgets.text_field(&format!("{p}min"), "Minimum", &fmt_f32(a.min), window, cx));
                body = body.child(self.widgets.text_field(&format!("{p}max"), "Maximum", &fmt_f32(a.max), window, cx));
                body = body.child(self.widgets.text_field(&format!("{p}divisions"), "Grid divisions", &a.grid_divisions.to_string(), window, cx));
                body = body.child(self.widgets.text_field(&format!("{p}smoothing"), "Smoothing time (s)", &fmt_f32(a.smoothing_time), window, cx));
                let edge = a.edge;
                body = body.child(labelled(
                    "Out of range",
                    Button::new("bs-edge").label(format!("{edge:?}")).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                        edit(this, window, cx, |m| m.set_edge_policy(vertical, if edge == EdgePolicy::Clamp { EdgePolicy::Wrap } else { EdgePolicy::Clamp }))
                    })),
                    cx,
                ));
            }
            _ => {
                let a = self.model.asset().clone();
                let status = self.statuses.get("skeleton").cloned().unwrap_or(RefStatus::Unset);
                body = body.child(div().text_sm().font_semibold().text_color(theme.foreground).child("Blend Space"));
                body = body.child(self.widgets.text_field("name", "Name", &a.name, window, cx));
                body = body.child(self.widgets.ref_field("skeleton", "Skeleton", SkeletonAssetKind::KIND, None, skeleton_id.clone(), &status, &root, window, cx));
                body = body.child(value_row("Samples", a.samples.len().to_string(), cx));
                body = body.child(section(
                    "Axes",
                    vec![
                        Button::new("bs-sel-x").label(format!("Horizontal: {}", a.x_axis.name)).small().outline().on_click(cx.listener(|this, _, _, cx| {
                            this.model.select_axis(false);
                            cx.notify();
                        })).into_any_element(),
                        a.y_axis
                            .as_ref()
                            .map(|y| Button::new("bs-sel-y").label(format!("Vertical: {}", y.name)).small().outline().on_click(cx.listener(|this, _, _, cx| {
                                this.model.select_axis(true);
                                cx.notify();
                            })).into_any_element())
                            .unwrap_or_else(|| div().into_any_element()),
                    ],
                    cx,
                ));
                let mode = a.sync.mode;
                let leader = a.sync.leader.clone();
                let mut sync_rows = vec![labelled(
                    "Sync",
                    Button::new("bs-sync").label(format!("{mode:?}")).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                        let next = match mode {
                            SyncMode::None => SyncMode::Phase,
                            SyncMode::Phase => SyncMode::Markers,
                            SyncMode::Markers => SyncMode::None,
                        };
                        edit(this, window, cx, |m| {
                            m.set_sync_mode(next);
                            Ok(())
                        })
                    })),
                    cx,
                )];
                if mode != SyncMode::None {
                    let ids: Vec<String> = a.samples.iter().map(|s| s.id.clone()).collect();
                    let current = leader.clone();
                    sync_rows.push(labelled(
                        "Leader",
                        Button::new("bs-leader")
                            .label(leader.as_deref().map(|l| self.sample_label(l)).unwrap_or_else(|| "Highest weight".into()))
                            .small()
                            .outline()
                            .tooltip("Cycle through the samples")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let next = match &current {
                                    None => ids.first().cloned(),
                                    Some(c) => ids.iter().position(|i| i == c).and_then(|p| ids.get(p + 1).cloned()),
                                };
                                edit(this, window, cx, |m| m.set_sync_leader(next))
                            })),
                        cx,
                    ));
                }
                body = body.child(section("Synchronisation", sync_rows, cx));
                let (ix, iy) = self.model.input;
                let mut live = vec![self.widgets.text_field("input.x", &a.x_axis.name, &fmt_f32(ix), window, cx)];
                if let Some(y) = &a.y_axis {
                    live.push(self.widgets.text_field("input.y", &y.name, &fmt_f32(iy), window, cx));
                }
                for (id, w) in self.model.preview().weights {
                    live.push(value_row(&self.sample_label(&id), format!("{:.1}%", w * 100.0), cx));
                }
                body = body.child(section("Live input (not saved)", live, cx));
            }
        }
        body.into_any_element()
    }
}


impl Render for BlendSpaceEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame(self, window, cx)
    }
}

crate::editor_model!(BlendSpaceModel, <pulsar_reflection::BlendSpaceAssetKind as AssetKind>::KIND);
crate::anim_panel!(BlendSpaceEditor, "blend-space-editor");

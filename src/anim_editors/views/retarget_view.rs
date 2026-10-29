//! The Retarget Profile and IK Retargeter editors: the source and target
//! skeletons side by side (or overlaid) running a shared sample Sequence
//! through the real retarget plan, a chain table whose missing, unmapped and
//! ambiguous rows carry their fixes, per-chain inspectors, root settings and
//! the retarget-pose tool (click a bone, rotate it).

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use plugin_editor_api::{OpenAsset, PluginError};
use pulsar_reflection::{AnimationSequenceAssetKind, AssetKind, AssetRef, IkRigAssetKind, SkeletonAssetKind};
use skeletal_assets::assets::AnimSeqAsset;
use skeletal_assets::ik_retarget::{FkSettings, IkRetargetAsset, IkSettings};
use skeletal_assets::ik_rig::IkRigAsset;
use skeletal_assets::retarget::{RetargetProfileAsset, RootScale, RotationMode, TranslationMode};
use skeletal_assets::skeleton::Skeleton;
use ui::button::{Button, ButtonVariants};
use ui::{h_flex, v_flex, ActiveTheme, IconName, Sizable, StyledExt};

use crate::anim_editors::refs::{self, ProjectRefIndex, RefStatus, SnapshotIndex};
use crate::anim_editors::retarget_model::{ChainRow, ChainStatus, IkRetargetModel, PoseSide, RetargetProfileModel};
use crate::anim_editors::retarget_preview::RetargetPreview;
use crate::anim_editors::selection::AnimSelection;
use crate::anim_editors::skeleton_proj::SkeletonProjection;
use crate::anim_editors::store::FsStore;
use crate::core::asset_index;

use super::chrome::{edit, empty_inspector, fmt_f32, frame, labelled, parse_f32, section, value_row, Chrome, EditorBody, EditorHost, Widgets};
use super::skeleton_canvas::{self, load_rig, load_skeleton, Overlay};

/// Preview state both editors share. None of it is saved.
struct Bench {
    source_bounds: Rc<Cell<Bounds<Pixels>>>,
    target_bounds: Rc<Cell<Bounds<Pixels>>>,
    preview: Option<RetargetPreview>,
    source_proj: SkeletonProjection,
    target_proj: SkeletonProjection,
    sample_id: Option<String>,
    sample: Option<AnimSeqAsset>,
    time: f32,
    playing: bool,
    overlay: bool,
    /// The bone being rotated by the retarget pose tool.
    pose_bone: Option<(PoseSide, String)>,
}

impl Bench {
    fn new() -> Self {
        Self {
            source_bounds: Rc::new(Cell::new(Bounds::default())),
            target_bounds: Rc::new(Cell::new(Bounds::default())),
            preview: None,
            source_proj: SkeletonProjection::default(),
            target_proj: SkeletonProjection::default(),
            sample_id: None,
            sample: None,
            time: 0.0,
            playing: false,
            overlay: false,
            pose_bone: None,
        }
    }

    fn load_sample(&mut self, root: &std::path::Path, id: Option<String>) {
        self.sample_id = id.clone();
        self.sample = id.and_then(|id| {
            let path = ProjectRefIndex::new(root).locate_path(AnimationSequenceAssetKind::KIND, &id)?;
            let text = String::from_utf8(engine_fs::virtual_fs::read_file(&path).ok()?).ok()?;
            AnimSeqAsset::parse(&text).ok()
        });
        self.time = 0.0;
    }

    fn refresh(&mut self, preview: Result<RetargetPreview, String>, source: Option<&Skeleton>, target: Option<&Skeleton>) {
        self.source_proj = source.map(SkeletonProjection::new).unwrap_or_default();
        self.target_proj = target.map(SkeletonProjection::new).unwrap_or_default();
        self.preview = preview.ok();
        self.pose();
    }

    fn pose(&mut self) {
        if let Some(p) = &self.preview {
            let (s, t) = p.projections_at(self.time);
            self.source_proj = s;
            self.target_proj = t;
        }
    }

    fn duration(&self) -> f32 {
        self.preview.as_ref().map_or(0.0, RetargetPreview::duration)
    }
}

fn skeleton_pane(
    title: &str,
    projection: &SkeletonProjection,
    bounds: &Rc<Cell<Bounds<Pixels>>>,
    overlays: Vec<Overlay>,
    selected: Option<String>,
    pick_target: Rc<dyn Fn(Option<String>, &mut Window, &mut App)>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let element = skeleton_canvas::element(projection.clone(), bounds.clone(), theme.foreground, theme.background, overlays, Vec::new(), selected, theme.primary);
    let (projection, bounds) = (projection.clone(), bounds.clone());
    div()
        .relative()
        .flex_1()
        .h_full()
        .min_w_0()
        .bg(theme.background)
        .border_r_1()
        .border_color(theme.border)
        .child(element)
        .child(div().absolute().top(px(6.0)).left(px(10.0)).text_xs().font_semibold().text_color(theme.muted_foreground).child(title.to_string()))
        .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, window, cx| {
            let bone = skeleton_canvas::pick(&projection, &bounds, e.position, 12.0);
            pick_target(bone, window, cx);
        })
        .into_any_element()
}

fn chain_bones(rig: Option<&IkRigAsset>, skeleton: Option<&Skeleton>, name: &str) -> Vec<String> {
    let (Some(rig), Some(sk)) = (rig, skeleton) else { return Vec::new() };
    rig.chain(name).and_then(|c| rig.chain_bones(sk, c)).unwrap_or_default()
}

fn status_chip(status: &ChainStatus, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (text, color) = match status {
        ChainStatus::Mapped => ("mapped", theme.success),
        ChainStatus::UnmappedTarget => ("unmapped", theme.warning),
        ChainStatus::MissingSource => ("source missing", theme.danger),
        ChainStatus::MissingTarget => ("target missing", theme.danger),
        ChainStatus::Ambiguous { .. } => ("ambiguous", theme.warning),
    };
    div().px_1p5().py_0p5().rounded(px(4.0)).bg(color.opacity(0.2)).text_xs().text_color(color).child(text).into_any_element()
}

// =============================== Retarget Profile ===============================

pub struct RetargetProfileEditor {
    model: RetargetProfileModel,
    chrome: Chrome,
    widgets: Widgets,
    bench: Bench,
    statuses: HashMap<String, RefStatus>,
}

impl RetargetProfileEditor {
    pub fn open(path: PathBuf, _window: &mut Window, cx: &mut App) -> Result<Entity<Self>, PluginError> {
        let text = RetargetProfileAsset::new("New Retarget Profile", None, None).to_text().unwrap_or_default();
        let model = RetargetProfileModel::open(Arc::new(FsStore), &path, &text).map_err(|message| PluginError::Other { message })?;
        let chrome = Chrome::new(&path, cx);
        asset_index::record_asset(&chrome.root, &path);
        let mut this = Self { model, chrome, widgets: Widgets::default(), bench: Bench::new(), statuses: HashMap::new() };
        this.reload_inputs();
        Ok(cx.new(|_| this))
    }

    /// Load the skeletons and rigs the profile names and rebuild the preview.
    fn reload_inputs(&mut self) {
        let root = self.chrome.root.clone();
        let a = self.model.asset();
        let id = |r: &Option<AssetRef<SkeletonAssetKind>>| r.as_ref().map(|s| s.as_str().to_string());
        let (ss, ts) = (id(&a.source_skeleton), id(&a.target_skeleton));
        let (sr, tr) = (a.source_rig.as_ref().map(|r| r.as_str().to_string()), a.target_rig.as_ref().map(|r| r.as_str().to_string()));
        self.model.source_skeleton = ss.as_deref().and_then(|i| load_skeleton(&root, i));
        self.model.target_skeleton = ts.as_deref().and_then(|i| load_skeleton(&root, i));
        self.model.source_rig = sr.as_deref().and_then(|i| load_rig(&root, i));
        self.model.target_rig = tr.as_deref().and_then(|i| load_rig(&root, i));
        let index = SnapshotIndex::new(&root);
        self.statuses.insert("source_skeleton".into(), refs::ref_status(&index, SkeletonAssetKind::KIND, ss.as_deref(), None));
        self.statuses.insert("target_skeleton".into(), refs::ref_status(&index, SkeletonAssetKind::KIND, ts.as_deref(), None));
        self.statuses.insert("source_rig".into(), refs::ref_status(&index, IkRigAssetKind::KIND, sr.as_deref(), ss.as_deref()));
        self.statuses.insert("target_rig".into(), refs::ref_status(&index, IkRigAssetKind::KIND, tr.as_deref(), ts.as_deref()));
        self.rebuild_preview();
    }

    fn rebuild_preview(&mut self) {
        let (s, t) = (self.model.source_skeleton.clone(), self.model.target_skeleton.clone());
        let preview = match (&s, &t) {
            (Some(s), Some(t)) => {
                let rigs = self.model.source_rig.as_ref().zip(self.model.target_rig.as_ref());
                RetargetPreview::for_profile(self.model.asset(), s, t, rigs, self.bench.sample.as_ref())
            }
            _ => Err("pick both skeletons".to_string()),
        };
        self.bench.refresh(preview, s.as_ref(), t.as_ref());
    }

    fn highlight(&self) -> (Vec<Overlay>, Vec<Overlay>, Option<String>) {
        let Some(name) = (match &self.model.selection {
            AnimSelection::RetargetChain { name } => Some(name.clone()),
            _ => None,
        }) else {
            return (Vec::new(), Vec::new(), None);
        };
        let color: Hsla = hsla(0.12, 0.9, 0.6, 1.0);
        let source_chain = self.model.asset().chain_map.iter().find(|m| m.target_chain == name).map(|m| m.source_chain.clone());
        let s = source_chain.map(|c| chain_bones(self.model.source_rig.as_ref(), self.model.source_skeleton.as_ref(), &c)).unwrap_or_default();
        let t = chain_bones(self.model.target_rig.as_ref(), self.model.target_skeleton.as_ref(), &name);
        (vec![Overlay { bones: s, color, width: 5.0 }], vec![Overlay { bones: t, color, width: 5.0 }], None)
    }

    fn pose_inspector(&mut self, side: PoseSide, bone: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let euler = self.model.pose_bone_euler(side, bone);
        let mut rows = Vec::new();
        for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
            rows.push(self.widgets.text_field(&format!("pose.{i}"), &format!("{axis} (deg)"), &fmt_f32(euler[i]), window, cx));
        }
        rows.push(
            Button::new("rt-pose-reset-bone").label("Reset this bone").small().outline().on_click(cx.listener(move |this, _, window, cx| {
                if let Some((side, bone)) = this.bench.pose_bone.clone() {
                    edit(this, window, cx, |m| m.set_pose_bone(side, &bone, [0.0; 3]).map(|_| ()));
                }
            })).into_any_element(),
        );
        rows.push(
            Button::new("rt-pose-reset-all").label("Reset whole pose").small().outline().on_click(cx.listener(move |this, _, window, cx| {
                edit(this, window, cx, |m| {
                    m.reset_pose(side);
                    Ok(())
                })
            })).into_any_element(),
        );
        section(&format!("Retarget pose \u{b7} {} \u{b7} {bone}", if side == PoseSide::Source { "source" } else { "target" }), rows, cx)
    }
}

impl EditorHost for RetargetProfileEditor {
    fn widgets(&mut self) -> &mut Widgets {
        &mut self.widgets
    }

    fn commit_field(&mut self, key: &str, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let v = parse_f32(&text);
        let sel = self.model.selection.clone();
        let pose_bone = self.bench.pose_bone.clone();
        edit(self, window, cx, |m| {
            let n = |what: &str| v.ok_or_else(|| format!("{what} must be a number"));
            match (key, &sel) {
                ("name", _) => {
                    m.rename(&text);
                    Ok(())
                }
                ("root.translation", _) | ("root.rotation", _) => {
                    let (t, r) = (m.asset().root.translation_alpha, m.asset().root.rotation_alpha);
                    if key == "root.translation" { m.set_root_alphas(n("alpha")?, r) } else { m.set_root_alphas(t, n("alpha")?) }
                }
                ("root.scale", _) => {
                    if text.trim().is_empty() || text.trim().eq_ignore_ascii_case("auto") {
                        m.set_root_scale(RootScale::Auto)
                    } else {
                        m.set_root_scale(RootScale::Fixed { factor: n("scale")? })
                    }
                }
                ("chain.rot_alpha", AnimSelection::RetargetChain { name }) => {
                    let c = m.asset().chain_map.iter().find(|c| &c.target_chain == name).cloned().ok_or("that chain is not mapped")?;
                    m.set_chain_rotation(name, c.rotation, n("alpha")?)
                }
                ("chain.tr_alpha", AnimSelection::RetargetChain { name }) => {
                    let c = m.asset().chain_map.iter().find(|c| &c.target_chain == name).cloned().ok_or("that chain is not mapped")?;
                    m.set_chain_translation(name, c.translation, n("alpha")?)
                }
                (k, _) if k.starts_with("pose.") => {
                    let (side, bone) = pose_bone.clone().ok_or("click a bone first")?;
                    let axis = k.trim_start_matches("pose.").parse::<usize>().unwrap_or(0).min(2);
                    let mut euler = m.pose_bone_euler(side, &bone);
                    euler[axis] = n("angle")?;
                    m.set_pose_bone(side, &bone, euler).map(|_| ())
                }
                _ => Ok(()),
            }
        });
        self.rebuild_preview();
    }

    fn pick_ref(&mut self, key: &str, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        edit(self, window, cx, |m| {
            let skel = id.as_deref().and_then(AssetRef::<SkeletonAssetKind>::parse);
            let rig = id.as_deref().and_then(AssetRef::<IkRigAssetKind>::parse);
            match key {
                "source_skeleton" => {
                    m.doc.apply("Set source skeleton", |a| a.source_skeleton = skel);
                    Ok(())
                }
                "target_skeleton" => {
                    m.doc.apply("Set target skeleton", |a| a.target_skeleton = skel);
                    Ok(())
                }
                "source_rig" | "target_rig" => {
                    let (s, t) = (m.asset().source_rig.clone(), m.asset().target_rig.clone());
                    if key == "source_rig" { m.set_rigs(rig, t) } else { m.set_rigs(s, rig) }
                        .or_else(|_| {
                            // one rig alone is not a valid state: hold it until its partner is set
                            Ok::<(), String>(())
                        })
                }
                _ => Ok(()),
            }
        });
        self.reload_inputs();
    }
}

impl EditorBody for RetargetProfileEditor {
    type Model = RetargetProfileModel;
    fn model(&self) -> &RetargetProfileModel {
        &self.model
    }
    fn model_mut(&mut self) -> &mut RetargetProfileModel {
        &mut self.model
    }
    fn chrome(&mut self) -> &mut Chrome {
        &mut self.chrome
    }
    fn chrome_ref(&self) -> &Chrome {
        &self.chrome
    }
    fn icon(&self) -> IconName {
        IconName::Link
    }
    fn kind_title(&self) -> &'static str {
        "Retarget Profile"
    }
    fn left_title(&self) -> &'static str {
        "Chains"
    }

    fn after_change(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.rebuild_preview();
    }

    fn select_location(&mut self, location: &str, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(m) = self.model.asset().chain_map.iter().find(|m| m.id == location) {
            let name = m.target_chain.clone();
            self.model.select_chain(&name);
        }
        cx.notify();
    }

    fn toolbar_extra(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let playing = self.bench.playing;
        let overlay = self.bench.overlay;
        vec![
            Button::new("rt-auto").icon(IconName::Link).label("Auto-map").small().ghost().tooltip("Map unmapped target chains or bones that have one same-named source").on_click(cx.listener(|this, _, window, cx| {
                edit(this, window, cx, |m| {
                    if m.asset().maps_by_chain() { m.auto_map_chains() } else { m.auto_map_bones() };
                    Ok(())
                })
            })).into_any_element(),
            Button::new("rt-play").icon(if playing { IconName::Pause } else { IconName::Play }).small().ghost().tooltip("Play the sample Sequence through the retarget").on_click(cx.listener(|this, _, _, cx| {
                this.bench.playing = !this.bench.playing;
                if this.bench.playing {
                    play_loop(cx, |this: &mut Self| (&mut this.bench, true));
                }
                cx.notify();
            })).into_any_element(),
            Button::new("rt-overlay").label("Overlay").small().ghost().selected_if(overlay).tooltip("Draw both skeletons in one view").on_click(cx.listener(|this, _, _, cx| {
                this.bench.overlay = !this.bench.overlay;
                cx.notify();
            })).into_any_element(),
        ]
    }

    fn left(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let rows = self.model.rows();
        let selected = match &self.model.selection {
            AnimSelection::RetargetChain { name } => Some(name.clone()),
            _ => None,
        };
        let mut list = v_flex().w_full();
        if !self.model.asset().maps_by_chain() {
            list = list.child(div().p_3().text_xs().text_color(theme.muted_foreground).child(format!("Mapping by bone ({} mapped). Set both IK rigs in the inspector to map by chain.", self.model.asset().bone_map.len())));
        }
        for row in rows {
            let name = row.target_chain.clone();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("rt-row-{}", row.target_chain)))
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .when(selected.as_deref() == Some(row.target_chain.as_str()), |d| d.bg(theme.accent))
                    .hover(|d| d.bg(theme.secondary))
                    .child(v_flex().flex_1().min_w_0().child(div().text_xs().text_color(theme.foreground).child(row.target_chain.clone())).child(div().text_xs().text_color(theme.muted_foreground).child(row.source_chain.clone().map_or("\u{2014}".to_string(), |s| format!("\u{2190} {s}")))))
                    .child(status_chip(&row.status, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.select_chain(&name);
                        cx.notify();
                    })),
            );
        }
        list.into_any_element()
    }

    fn center(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let (src_overlay, tgt_overlay, _) = self.highlight();
        let entity = cx.entity();
        let pick = move |side: PoseSide| -> Rc<dyn Fn(Option<String>, &mut Window, &mut App)> {
            let entity = entity.clone();
            Rc::new(move |bone, _window, cx| {
                entity.update(cx, |this, cx| {
                    this.bench.pose_bone = bone.map(|b| (side, b));
                    this.model.selection = AnimSelection::None;
                    cx.notify();
                });
            })
        };
        let selected_of = |side: PoseSide, bench: &Bench| bench.pose_bone.as_ref().filter(|(s, _)| *s == side).map(|(_, b)| b.clone());
        let source = skeleton_pane("SOURCE", &self.bench.source_proj, &self.bench.source_bounds, src_overlay, selected_of(PoseSide::Source, &self.bench), pick(PoseSide::Source), cx);
        let target = skeleton_pane("TARGET", &self.bench.target_proj, &self.bench.target_bounds, tgt_overlay, selected_of(PoseSide::Target, &self.bench), pick(PoseSide::Target), cx);
        let table = self.table(cx);
        let duration = self.bench.duration();
        let scrub = h_flex()
            .w_full()
            .px_3()
            .h(px(26.0))
            .flex_shrink_0()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(format!("Sample: {}", self.bench.sample_id.as_deref().map(|i| format!("{}\u{2026}", &i[..8.min(i.len())])).unwrap_or_else(|| "none (pick one in the inspector)".into())))
            .child(div().flex_1())
            .child(format!("{} / {} s", fmt_f32(self.bench.time), fmt_f32(duration)))
            .child(Button::new("rt-back").icon(IconName::ArrowUp).xsmall().ghost().tooltip("Back to the start").on_click(cx.listener(|this, _, _, cx| {
                this.bench.time = 0.0;
                this.bench.pose();
                cx.notify();
            })))
            .child(Button::new("rt-step").icon(IconName::ChevronRight).xsmall().ghost().tooltip("Step one frame").on_click(cx.listener(|this, _, _, cx| {
                let d = this.bench.duration().max(0.0333);
                this.bench.time = (this.bench.time + 1.0 / 30.0) % d;
                this.bench.pose();
                cx.notify();
            })));
        v_flex()
            .size_full()
            .child(h_flex().flex_1().min_h_0().w_full().child(if self.bench.overlay { self.overlay_pane(cx) } else { h_flex().size_full().child(source).child(target).into_any_element() }))
            .child(scrub)
            .child(div().h(px(150.0)).flex_shrink_0().border_t_1().border_color(theme.border).overflow_y_scroll().child(table))
            .into_any_element()
    }

    fn inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let root = self.chrome.root.clone();
        let theme = cx.theme().clone();
        let a = self.model.asset().clone();
        let mut body = v_flex().w_full().gap_2();
        match self.model.selection.clone() {
            AnimSelection::RetargetChain { name } => {
                let rows = self.model.rows();
                let Some(row) = rows.iter().find(|r| r.target_chain == name).cloned() else { return empty_inspector(&self.model.selection, "The chain no longer exists.", cx) };
                body = body.child(div().text_sm().font_semibold().text_color(theme.foreground).child(format!("Chain: {name}")));
                body = body.child(labelled("Status", status_chip(&row.status, cx), cx));
                body = body.child(value_row("Source chain", row.source_chain.clone().unwrap_or_else(|| "None".into()), cx));
                // fixes
                let mut fixes = Vec::new();
                for s in row.suggestions.iter() {
                    let (s, t) = (s.clone(), name.clone());
                    fixes.push(Button::new(SharedString::from(format!("rt-map-{s}"))).label(format!("Map from {s}")).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                        let (s, t) = (s.clone(), t.clone());
                        edit(this, window, cx, |m| m.map_chain(&s, &t));
                    })).into_any_element());
                }
                if row.source_chain.is_some() {
                    let t = name.clone();
                    fixes.push(Button::new("rt-unmap").label("Clear mapping").small().ghost().on_click(cx.listener(move |this, _, window, cx| {
                        let t = t.clone();
                        edit(this, window, cx, |m| {
                            m.unmap_chain(&t);
                            Ok(())
                        })
                    })).into_any_element());
                }
                if let Some(src) = self.model.source_rig.as_ref().filter(|_| row.status.needs_action()) {
                    for c in src.chains.iter().filter(|c| !row.suggestions.contains(&c.name)).take(6) {
                        let (s, t) = (c.name.clone(), name.clone());
                        fixes.push(Button::new(SharedString::from(format!("rt-any-{s}"))).label(format!("Map from {s}")).small().ghost().on_click(cx.listener(move |this, _, window, cx| {
                            let (s, t) = (s.clone(), t.clone());
                            edit(this, window, cx, |m| m.map_chain(&s, &t));
                        })).into_any_element());
                    }
                }
                if !fixes.is_empty() {
                    body = body.child(section("Fix", fixes, cx));
                }
                if let Some(m) = a.chain_map.iter().find(|m| m.target_chain == name).cloned() {
                    let n1 = name.clone();
                    let n2 = name.clone();
                    let (rot, tr, ra, ta) = (m.rotation, m.translation, m.rotation_alpha, m.translation_alpha);
                    body = body.child(section(
                        "Rotation",
                        vec![
                            labelled(
                                "Mode",
                                Button::new("rt-rot-mode").label(format!("{rot:?}")).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                                    let next = match rot {
                                        RotationMode::Interpolated => RotationMode::OneToOne,
                                        RotationMode::OneToOne => RotationMode::OneToOneReversed,
                                        RotationMode::OneToOneReversed => RotationMode::None,
                                        RotationMode::None => RotationMode::Interpolated,
                                    };
                                    let n = n1.clone();
                                    edit(this, window, cx, |m| m.set_chain_rotation(&n, next, ra));
                                })),
                                cx,
                            ),
                            self.widgets.text_field("chain.rot_alpha", "Alpha", &fmt_f32(ra), window, cx),
                        ],
                        cx,
                    ));
                    body = body.child(section(
                        "Translation",
                        vec![
                            labelled(
                                "Mode",
                                Button::new("rt-tr-mode").label(format!("{tr:?}")).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                                    let next = match tr {
                                        TranslationMode::None => TranslationMode::GloballyScaled,
                                        TranslationMode::GloballyScaled => TranslationMode::Absolute,
                                        TranslationMode::Absolute => TranslationMode::None,
                                    };
                                    let n = n2.clone();
                                    edit(this, window, cx, |m| m.set_chain_translation(&n, next, ta));
                                })),
                                cx,
                            ),
                            self.widgets.text_field("chain.tr_alpha", "Alpha", &fmt_f32(ta), window, cx),
                        ],
                        cx,
                    ));
                }
            }
            _ => {
                if let Some((side, bone)) = self.bench.pose_bone.clone() {
                    body = body.child(self.pose_inspector(side, &bone, window, cx));
                }
                body = body.child(div().text_sm().font_semibold().text_color(theme.foreground).child("Retarget Profile"));
                body = body.child(self.widgets.text_field("name", "Name", &a.name, window, cx));
                let id = |r: &Option<AssetRef<SkeletonAssetKind>>| r.as_ref().map(|s| s.as_str().to_string());
                let rid = |r: &Option<AssetRef<IkRigAssetKind>>| r.as_ref().map(|s| s.as_str().to_string());
                let st = |s: &HashMap<String, RefStatus>, k: &str| s.get(k).cloned().unwrap_or(RefStatus::Unset);
                let (ss, ts) = (id(&a.source_skeleton), id(&a.target_skeleton));
                body = body.child(self.widgets.ref_field("source_skeleton", "Source skeleton", SkeletonAssetKind::KIND, None, ss.clone(), &st(&self.statuses, "source_skeleton"), &root, window, cx));
                body = body.child(self.widgets.ref_field("target_skeleton", "Target skeleton", SkeletonAssetKind::KIND, None, ts.clone(), &st(&self.statuses, "target_skeleton"), &root, window, cx));
                body = body.child(self.widgets.ref_field("source_rig", "Source IK rig", IkRigAssetKind::KIND, ss, rid(&a.source_rig), &st(&self.statuses, "source_rig"), &root, window, cx));
                body = body.child(self.widgets.ref_field("target_rig", "Target IK rig", IkRigAssetKind::KIND, ts, rid(&a.target_rig), &st(&self.statuses, "target_rig"), &root, window, cx));
                if let Some(path) = a.source_rig.as_ref().and_then(|r| ProjectRefIndex::new(&root).locate_path(IkRigAssetKind::KIND, r.as_str())) {
                    body = body.child(Button::new("rt-open-src").icon(IconName::ExternalLink).label("Open source IK Rig").small().ghost().on_click(move |_, window, cx| window.dispatch_action(Box::new(OpenAsset { path: path.clone() }), cx)));
                }
                if let Some(path) = a.target_rig.as_ref().and_then(|r| ProjectRefIndex::new(&root).locate_path(IkRigAssetKind::KIND, r.as_str())) {
                    body = body.child(Button::new("rt-open-tgt").icon(IconName::ExternalLink).label("Open target IK Rig").small().ghost().on_click(move |_, window, cx| window.dispatch_action(Box::new(OpenAsset { path: path.clone() }), cx)));
                }
                body = body.child(self.sample_field(window, cx));
                body = body.child(section(
                    "Root",
                    vec![
                        self.widgets.text_field("root.translation", "Translation alpha", &fmt_f32(a.root.translation_alpha), window, cx),
                        self.widgets.text_field("root.rotation", "Rotation alpha", &fmt_f32(a.root.rotation_alpha), window, cx),
                        self.widgets.text_field("root.scale", "Scale (auto or factor)", &match a.root.scale {
                            RootScale::Auto => "auto".to_string(),
                            RootScale::Fixed { factor } => fmt_f32(factor),
                        }, window, cx),
                    ],
                    cx,
                ));
            }
        }
        body.into_any_element()
    }
}

impl RetargetProfileEditor {
    fn sample_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let status = {
            let index = SnapshotIndex::new(&self.chrome.root);
            refs::ref_status(&index, AnimationSequenceAssetKind::KIND, self.bench.sample_id.as_deref(), self.model.asset().source_skeleton.as_ref().map(|s| s.as_str()))
        };
        let root = self.chrome.root.clone();
        let skeleton = self.model.asset().source_skeleton.as_ref().map(|s| s.as_str().to_string());
        let current = self.bench.sample_id.clone();
        self.widgets.ref_field("preview_sample", "Preview sample", AnimationSequenceAssetKind::KIND, skeleton, current, &status, &root, window, cx)
    }

    fn overlay_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        overlay_pane(&self.bench, cx)
    }

    fn table(&mut self, cx: &mut Context<Self>) -> AnyElement {
        chain_table(self.model.rows(), match &self.model.selection {
            AnimSelection::RetargetChain { name } => Some(name.clone()),
            _ => None,
        }, cx, |this: &mut Self, name| {
            this.model.select_chain(&name);
        })
    }
}

fn overlay_pane(bench: &Bench, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let source = skeleton_canvas::element(bench.source_proj.clone(), bench.source_bounds.clone(), theme.muted_foreground, theme.background.opacity(0.0), Vec::new(), Vec::new(), None, theme.primary);
    let target = skeleton_canvas::element(bench.target_proj.clone(), bench.target_bounds.clone(), theme.info, theme.background.opacity(0.0), Vec::new(), Vec::new(), None, theme.primary);
    div()
        .relative()
        .size_full()
        .bg(theme.background)
        .child(div().absolute().size_full().child(source))
        .child(div().absolute().size_full().child(target))
        .child(div().absolute().top(px(6.0)).left(px(10.0)).text_xs().text_color(theme.muted_foreground).child("Source (grey) over target (blue)"))
        .into_any_element()
}

/// The chain status table under the viewport.
fn chain_table<V: 'static>(rows: Vec<ChainRow>, selected: Option<String>, cx: &mut Context<V>, select: fn(&mut V, String)) -> AnyElement {
    let theme = cx.theme().clone();
    let mut t = v_flex().w_full();
    t = t.child(
        h_flex().w_full().px_3().h(px(22.0)).items_center().gap_2().bg(theme.secondary).text_xs().text_color(theme.muted_foreground).child(div().w(px(130.0)).child("Target chain")).child(div().w(px(130.0)).child("Source chain")).child(div().w(px(110.0)).child("Status")).child("Suggestions"),
    );
    for row in rows {
        let name = row.target_chain.clone();
        t = t.child(
            h_flex()
                .id(SharedString::from(format!("tbl-{}", row.target_chain)))
                .w_full()
                .px_3()
                .py_0p5()
                .gap_2()
                .items_center()
                .cursor_pointer()
                .text_xs()
                .when(selected.as_deref() == Some(row.target_chain.as_str()), |d| d.bg(theme.accent))
                .hover(|d| d.bg(theme.secondary))
                .child(div().w(px(130.0)).text_color(theme.foreground).child(row.target_chain.clone()))
                .child(div().w(px(130.0)).text_color(theme.muted_foreground).child(row.source_chain.clone().unwrap_or_else(|| "\u{2014}".into())))
                .child(div().w(px(110.0)).child(status_chip(&row.status, cx)))
                .child(div().text_color(theme.muted_foreground).child(row.suggestions.join(", ")))
                .on_click(cx.listener(move |this, _, _, cx| {
                    select(this, name.clone());
                    cx.notify();
                })),
        );
    }
    t.into_any_element()
}

/// Advance a bench's playhead on a timer while `playing`.
fn play_loop<V: 'static>(cx: &mut Context<V>, bench: fn(&mut V) -> (&mut Bench, bool)) {
    let weak = cx.weak_entity();
    cx.spawn(async move |_, cx| {
        loop {
            smol::Timer::after(std::time::Duration::from_millis(33)).await;
            let go = weak.update(cx, |this, cx| {
                let (b, _) = bench(this);
                if !b.playing {
                    return false;
                }
                let d = b.duration().max(1.0 / 30.0);
                b.time = (b.time + 0.033) % d;
                b.pose();
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

trait SelectedIf {
    fn selected_if(self, on: bool) -> Self;
}
impl SelectedIf for Button {
    fn selected_if(self, on: bool) -> Self {
        ui::Selectable::selected(self, on)
    }
}

// ================================= IK Retargeter ================================

pub struct IkRetargetEditor {
    model: IkRetargetModel,
    chrome: Chrome,
    widgets: Widgets,
    bench: Bench,
    statuses: HashMap<String, RefStatus>,
}

impl IkRetargetEditor {
    pub fn open(path: PathBuf, _window: &mut Window, cx: &mut App) -> Result<Entity<Self>, PluginError> {
        let text = IkRetargetAsset::new("New IK Retargeter", None, None).to_text().unwrap_or_default();
        let model = IkRetargetModel::open(Arc::new(FsStore), &path, &text).map_err(|message| PluginError::Other { message })?;
        let chrome = Chrome::new(&path, cx);
        asset_index::record_asset(&chrome.root, &path);
        let mut this = Self { model, chrome, widgets: Widgets::default(), bench: Bench::new(), statuses: HashMap::new() };
        this.reload_inputs();
        Ok(cx.new(|_| this))
    }

    fn reload_inputs(&mut self) {
        let root = self.chrome.root.clone();
        let a = self.model.asset();
        let (sr, tr) = (a.source_rig.as_ref().map(|r| r.as_str().to_string()), a.target_rig.as_ref().map(|r| r.as_str().to_string()));
        self.model.source_rig = sr.as_deref().and_then(|i| load_rig(&root, i));
        self.model.target_rig = tr.as_deref().and_then(|i| load_rig(&root, i));
        let skeleton_of = |rig: &Option<IkRigAsset>| rig.as_ref().and_then(|r| r.skeleton.as_ref()).and_then(|s| load_skeleton(&root, s.as_str()));
        self.model.source_skeleton = skeleton_of(&self.model.source_rig);
        self.model.target_skeleton = skeleton_of(&self.model.target_rig);
        let index = SnapshotIndex::new(&root);
        self.statuses.insert("source_rig".into(), refs::ref_status(&index, IkRigAssetKind::KIND, sr.as_deref(), None));
        self.statuses.insert("target_rig".into(), refs::ref_status(&index, IkRigAssetKind::KIND, tr.as_deref(), None));
        self.rebuild_preview();
    }

    fn rebuild_preview(&mut self) {
        let (s, t) = (self.model.source_skeleton.clone(), self.model.target_skeleton.clone());
        let preview = match (&s, &t, &self.model.source_rig, &self.model.target_rig) {
            (Some(s), Some(t), Some(sr), Some(tr)) => RetargetPreview::for_ik_retargeter(self.model.asset(), s, t, sr, tr, self.bench.sample.as_ref()),
            _ => Err("pick both IK rigs".to_string()),
        };
        self.bench.refresh(preview, s.as_ref(), t.as_ref());
    }

    fn chain_inspector(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(c) = self.model.asset().chains.iter().find(|c| c.target_chain == name).cloned() else { return empty_inspector(&self.model.selection, "The chain no longer exists.", cx) };
        let rows = self.model.rows();
        let row = rows.iter().find(|r| r.target_chain == name).cloned();
        let mut body = v_flex().w_full().gap_2().child(div().text_sm().font_semibold().text_color(cx.theme().foreground).child(format!("Chain: {name}")));
        if let Some(row) = &row {
            body = body.child(labelled("Status", status_chip(&row.status, cx), cx));
        }
        let choices: Vec<String> = self.model.source_rig.as_ref().map(|r| r.chains.iter().map(|c| c.name.clone()).collect()).unwrap_or_default();
        let (current, t) = (c.source_chain.clone(), name.to_string());
        body = body.child(labelled(
            "Source chain",
            Button::new("irt-src").label(c.source_chain.clone().unwrap_or_else(|| "None".into())).small().outline().tooltip("Cycle through the source rig's chains").on_click(cx.listener(move |this, _, window, cx| {
                let next = match &current {
                    None => choices.first().cloned(),
                    Some(c) => choices.iter().position(|x| x == c).and_then(|p| choices.get(p + 1).cloned()),
                };
                let t = t.clone();
                edit(this, window, cx, |m| m.set_source_chain(&t, next.as_deref()));
            })),
            cx,
        ));
        if let Some(row) = &row {
            for s in row.suggestions.iter().take(4) {
                let (s2, t) = (s.clone(), name.to_string());
                body = body.child(Button::new(SharedString::from(format!("irt-sg-{s}"))).label(format!("Use {s}")).small().ghost().on_click(cx.listener(move |this, _, window, cx| {
                    let (s2, t) = (s2.clone(), t.clone());
                    edit(this, window, cx, |m| m.set_source_chain(&t, Some(&s2)));
                })));
            }
        }
        let fk = c.fk.clone();
        let t1 = name.to_string();
        let fk1 = fk.clone();
        body = body.child(section(
            "FK",
            vec![
                labelled(
                    "Enabled",
                    Button::new("irt-fk-en").label(if fk.enabled { "On" } else { "Off" }).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                        let t = t1.clone();
                        let mut fk = fk1.clone();
                        fk.enabled = !fk.enabled;
                        edit(this, window, cx, |m| m.set_fk(&t, fk));
                    })),
                    cx,
                ),
                {
                    let t = name.to_string();
                    let fk2 = fk.clone();
                    labelled(
                        "Rotation",
                        Button::new("irt-fk-rot").label(format!("{:?}", fk.rotation)).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                            let t = t.clone();
                            let mut fk = fk2.clone();
                            fk.rotation = match fk.rotation {
                                RotationMode::Interpolated => RotationMode::OneToOne,
                                RotationMode::OneToOne => RotationMode::OneToOneReversed,
                                RotationMode::OneToOneReversed => RotationMode::None,
                                RotationMode::None => RotationMode::Interpolated,
                            };
                            edit(this, window, cx, |m| m.set_fk(&t, fk));
                        })),
                        cx,
                    )
                },
                self.widgets.text_field("fk.rot_alpha", "Rotation alpha", &fmt_f32(fk.rotation_alpha), window, cx),
                self.widgets.text_field("fk.tr_alpha", "Translation alpha", &fmt_f32(fk.translation_alpha), window, cx),
            ],
            cx,
        ));
        let ik = c.ik.clone();
        let (t2, ik1) = (name.to_string(), ik.clone());
        body = body.child(section(
            "IK",
            vec![
                labelled(
                    "Enabled",
                    Button::new("irt-ik-en").label(if ik.enabled { "On" } else { "Off" }).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                        let t = t2.clone();
                        let mut ik = ik1.clone();
                        ik.enabled = !ik.enabled;
                        edit(this, window, cx, |m| m.set_ik(&t, ik));
                    })),
                    cx,
                ),
                self.widgets.text_field("ik.blend", "Blend to source", &fmt_f32(ik.blend_to_source), window, cx),
                self.widgets.text_field("ik.extension", "Extension", &fmt_f32(ik.extension), window, cx),
                self.widgets.text_field("ik.vscale", "Vertical scale", &fmt_f32(ik.scale_vertical), window, cx),
                self.widgets.text_field("ik.off0", "Offset X", &fmt_f32(ik.static_offset[0]), window, cx),
                self.widgets.text_field("ik.off1", "Offset Y", &fmt_f32(ik.static_offset[1]), window, cx),
                self.widgets.text_field("ik.off2", "Offset Z", &fmt_f32(ik.static_offset[2]), window, cx),
            ],
            cx,
        ));
        body.into_any_element()
    }
}

impl EditorHost for IkRetargetEditor {
    fn widgets(&mut self) -> &mut Widgets {
        &mut self.widgets
    }

    fn commit_field(&mut self, key: &str, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let v = parse_f32(&text);
        let sel = self.model.selection.clone();
        let pose_bone = self.bench.pose_bone.clone();
        edit(self, window, cx, |m| {
            let n = |what: &str| v.ok_or_else(|| format!("{what} must be a number"));
            match (key, &sel) {
                ("name", _) => {
                    m.doc.apply("Rename", |a| a.name = text.trim().to_string());
                    Ok(())
                }
                ("root.rot", _) => m.set_root(|r| r.rotation_alpha = v.unwrap_or(r.rotation_alpha)).and(n("alpha").map(|_| ())),
                ("root.tr", _) => m.set_root(|r| r.translation_alpha = v.unwrap_or(r.translation_alpha)).and(n("alpha").map(|_| ())),
                ("root.sh", _) => m.set_root(|r| r.scale_horizontal = v.unwrap_or(r.scale_horizontal)).and(n("scale").map(|_| ())),
                ("root.sv", _) => m.set_root(|r| r.scale_vertical = v.unwrap_or(r.scale_vertical)).and(n("scale").map(|_| ())),
                (k, AnimSelection::RetargetChain { name }) if k.starts_with("fk.") || k.starts_with("ik.") => {
                    let c = m.asset().chains.iter().find(|c| &c.target_chain == name).cloned().ok_or("no such chain")?;
                    let value = n("value")?;
                    if k.starts_with("fk.") {
                        let mut fk: FkSettings = c.fk.clone();
                        if k == "fk.rot_alpha" { fk.rotation_alpha = value } else { fk.translation_alpha = value }
                        m.set_fk(name, fk)
                    } else {
                        let mut ik: IkSettings = c.ik.clone();
                        match k {
                            "ik.blend" => ik.blend_to_source = value,
                            "ik.extension" => ik.extension = value,
                            "ik.vscale" => ik.scale_vertical = value,
                            other => ik.static_offset[other.trim_start_matches("ik.off").parse::<usize>().unwrap_or(0).min(2)] = value,
                        }
                        m.set_ik(name, ik)
                    }
                }
                (k, _) if k.starts_with("pose.") => {
                    let (side, bone) = pose_bone.clone().ok_or("click a bone first")?;
                    let axis = k.trim_start_matches("pose.").parse::<usize>().unwrap_or(0).min(2);
                    let mut euler = m.pose_bone_euler(side, &bone);
                    euler[axis] = n("angle")?;
                    m.set_pose_bone(side, &bone, euler).map(|_| ())
                }
                _ => Ok(()),
            }
        });
        self.rebuild_preview();
    }

    fn pick_ref(&mut self, key: &str, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if key == "preview_sample" {
            let root = self.chrome.root.clone();
            self.bench.load_sample(&root, id);
            self.rebuild_preview();
            cx.notify();
            return;
        }
        edit(self, window, cx, |m| {
            let rig = id.as_deref().and_then(AssetRef::<IkRigAssetKind>::parse);
            let (s, t) = (m.asset().source_rig.clone(), m.asset().target_rig.clone());
            if key == "source_rig" { m.set_rigs(rig, t) } else { m.set_rigs(s, rig) }
        });
        self.reload_inputs();
    }
}

impl EditorBody for IkRetargetEditor {
    type Model = IkRetargetModel;
    fn model(&self) -> &IkRetargetModel {
        &self.model
    }
    fn model_mut(&mut self) -> &mut IkRetargetModel {
        &mut self.model
    }
    fn chrome(&mut self) -> &mut Chrome {
        &mut self.chrome
    }
    fn chrome_ref(&self) -> &Chrome {
        &self.chrome
    }
    fn icon(&self) -> IconName {
        IconName::Link
    }
    fn kind_title(&self) -> &'static str {
        "IK Retargeter"
    }
    fn left_title(&self) -> &'static str {
        "Chains"
    }

    fn after_change(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.rebuild_preview();
    }

    fn select_location(&mut self, location: &str, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(c) = self.model.asset().chains.iter().find(|c| c.id == location) {
            let name = c.target_chain.clone();
            self.model.select_chain(&name);
        }
        cx.notify();
    }

    fn toolbar_extra(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let playing = self.bench.playing;
        let overlay = self.bench.overlay;
        let a = self.model.asset();
        let (src_path, tgt_path) = {
            let index = ProjectRefIndex::new(&self.chrome.root);
            (a.source_rig.as_ref().and_then(|r| index.locate_path(IkRigAssetKind::KIND, r.as_str())), a.target_rig.as_ref().and_then(|r| index.locate_path(IkRigAssetKind::KIND, r.as_str())))
        };
        let open = |id: &'static str, label: &'static str, path: Option<PathBuf>| {
            Button::new(id).icon(IconName::ExternalLink).label(label).small().ghost().disabled_if(path.is_none()).on_click(move |_, window, cx| {
                if let Some(p) = &path {
                    window.dispatch_action(Box::new(OpenAsset { path: p.clone() }), cx);
                }
            })
        };
        vec![
            Button::new("irt-sync").icon(IconName::Plus).label("Add chains").small().ghost().tooltip("Add a setting for every target chain that has none").on_click(cx.listener(|this, _, window, cx| {
                edit(this, window, cx, |m| {
                    m.sync_chains();
                    Ok(())
                })
            })).into_any_element(),
            open("irt-open-src", "Source rig", src_path).into_any_element(),
            open("irt-open-tgt", "Target rig", tgt_path).into_any_element(),
            Button::new("irt-play").icon(if playing { IconName::Pause } else { IconName::Play }).small().ghost().on_click(cx.listener(|this, _, _, cx| {
                this.bench.playing = !this.bench.playing;
                if this.bench.playing {
                    play_loop(cx, |this: &mut Self| (&mut this.bench, true));
                }
                cx.notify();
            })).into_any_element(),
            Button::new("irt-overlay").label("Overlay").small().ghost().selected_if(overlay).on_click(cx.listener(|this, _, _, cx| {
                this.bench.overlay = !this.bench.overlay;
                cx.notify();
            })).into_any_element(),
        ]
    }

    fn left(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let selected = match &self.model.selection {
            AnimSelection::RetargetChain { name } => Some(name.clone()),
            _ => None,
        };
        let mut list = v_flex().w_full();
        let rows = self.model.rows();
        if rows.is_empty() {
            list = list.child(div().p_3().text_xs().text_color(theme.muted_foreground).child("Pick both IK rigs, then Add chains."));
        }
        for row in rows {
            let name = row.target_chain.clone();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("irt-row-{}", row.target_chain)))
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .when(selected.as_deref() == Some(row.target_chain.as_str()), |d| d.bg(theme.accent))
                    .hover(|d| d.bg(theme.secondary))
                    .child(v_flex().flex_1().min_w_0().child(div().text_xs().text_color(theme.foreground).child(row.target_chain.clone())).child(div().text_xs().text_color(theme.muted_foreground).child(row.source_chain.clone().map_or("\u{2014}".to_string(), |s| format!("\u{2190} {s}")))))
                    .child(status_chip(&row.status, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.select_chain(&name);
                        cx.notify();
                    })),
            );
        }
        list.into_any_element()
    }

    fn center(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let entity = cx.entity();
        let pick = move |side: PoseSide| -> Rc<dyn Fn(Option<String>, &mut Window, &mut App)> {
            let entity = entity.clone();
            Rc::new(move |bone, _window, cx| {
                entity.update(cx, |this, cx| {
                    this.bench.pose_bone = bone.map(|b| (side, b));
                    this.model.selection = AnimSelection::None;
                    cx.notify();
                });
            })
        };
        let selected_of = |side: PoseSide, bench: &Bench| bench.pose_bone.as_ref().filter(|(s, _)| *s == side).map(|(_, b)| b.clone());
        let source = skeleton_pane("SOURCE", &self.bench.source_proj, &self.bench.source_bounds, Vec::new(), selected_of(PoseSide::Source, &self.bench), pick(PoseSide::Source), cx);
        let target = skeleton_pane("TARGET", &self.bench.target_proj, &self.bench.target_bounds, Vec::new(), selected_of(PoseSide::Target, &self.bench), pick(PoseSide::Target), cx);
        let table = chain_table(self.model.rows(), match &self.model.selection {
            AnimSelection::RetargetChain { name } => Some(name.clone()),
            _ => None,
        }, cx, |this: &mut Self, name| {
            this.model.select_chain(&name);
        });
        v_flex()
            .size_full()
            .child(h_flex().flex_1().min_h_0().w_full().child(if self.bench.overlay { overlay_pane(&self.bench, cx) } else { h_flex().size_full().child(source).child(target).into_any_element() }))
            .child(div().h(px(150.0)).flex_shrink_0().border_t_1().border_color(theme.border).overflow_y_scroll().child(table))
            .into_any_element()
    }

    fn inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let root = self.chrome.root.clone();
        let theme = cx.theme().clone();
        if let AnimSelection::RetargetChain { name } = self.model.selection.clone() {
            return self.chain_inspector(&name, window, cx);
        }
        let a = self.model.asset().clone();
        let mut body = v_flex().w_full().gap_2();
        if let Some((side, bone)) = self.bench.pose_bone.clone() {
            let euler = self.model.pose_bone_euler(side, &bone);
            let mut rows = Vec::new();
            for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
                rows.push(self.widgets.text_field(&format!("pose.{i}"), &format!("{axis} (deg)"), &fmt_f32(euler[i]), window, cx));
            }
            rows.push(Button::new("irt-pose-reset").label("Reset this side's pose").small().outline().on_click(cx.listener(move |this, _, window, cx| {
                edit(this, window, cx, |m| {
                    m.reset_pose(side);
                    Ok(())
                })
            })).into_any_element());
            body = body.child(section(&format!("Retarget pose \u{b7} {} \u{b7} {bone}", if side == PoseSide::Source { "source" } else { "target" }), rows, cx));
        }
        body = body.child(div().text_sm().font_semibold().text_color(theme.foreground).child("IK Retargeter"));
        body = body.child(self.widgets.text_field("name", "Name", &a.name, window, cx));
        let rid = |r: &Option<AssetRef<IkRigAssetKind>>| r.as_ref().map(|s| s.as_str().to_string());
        let st = |s: &HashMap<String, RefStatus>, k: &str| s.get(k).cloned().unwrap_or(RefStatus::Unset);
        body = body.child(self.widgets.ref_field("source_rig", "Source IK rig", IkRigAssetKind::KIND, None, rid(&a.source_rig), &st(&self.statuses, "source_rig"), &root, window, cx));
        body = body.child(self.widgets.ref_field("target_rig", "Target IK rig", IkRigAssetKind::KIND, None, rid(&a.target_rig), &st(&self.statuses, "target_rig"), &root, window, cx));
        let sample_status = {
            let index = SnapshotIndex::new(&root);
            refs::ref_status(&index, AnimationSequenceAssetKind::KIND, self.bench.sample_id.as_deref(), self.model.source_rig.as_ref().and_then(|r| r.skeleton.as_ref()).map(|s| s.as_str()))
        };
        let skeleton = self.model.source_rig.as_ref().and_then(|r| r.skeleton.as_ref()).map(|s| s.as_str().to_string());
        body = body.child(self.widgets.ref_field("preview_sample", "Preview sample", AnimationSequenceAssetKind::KIND, skeleton, self.bench.sample_id.clone(), &sample_status, &root, window, cx));
        body = body.child(section(
            "Root",
            vec![
                self.widgets.text_field("root.rot", "Rotation alpha", &fmt_f32(a.root.rotation_alpha), window, cx),
                self.widgets.text_field("root.tr", "Translation alpha", &fmt_f32(a.root.translation_alpha), window, cx),
                self.widgets.text_field("root.sh", "Horizontal scale", &fmt_f32(a.root.scale_horizontal), window, cx),
                self.widgets.text_field("root.sv", "Vertical scale", &fmt_f32(a.root.scale_vertical), window, cx),
            ],
            cx,
        ));
        body.into_any_element()
    }
}

trait DisabledIf {
    fn disabled_if(self, on: bool) -> Self;
}
impl DisabledIf for Button {
    fn disabled_if(self, on: bool) -> Self {
        ui::Disableable::disabled(self, on)
    }
}

impl Render for RetargetProfileEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame(self, window, cx)
    }
}

impl Render for IkRetargetEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame(self, window, cx)
    }
}

crate::editor_model!(RetargetProfileModel, <pulsar_reflection::RetargetProfileAssetKind as AssetKind>::KIND);
crate::editor_model!(IkRetargetModel, <pulsar_reflection::IkRetargeterAssetKind as AssetKind>::KIND);
crate::anim_panel!(RetargetProfileEditor, "retarget-profile-editor");
crate::anim_panel!(IkRetargetEditor, "ik-retargeter-editor");


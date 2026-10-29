//! The Pose Search Database editor: a source clip table with typed pickers,
//! feature channel authoring, the derived-index build state (status, version,
//! hash, entries) with the real builder behind a Build button, and a query
//! debugger whose rows show the cost breakdown of the best match per query.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use plugin_editor_api::{OpenAsset, PluginError};
use pulsar_reflection::{AnimationSequenceAssetKind, AssetKind, AssetRef, BlendSpaceAssetKind, SkeletonAssetKind};
use skeletal_assets::assets::AnimSeqAsset;
use skeletal_assets::blend_space::BlendSpaceAsset;
use skeletal_assets::pose_db::{derived_index_path, encode_index, ChannelKind, Normalization, PoseDbAsset, PoseSource, SearchSettings};
use skeletal_assets::runtime::clip::CompiledClip;
use skeletal_assets::runtime::posesearch::SearchClip;
use skeletal_assets::runtime::pose::RuntimeSkeleton;
use ui::button::{Button, ButtonVariants};
use ui::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt};

use crate::anim_editors::document::content_hash;
use crate::anim_editors::graph_preview::ProjectAssets;
use crate::anim_editors::pose_db_build::{BuildHandle, BuildJob, BuildOutput, ProjectSources};
use crate::anim_editors::skin_preview::FallbackPreview;
use crate::anim_editors::pose_db_model::{IndexStatus, PoseDbModel};
use crate::anim_editors::refs::{self, ProjectRefIndex, RefStatus, SnapshotIndex};
use crate::anim_editors::selection::AnimSelection;
use crate::anim_editors::store::FsStore;
use crate::core::asset_index;

use super::chrome::{edit, empty_inspector, fmt_f32, frame, labelled, parse_f32, section, short_id, value_row, Chrome, EditorBody, EditorHost, EditorModel, Widgets};
use super::skeleton_canvas::load_skeleton;

const CHANNEL_COLORS: [(f32, f32, f32); 6] = [(0.55, 0.8, 0.6), (0.75, 0.6, 0.7), (0.12, 0.85, 0.6), (0.4, 0.7, 0.55), (0.97, 0.75, 0.62), (0.9, 0.7, 0.65)];

pub struct PoseDbEditor {
    model: PoseDbModel,
    chrome: Chrome,
    widgets: Widgets,
    statuses: HashMap<String, RefStatus>,
    index_status: IndexStatus,
    /// Source file hashes keyed by source id, for the index key.
    hashes: Vec<(String, String)>,
    build_message: Option<String>,
    /// The index build running on a background thread.
    running: Option<BuildHandle>,
    /// The fallback preview: the skinned pose a Pose Search node plays without its database.
    fallback: super::skin_pane::SkinPane,
    fallback_preview: Option<(u64, Result<FallbackPreview, String>)>,
    /// Query editor state (not saved).
    query_speed: f32,
    query_time: f32,
}

impl PoseDbEditor {
    pub fn open(path: PathBuf, _window: &mut Window, cx: &mut App) -> Result<Entity<Self>, PluginError> {
        let text = PoseDbAsset::new("New Pose Database", None).to_text().unwrap_or_default();
        let model = PoseDbModel::open(Arc::new(FsStore), &path, &text).map_err(|message| PluginError::Other { message })?;
        let chrome = Chrome::new(&path, cx);
        asset_index::record_asset(&chrome.root, &path);
        let mut this = Self { model, chrome, widgets: Widgets::default(), statuses: HashMap::new(), index_status: IndexStatus::Missing, hashes: Vec::new(), build_message: None, running: None, fallback: super::skin_pane::SkinPane::default(), fallback_preview: None, query_speed: 2.0, query_time: 0.5 };
        this.reload();
        Ok(cx.new(|_| this))
    }

    fn reload(&mut self) {
        let root = self.chrome.root.clone();
        let skeleton_id = self.model.asset().skeleton.as_ref().map(|s| s.as_str().to_string());
        self.model.skeleton = skeleton_id.as_deref().and_then(|id| load_skeleton(&root, id));
        let index = SnapshotIndex::new(&root);
        self.statuses.insert("skeleton".into(), refs::ref_status(&index, SkeletonAssetKind::KIND, skeleton_id.as_deref(), None));
        let sources = self.model.asset().sources.clone();
        self.hashes.clear();
        for s in &sources {
            let (kind, id) = match &s.source {
                PoseSource::Sequence { sequence } => (AnimationSequenceAssetKind::KIND, sequence.as_str().to_string()),
                PoseSource::BlendSpace { blend_space } => (BlendSpaceAssetKind::KIND, blend_space.as_str().to_string()),
            };
            self.statuses.insert(s.id.clone(), refs::ref_status(&index, kind, Some(&id), skeleton_id.as_deref()));
            if s.enabled {
                if let Some(path) = ProjectRefIndex::new(&root).locate_path(kind, &id) {
                    if let Ok(bytes) = engine_fs::virtual_fs::read_file(&path) {
                        self.hashes.push((s.id.clone(), format!("{:016x}", content_hash(&bytes))));
                    }
                }
            }
        }
        self.refresh_index_status();
    }

    fn key(&self) -> skeletal_assets::pose_db::IndexKey {
        self.model.index_key(&self.hashes)
    }

    fn cache_path(&self) -> PathBuf {
        derived_index_path(&self.chrome.root, &self.model.asset().id, &self.key())
    }

    fn refresh_index_status(&mut self) {
        let key = self.key();
        let bytes = engine_fs::virtual_fs::read_file(&self.cache_path()).ok();
        self.index_status = self.model.index_status(bytes.as_deref(), &key);
    }

    fn source_label(&self, id: &str) -> String {
        let Some(s) = self.model.asset().sources.iter().find(|s| s.id == id) else { return "?".into() };
        match &s.source {
            PoseSource::Sequence { sequence } => format!("Sequence {}", short_id(sequence.as_str())),
            PoseSource::BlendSpace { blend_space } => format!("Blend Space {}", short_id(blend_space.as_str())),
        }
    }

    /// Start building the index on a background thread: the sources are read
    /// and compiled and the real builder runs there, reporting progress the
    /// panel shows and polling a cancel flag between poses.
    fn start_build(&mut self, cx: &mut Context<Self>) {
        if self.running.is_some() {
            return;
        }
        let Some(skeleton) = self.model.skeleton.clone() else {
            self.build_message = Some("Pick the database's skeleton first.".into());
            return;
        };
        let key = self.key();
        let job = BuildJob { asset: self.model.asset().clone(), skeleton, sources: Arc::new(ProjectSources { root: self.chrome.root.clone() }) };
        let handle = BuildHandle::default();
        let mut reporter = handle.reporter();
        self.running = Some(handle);
        self.build_message = None;
        let weak = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            let result = cx.background_executor().spawn(async move { job.run(&mut reporter) }).await;
            let _ = weak.update(cx, |this, cx| this.finish_build(result, &key, cx));
        })
        .detach();
        // repaint the progress while the build runs
        let weak = cx.weak_entity();
        cx.spawn(async move |_, cx| loop {
            smol::Timer::after(std::time::Duration::from_millis(100)).await;
            if !matches!(weak.update(cx, |this, cx| {
                cx.notify();
                this.running.is_some()
            }), Ok(true))
            {
                break;
            }
        })
        .detach();
        cx.notify();
    }

    fn cancel_build(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = &self.running {
            handle.cancel();
            self.build_message = Some("Cancelling\u{2026}".into());
            cx.notify();
        }
    }

    fn finish_build(&mut self, result: Result<BuildOutput, Vec<skeletal_assets::diagnostics::Diagnostic>>, key: &skeletal_assets::pose_db::IndexKey, cx: &mut Context<Self>) {
        self.running = None;
        let missing = result.as_ref().map(|o| o.missing.clone()).unwrap_or_default();
        match self.model.finish_build(result, key) {
            Ok(info) => {
                let mut cache_error = None;
                if let Some(payload) = self.model.index_payload() {
                    let path = self.cache_path();
                    if let Some(parent) = path.parent() {
                        let _ = engine_fs::virtual_fs::create_dir_all(parent);
                    }
                    if let Err(e) = engine_fs::virtual_fs::write_file_atomically(&path, &encode_index(key, &payload)) {
                        cache_error = Some(format!("Built, but the cache could not be written: {e}"));
                    }
                }
                self.build_message = Some(match (cache_error, missing.is_empty()) {
                    (Some(e), _) => e,
                    (None, true) => format!("Built {} poses from {} clips.", info.entries, info.clips),
                    (None, false) => format!("Built without {}.", missing.join(", ")),
                });
            }
            Err(d) if crate::anim_editors::pose_db_build::is_cancelled(&d) => self.build_message = Some("Build cancelled; the previous index is unchanged.".into()),
            Err(d) => self.build_message = Some(format!("Build failed: {}", d.first().map_or(String::new(), |d| d.message.clone()))),
        }
        self.refresh_index_status();
        cx.notify();
    }

    fn query(&mut self) {
        let Some(skeleton) = self.model.skeleton.clone() else {
            self.build_message = Some("Pick the database's skeleton first.".into());
            return;
        };
        // the reference pose comes from the first enabled sequence source
        let root = self.chrome.root.clone();
        let first = self.model.asset().sources.iter().find_map(|s| match &s.source {
            PoseSource::Sequence { sequence } if s.enabled => Some(sequence.as_str().to_string()),
            _ => None,
        });
        let Some(clip_asset) = first.and_then(|id| {
            let path = ProjectRefIndex::new(&root).locate_path(AnimationSequenceAssetKind::KIND, &id)?;
            AnimSeqAsset::parse(&String::from_utf8(engine_fs::virtual_fs::read_file(&path).ok()?).ok()?).ok()
        }) else {
            self.build_message = Some("Add a Sequence source to take the query pose from.".into());
            return;
        };
        let Ok(runtime) = RuntimeSkeleton::new("posedb", &skeleton) else { return };
        let clip = CompiledClip::compile(&clip_asset, &runtime);
        let time = self.query_time.min(clip.duration);
        if let Err(e) = self.model.run_query(&skeleton, &clip, time, [0.0, 0.0, self.query_speed], None) {
            self.build_message = Some(e);
        }
    }

    fn query_timeline(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let queries = self.model.queries.clone();
        let selected = match self.model.selection {
            AnimSelection::PoseResult { index } => Some(index),
            _ => None,
        };
        let max_cost = queries.iter().map(|q| q.result.cost).fold(0.0f32, f32::max).max(1e-4);
        let mut col = v_flex().w_full().gap_1().p_2();
        if queries.is_empty() {
            col = col.child(div().text_xs().text_color(theme.muted_foreground).child("No queries yet. Build the index, then Run query: it matches a pose of the first Sequence source against the database."));
        }
        for (i, q) in queries.iter().enumerate() {
            let total: f32 = q.result.breakdown.iter().map(|c| c.cost).sum::<f32>().max(1e-6);
            let bar_w = 360.0 * (q.result.cost / max_cost).clamp(0.02, 1.0);
            let mut bar = h_flex().w(px(bar_w)).h(px(12.0)).rounded(px(2.0)).overflow_hidden();
            for (ci, c) in q.result.breakdown.iter().enumerate() {
                let (h, s, l) = CHANNEL_COLORS[ci % CHANNEL_COLORS.len()];
                bar = bar.child(div().h_full().w(relative(c.cost / total)).bg(hsla(h, s, l, 0.9)));
            }
            col = col.child(
                h_flex()
                    .id(SharedString::from(format!("q-{i}")))
                    .w_full()
                    .gap_2()
                    .px_1()
                    .py_0p5()
                    .items_center()
                    .rounded(px(3.0))
                    .cursor_pointer()
                    .when(selected == Some(i), |d| d.bg(theme.accent))
                    .hover(|d| d.bg(theme.secondary))
                    .text_xs()
                    .child(div().w(px(24.0)).text_color(theme.muted_foreground).child(format!("#{}", i + 1)))
                    .child(div().w(px(130.0)).text_color(theme.foreground).child(format!("{} @ {}s", short_id(&q.source), fmt_f32(q.result.clip_time))))
                    .child(bar)
                    .child(div().text_color(theme.muted_foreground).child(format!("cost {:.3}", q.result.cost)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.select_result(i);
                        cx.notify();
                    })),
            );
        }
        col.into_any_element()
    }

    fn build_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let (status_text, color) = match &self.index_status {
            IndexStatus::Current => ("Up to date".to_string(), theme.success),
            IndexStatus::Missing => ("Not built".to_string(), theme.warning),
            IndexStatus::Stale { .. } => ("Stale: settings or a source changed".to_string(), theme.warning),
            IndexStatus::Corrupt => ("Cache unreadable; rebuild".to_string(), theme.danger),
        };
        let key = self.key();
        let info = self.model.last_build.clone();
        let rows: Vec<AnyElement> = vec![
            labelled("Index", div().text_xs().text_color(color).child(status_text), cx),
            value_row("Builder version", key.builder_version.to_string(), cx),
            value_row("Input hash", short_id(&key.hash).to_string() + "\u{2026}", cx),
            value_row("Last build", info.map_or("this session: none".to_string(), |i| format!("{} poses \u{b7} {} dims \u{b7} {} clips", i.entries, i.dims, i.clips)), cx),
        ];
        v_flex()
            .w_full()
            .p_2()
            .gap_1()
            .children(rows)
            .child(match &self.running {
                Some(handle) => {
                    let percent = handle.fraction().map_or("starting".to_string(), |f| format!("{:.0}%", f * 100.0));
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().text_xs().text_color(theme.foreground).child(format!("Building index\u{2026} {percent}")))
                        .child(Button::new("pdb-cancel").icon(IconName::Close).label("Cancel").small().outline().on_click(cx.listener(|this, _, _, cx| this.cancel_build(cx))))
                        .into_any_element()
                }
                None => Button::new("pdb-build").icon(IconName::Refresh).label("Build index").small().primary().on_click(cx.listener(|this, _, window, cx| {
                    let _ = window;
                    this.start_build(cx);
                })).into_any_element(),
            })
            .children(self.build_message.clone().map(|m| div().text_xs().text_color(theme.muted_foreground).child(m)))
            .into_any_element()
    }
}

impl EditorHost for PoseDbEditor {
    fn widgets(&mut self) -> &mut Widgets {
        &mut self.widgets
    }

    fn commit_field(&mut self, key: &str, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let v = parse_f32(&text);
        let sel = self.model.selection.clone();
        let (speed, time) = (&mut self.query_speed, &mut self.query_time);
        if key == "q.speed" {
            if let Some(v) = v {
                *speed = v;
            }
            cx.notify();
            return;
        }
        if key == "q.time" {
            if let Some(v) = v {
                *time = v.max(0.0);
            }
            cx.notify();
            return;
        }
        edit(self, window, cx, |m| {
            let n = |what: &str| v.ok_or_else(|| format!("{what} must be a number"));
            match (key, &sel) {
                ("name", _) => {
                    m.rename(&text);
                    Ok(())
                }
                ("sampling.interval", _) | ("sampling.margin", _) => {
                    let (i, e) = (m.asset().sampling.interval, m.asset().sampling.end_margin);
                    if key == "sampling.interval" { m.set_sampling(n("interval")?, e) } else { m.set_sampling(i, n("margin")?) }
                }
                (k, _) if k.starts_with("search.") => {
                    let mut s: SearchSettings = m.asset().search.clone();
                    match k {
                        "search.candidates" => s.candidate_count = text.trim().parse().map_err(|_| "candidates must be a whole number".to_string())?,
                        "search.bias" => s.continuing_pose_bias = n("bias")?,
                        "search.blend" => s.blend_time = n("blend time")?,
                        _ => s.query_history = text.trim().parse().map_err(|_| "history must be a whole number".to_string())?,
                    }
                    m.set_search(s)
                }
                ("add.name", _) | ("add.bone", _) | ("add.offsets", _) | ("add.curve", _) => Ok(()),
                ("src.bias", AnimSelection::PoseSource { id }) => m.set_source_bias(id, n("bias")?),
                ("src.ex_start", AnimSelection::PoseSource { .. }) | ("src.ex_end", AnimSelection::PoseSource { .. }) => Ok(()),
                ("ch.weight", AnimSelection::PoseChannel { index }) => m.set_channel_weight(*index, n("weight")?),
                ("ch.offsets", AnimSelection::PoseChannel { index }) => {
                    let ChannelKind::Trajectory { position, facing, velocity, .. } = m.asset().channels.get(*index).map(|c| c.kind.clone()).ok_or("no such channel")? else { return Err("not a trajectory".into()) };
                    let offsets: Result<Vec<f32>, _> = text.split(',').filter(|t| !t.trim().is_empty()).map(|t| t.trim().parse::<f32>()).collect();
                    m.set_trajectory(*index, offsets.map_err(|_| "enter seconds separated by commas".to_string())?, position, facing, velocity)
                }
                _ => Ok(()),
            }
        });
        self.reload();
    }

    fn pick_ref(&mut self, key: &str, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let seq = id.as_deref().and_then(AssetRef::<AnimationSequenceAssetKind>::parse);
        let space = id.as_deref().and_then(AssetRef::<BlendSpaceAssetKind>::parse);
        let skeleton = id.as_deref().and_then(AssetRef::<SkeletonAssetKind>::parse);
        let selected = match &self.model.selection {
            AnimSelection::PoseSource { id } => Some(id.clone()),
            _ => None,
        };
        edit(self, window, cx, |m| match key {
            "skeleton" => {
                m.doc.apply("Set skeleton", |a| a.skeleton = skeleton);
                Ok(())
            }
            "add.sequence" => seq.ok_or("pick a sequence".to_string()).and_then(|s| m.add_sequence_source(s)).map(|_| ()),
            "add.blendspace" => space.ok_or("pick a blend space".to_string()).and_then(|s| m.add_blend_space_source(s)).map(|_| ()),
            "src.asset" => {
                let source = selected.ok_or("select a source")?;
                match (seq, space) {
                    (Some(s), _) => m.replace_source_asset(&source, PoseSource::Sequence { sequence: s }),
                    (_, Some(b)) => m.replace_source_asset(&source, PoseSource::BlendSpace { blend_space: b }),
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        });
        self.reload();
    }
}

impl EditorBody for PoseDbEditor {
    type Model = PoseDbModel;
    fn model(&self) -> &PoseDbModel {
        &self.model
    }
    fn model_mut(&mut self) -> &mut PoseDbModel {
        &mut self.model
    }
    fn chrome(&mut self) -> &mut Chrome {
        &mut self.chrome
    }
    fn chrome_ref(&self) -> &Chrome {
        &self.chrome
    }
    fn icon(&self) -> IconName {
        IconName::Search
    }
    fn kind_title(&self) -> &'static str {
        "Pose Search Database"
    }
    fn left_title(&self) -> &'static str {
        "Sources & Features"
    }

    fn after_change(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.reload();
    }

    fn select_location(&mut self, location: &str, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.model.asset().channels.iter().position(|c| c.id == location) {
            self.model.select_channel(i);
        } else {
            self.model.select_source(location);
        }
        cx.notify();
    }

    fn toolbar_extra(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            Button::new("pdb-tb-build").icon(IconName::Refresh).label("Build").small().ghost().on_click(cx.listener(|this, _, window, cx| {
                let _ = window;
                this.start_build(cx);
            })).into_any_element(),
            Button::new("pdb-tb-query").icon(IconName::Search).label("Run query").small().ghost().tooltip("Search the built index with the query settings in the inspector").on_click(cx.listener(|this, _, _, cx| {
                this.query();
                cx.notify();
            })).into_any_element(),
        ]
    }

    fn left(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let a = self.model.asset().clone();
        let selection = self.model.selection.clone();
        let mut list = v_flex().w_full();
        list = list.child(div().px_2().py_1().text_xs().font_semibold().text_color(theme.muted_foreground).child("SOURCES"));
        for s in &a.sources {
            let id = s.id.clone();
            let sel = matches!(&selection, AnimSelection::PoseSource { id } if *id == s.id);
            let problem = self.statuses.get(&s.id).is_some_and(RefStatus::is_problem);
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("ps-{}", s.id)))
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .when(sel, |d| d.bg(theme.accent))
                    .hover(|d| d.bg(theme.secondary))
                    .child(Icon::new(if problem { IconName::TriangleAlert } else { IconName::Keyframes }).size(px(13.0)).text_color(if problem { theme.danger } else { theme.muted_foreground }))
                    .child(div().flex_1().text_xs().text_color(if s.enabled { theme.foreground } else { theme.muted_foreground }).child(self.source_label(&s.id)))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(if s.mirrored { "mirrored" } else { "" }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.select_source(&id);
                        cx.notify();
                    })),
            );
        }
        list = list.child(div().px_2().pt_2().pb_1().text_xs().font_semibold().text_color(theme.muted_foreground).child("FEATURE CHANNELS"));
        for (i, c) in a.channels.iter().enumerate() {
            let sel = matches!(&selection, AnimSelection::PoseChannel { index } if *index == i);
            let (h, s, l) = CHANNEL_COLORS[i % CHANNEL_COLORS.len()];
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("pc-{}", c.id)))
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .when(sel, |d| d.bg(theme.accent))
                    .hover(|d| d.bg(theme.secondary))
                    .child(div().size(px(8.0)).rounded_full().bg(hsla(h, s, l, 0.9)))
                    .child(div().flex_1().text_xs().text_color(theme.foreground).child(c.name.clone()))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(format!("w {}", fmt_f32(c.weight))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.select_channel(i);
                        cx.notify();
                    })),
            );
        }
        list.into_any_element()
    }

    fn center(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let timeline = self.query_timeline(cx);
        let build = self.build_panel(cx);
        let a = self.model.asset().clone();
        let dims = self.model.feature_dimensions();
        let mut table = v_flex().w_full();
        table = table.child(
            h_flex().w_full().px_3().h(px(22.0)).items_center().gap_2().bg(theme.secondary).text_xs().text_color(theme.muted_foreground).child(div().w(px(170.0)).child("Source")).child(div().w(px(90.0)).child("Kind")).child(div().w(px(80.0)).child("Options")).child("Status"),
        );
        for s in &a.sources {
            let id = s.id.clone();
            let status = self.statuses.get(&s.id).cloned().unwrap_or(RefStatus::Unset);
            let (text, color) = match &status {
                RefStatus::Ok { name } => (name.clone(), theme.success),
                RefStatus::Incompatible { name, .. } => (format!("{name}: other skeleton"), theme.warning),
                RefStatus::Missing => ("missing".to_string(), theme.danger),
                RefStatus::Ambiguous { .. } => ("ambiguous".to_string(), theme.danger),
                RefStatus::Unset => ("unset".to_string(), theme.danger),
            };
            table = table.child(
                h_flex()
                    .id(SharedString::from(format!("src-row-{}", s.id)))
                    .w_full()
                    .px_3()
                    .py_0p5()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .text_xs()
                    .hover(|d| d.bg(theme.secondary))
                    .child(div().w(px(170.0)).text_color(theme.foreground).child(self.source_label(&s.id)))
                    .child(div().w(px(90.0)).text_color(theme.muted_foreground).child(match s.source { PoseSource::Sequence { .. } => "Sequence", PoseSource::BlendSpace { .. } => "Blend Space" }))
                    .child(div().w(px(80.0)).text_color(theme.muted_foreground).child(format!("{}{}{}", if s.enabled { "on" } else { "off" }, if s.mirrored { " \u{b7} mirror" } else { "" }, if s.looping { " \u{b7} loop" } else { "" })))
                    .child(div().text_color(color).child(text))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.select_source(&id);
                        cx.notify();
                    })),
            );
        }
        if a.sources.is_empty() {
            table = table.child(div().p_3().text_xs().text_color(theme.muted_foreground).child("No source clips. Add a Sequence or Blend Space in the inspector."));
        }
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .w_full()
                    .h(px(150.0))
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().px_3().h(px(24.0)).flex().items_center().flex_shrink_0().bg(theme.secondary).text_xs().font_semibold().text_color(theme.muted_foreground).child(format!("QUERY TIMELINE \u{b7} {} channels \u{b7} {dims} dimensions", a.channels.len())))
                    .child(div().flex_1().min_h_0().overflow_y_scroll().child(timeline)),
            )
            .child(div().flex_1().min_h_0().overflow_y_scroll().child(table))
            .child(div().flex_shrink_0().border_t_1().border_color(theme.border).child(build))
            .into_any_element()
    }

    fn inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let fallback = self.fallback_section(cx);
        let inner = self.selection_inspector(window, cx);
        v_flex().w_full().gap_2().child(inner).child(fallback).into_any_element()
    }
}

impl PoseDbEditor {
    /// What a graph's Pose Search node plays when this database cannot be used:
    /// the graph is compiled and run by the real runtime with the database
    /// hidden, so the fallback path itself is what is shown.
    fn fallback_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let root = self.chrome.root.clone();
        let skeleton = self.model.asset().skeleton.as_ref().map(|s| s.as_str().to_string());
        let mut problem = self.fallback.rig(&root, skeleton.as_deref()).err();
        let generation = self.model.generation();
        if problem.is_none() && self.fallback_preview.as_ref().is_none_or(|(g, _)| *g != generation) {
            let assets = ProjectAssets::new(&root);
            let built = match self.fallback.rig(&root, skeleton.as_deref()) {
                Ok(rig) => FallbackPreview::build(self.model.asset(), rig, &assets).map_err(|d| d.first().map(|d| d.message.clone()).unwrap_or_default()),
                Err(e) => Err(e),
            };
            if let Ok(preview) = &built {
                debug_assert!(preview.falls_back());
            }
            self.fallback_preview = Some((generation, built));
        }
        let mut note = None;
        if problem.is_none() {
            let palette = crate::style::Palette::from_theme(cx.theme());
            match &mut self.fallback_preview {
                Some((_, Ok(preview))) => {
                    let pose = preview.advance(0.0);
                    self.fallback.draw(&pose, generation, &palette);
                    note = Some(format!("Sequence {} plays through the Fallback pin; the compiler reports {}.", short_id(&preview.sequence), if preview.falls_back() { "the fallback" } else { "no fallback" }));
                }
                Some((_, Err(e))) => problem = Some(e.clone()),
                None => {}
            }
        }
        let mut rows = vec![self.fallback.element(problem.as_deref(), cx)];
        rows.extend(note.map(|n| div().text_xs().text_color(cx.theme().muted_foreground).child(n).into_any_element()));
        section("Fallback when the database is unavailable", rows, cx)
    }

    fn selection_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let root = self.chrome.root.clone();
        let skeleton_id = self.model.asset().skeleton.as_ref().map(|s| s.as_str().to_string());
        let mut body = v_flex().w_full().gap_2();
        match self.model.selection.clone() {
            AnimSelection::PoseSource { id } => {
                let Some(s) = self.model.asset().sources.iter().find(|s| s.id == id).cloned() else { return empty_inspector(&self.model.selection, "The source no longer exists.", cx) };
                let (kind, current) = match &s.source {
                    PoseSource::Sequence { sequence } => (AnimationSequenceAssetKind::KIND, sequence.as_str().to_string()),
                    PoseSource::BlendSpace { blend_space } => (BlendSpaceAssetKind::KIND, blend_space.as_str().to_string()),
                };
                let status = self.statuses.get(&s.id).cloned().unwrap_or(RefStatus::Unset);
                body = body.child(div().text_sm().font_semibold().text_color(theme.foreground).child("Source Clip"));
                body = body.child(self.widgets.ref_field("src.asset", "Asset", kind, skeleton_id.clone(), Some(current.clone()), &status, &root, window, cx));
                if let Some(path) = ProjectRefIndex::new(&root).locate_path(kind, &current) {
                    body = body.child(Button::new("pdb-open-src").icon(IconName::ExternalLink).label("Open source").small().ghost().on_click(move |_, window, cx| window.dispatch_action(Box::new(OpenAsset { path: path.clone() }), cx)));
                }
                let (e, m, l, sid) = (s.enabled, s.mirrored, s.looping, s.id.clone());
                let flag = |label: &'static str, id: &'static str, on: bool, which: u8, cx: &mut Context<Self>| {
                    let sid = sid.clone();
                    labelled(label, Button::new(id).label(if on { "On" } else { "Off" }).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                        let sid = sid.clone();
                        let (e, m, l) = match which {
                            0 => (!e, m, l),
                            1 => (e, !m, l),
                            _ => (e, m, !l),
                        };
                        edit(this, window, cx, |model| {
                            model.set_source_flags(&sid, e, m, l);
                            Ok(())
                        })
                    })), cx)
                };
                body = body.child(flag("Enabled", "pdb-en", e, 0, cx));
                body = body.child(flag("Mirrored", "pdb-mir", m, 1, cx));
                body = body.child(flag("Looping", "pdb-loop", l, 2, cx));
                body = body.child(self.widgets.text_field("src.bias", "Cost bias", &fmt_f32(s.cost_bias), window, cx));
                let mut ex = Vec::new();
                for (i, r) in s.exclusions.iter().enumerate() {
                    let sid = s.id.clone();
                    ex.push(h_flex().w_full().justify_between().items_center().child(div().text_xs().text_color(theme.foreground).child(format!("{} \u{2192} {} s", fmt_f32(r.start), fmt_f32(r.end)))).child(Button::new(SharedString::from(format!("pdb-ex-del-{i}"))).icon(IconName::Trash).xsmall().ghost().on_click(cx.listener(move |this, _, window, cx| {
                        let sid = sid.clone();
                        edit(this, window, cx, |m| {
                            m.remove_exclusion(&sid, i);
                            Ok(())
                        })
                    }))).into_any_element());
                }
                let sid = s.id.clone();
                ex.push(Button::new("pdb-ex-add").icon(IconName::Plus).label("Exclude the first 0.2 s").small().ghost().on_click(cx.listener(move |this, _, window, cx| {
                    let sid = sid.clone();
                    edit(this, window, cx, |m| m.add_exclusion(&sid, 0.0, 0.2))
                })).into_any_element());
                body = body.child(section("Excluded ranges", ex, cx));
                let sid = s.id.clone();
                body = body.child(Button::new("pdb-src-del").icon(IconName::Trash).label("Delete Source").small().danger().on_click(cx.listener(move |this, _, window, cx| {
                    let sid = sid.clone();
                    edit(this, window, cx, |m| {
                        m.delete_source(&sid);
                        Ok(())
                    })
                })));
            }
            AnimSelection::PoseChannel { index } => {
                let Some(c) = self.model.asset().channels.get(index).cloned() else { return empty_inspector(&self.model.selection, "The channel no longer exists.", cx) };
                body = body.child(div().text_sm().font_semibold().text_color(theme.foreground).child(format!("Channel: {}", c.name)));
                body = body.child(value_row("Kind", match &c.kind {
                    ChannelKind::BonePosition { bone } => format!("Bone position \u{b7} {bone}"),
                    ChannelKind::BoneRotation { bone } => format!("Bone rotation \u{b7} {bone}"),
                    ChannelKind::BoneVelocity { bone } => format!("Bone velocity \u{b7} {bone}"),
                    ChannelKind::Trajectory { .. } => "Root trajectory".to_string(),
                    ChannelKind::Curve { curve } => format!("Curve \u{b7} {curve}"),
                    ChannelKind::Phase => "Gait phase".to_string(),
                }, cx));
                body = body.child(self.widgets.text_field("ch.weight", "Weight", &fmt_f32(c.weight), window, cx));
                let norm = c.normalization;
                body = body.child(labelled("Normalisation", Button::new("pdb-norm").label(format!("{norm:?}")).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                    let next = if norm == Normalization::None { Normalization::Standardize } else { Normalization::None };
                    edit(this, window, cx, |m| {
                        m.set_channel_normalization(index, next);
                        Ok(())
                    })
                })), cx));
                if let ChannelKind::Trajectory { offsets, position, facing, velocity } = &c.kind {
                    body = body.child(self.widgets.text_field("ch.offsets", "Sample times (s)", &offsets.iter().map(|o| fmt_f32(*o)).collect::<Vec<_>>().join(", "), window, cx));
                    let (o, p, f, v) = (offsets.clone(), *position, *facing, *velocity);
                    for (label, which, on, id) in [("Position", 0u8, p, "pdb-tp"), ("Facing", 1, f, "pdb-tf"), ("Velocity", 2, v, "pdb-tv")] {
                        let o = o.clone();
                        body = body.child(labelled(label, Button::new(id).label(if on { "On" } else { "Off" }).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                            let (mut p, mut f, mut v) = (p, f, v);
                            match which {
                                0 => p = !p,
                                1 => f = !f,
                                _ => v = !v,
                            }
                            let o = o.clone();
                            edit(this, window, cx, |m| m.set_trajectory(index, o, p, f, v))
                        })), cx));
                    }
                }
                body = body.child(Button::new("pdb-ch-del").icon(IconName::Trash).label("Delete Channel").small().danger().on_click(cx.listener(move |this, _, window, cx| {
                    edit(this, window, cx, |m| {
                        m.delete_channel(index);
                        Ok(())
                    })
                })));
            }
            AnimSelection::PoseResult { index } => {
                let Some(q) = self.model.queries.get(index).cloned() else { return empty_inspector(&self.model.selection, "The query is gone.", cx) };
                body = body.child(div().text_sm().font_semibold().text_color(theme.foreground).child(format!("Match #{}", index + 1)));
                body = body.child(value_row("Source", q.source.clone(), cx));
                body = body.child(value_row("Clip time", format!("{} s", fmt_f32(q.result.clip_time)), cx));
                body = body.child(value_row("Total cost", format!("{:.4}", q.result.cost), cx));
                body = body.child(value_row("Continuing pose", q.result.continuing.to_string(), cx));
                let rows: Vec<AnyElement> = q.result.breakdown.iter().map(|c| value_row(&c.channel, format!("{:.4}", c.cost), cx)).collect();
                body = body.child(section("Cost per channel", rows, cx));
                let cands: Vec<AnyElement> = q.result.candidates.iter().map(|(entry, cost)| value_row(&format!("entry {entry}"), format!("{cost:.4}"), cx)).collect();
                body = body.child(section("Candidates", cands, cx));
            }
            _ => {
                let a = self.model.asset().clone();
                let status = self.statuses.get("skeleton").cloned().unwrap_or(RefStatus::Unset);
                body = body.child(div().text_sm().font_semibold().text_color(theme.foreground).child("Pose Search Database"));
                body = body.child(self.widgets.text_field("name", "Name", &a.name, window, cx));
                body = body.child(self.widgets.ref_field("skeleton", "Skeleton", SkeletonAssetKind::KIND, None, skeleton_id.clone(), &status, &root, window, cx));
                body = body.child(section(
                    "Add source",
                    vec![
                        self.widgets.ref_field("add.sequence", "Sequence", AnimationSequenceAssetKind::KIND, skeleton_id.clone(), None, &RefStatus::Unset, &root, window, cx),
                        self.widgets.ref_field("add.blendspace", "Blend Space", BlendSpaceAssetKind::KIND, skeleton_id.clone(), None, &RefStatus::Unset, &root, window, cx),
                    ],
                    cx,
                ));
                let bone_status: Vec<AnyElement> = vec![
                    self.widgets.text_field("add.bone", "Bone", "", window, cx),
                    h_flex().gap_1().flex_wrap()
                        .child(Button::new("pdb-add-pos").label("Position").small().outline().on_click(cx.listener(|this, _, window, cx| this.add_bone_channel(0, window, cx))))
                        .child(Button::new("pdb-add-rot").label("Rotation").small().outline().on_click(cx.listener(|this, _, window, cx| this.add_bone_channel(1, window, cx))))
                        .child(Button::new("pdb-add-vel").label("Velocity").small().outline().on_click(cx.listener(|this, _, window, cx| this.add_bone_channel(2, window, cx))))
                        .into_any_element(),
                    h_flex().gap_1().flex_wrap()
                        .child(Button::new("pdb-add-traj").label("Trajectory").small().outline().on_click(cx.listener(|this, _, window, cx| {
                            edit(this, window, cx, |m| {
                                let name = format!("Trajectory {}", m.asset().channels.len() + 1);
                                m.add_trajectory_channel(&name, vec![0.2, 0.4, 0.6], 1.0).map(|_| ())
                            })
                        })))
                        .child(Button::new("pdb-add-phase").label("Phase").small().outline().on_click(cx.listener(|this, _, window, cx| {
                            edit(this, window, cx, |m| m.add_phase_channel(1.0).map(|_| ()))
                        })))
                        .into_any_element(),
                ];
                body = body.child(section("Add feature channel", bone_status, cx));
                body = body.child(section(
                    "Sampling",
                    vec![
                        self.widgets.text_field("sampling.interval", "Interval (s)", &fmt_f32(a.sampling.interval), window, cx),
                        self.widgets.text_field("sampling.margin", "End margin (s)", &fmt_f32(a.sampling.end_margin), window, cx),
                    ],
                    cx,
                ));
                body = body.child(section(
                    "Search (not part of the index)",
                    vec![
                        self.widgets.text_field("search.candidates", "Candidates", &a.search.candidate_count.to_string(), window, cx),
                        self.widgets.text_field("search.bias", "Continuing pose bias", &fmt_f32(a.search.continuing_pose_bias), window, cx),
                        self.widgets.text_field("search.blend", "Blend time (s)", &fmt_f32(a.search.blend_time), window, cx),
                        self.widgets.text_field("search.history", "Query history", &a.search.query_history.to_string(), window, cx),
                    ],
                    cx,
                ));
                let (speed, time) = (self.query_speed, self.query_time);
                body = body.child(section(
                    "Query (not saved)",
                    vec![self.widgets.text_field("q.speed", "Desired speed", &fmt_f32(speed), window, cx), self.widgets.text_field("q.time", "Pose time (s)", &fmt_f32(time), window, cx)],
                    cx,
                ));
            }
        }
        body.into_any_element()
    }
}

impl PoseDbEditor {
    /// Add a bone channel from the "Bone" field. `kind`: 0 position, 1 rotation, 2 velocity.
    fn add_bone_channel(&mut self, kind: u8, window: &mut Window, cx: &mut Context<Self>) {
        let bone_text = self.widgets.input("add.bone", "", window, cx).read(cx).value().to_string();
        let bone = bone_text.trim().to_string();
        edit(self, window, cx, |m| {
            let name = format!("{}{}", bone, ["Position", "Rotation", "Velocity"][kind as usize]);
            match kind {
                0 => m.add_bone_channel(&name, &bone, true, 1.0),
                1 => m.add_rotation_channel(&name, &bone, 1.0),
                _ => m.add_bone_channel(&name, &bone, false, 1.0),
            }
            .map(|_| ())
        });
    }
}

impl Render for PoseDbEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame(self, window, cx)
    }
}

crate::editor_model!(PoseDbModel, <pulsar_reflection::PoseDatabaseAssetKind as AssetKind>::KIND);
crate::anim_panel!(PoseDbEditor, "pose-search-database-editor");

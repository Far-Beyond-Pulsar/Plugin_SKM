//! `SkeletalAnimEditorPanel` — the top-level editor entity.
//!
//! Owns the skeleton/animation data model, selection state, playback state,
//! and the nine transform `NumberInput`s shown in the properties panel. The
//! dock workspace (center viewport, bone hierarchy, properties, timeline) is
//! built lazily on first render in [`super::workspace`].
//!
//! One panel type serves the three assets (see [`EditorMode`]). In `Skeleton` mode
//! it edits a `.skeleton` file's bones and bind pose. In `Sequence` mode it
//! edits a `.animseq` file; the skeleton shown is the one the sequence
//! references by id, resolved through the project [`Library`] and read-only.
//! In `Mesh` mode it edits a `.skmesh`: the skeleton is again the referenced
//! one, and the mesh is drawn skinned to it, with the weights inspectable.
//!
//! A skeleton or sequence can also name a skeletal mesh to preview (by id,
//! resolved through the [`MeshLibrary`]); the mesh is then drawn posed by the
//! clip, for orientation only.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::*;
use ui::input::{InputEvent, NumberInputEvent, StepAction};

use crate::core::assets::{
    self, AnimSeqAsset, SkeletonAsset, ANIMSEQ_EXTENSION, SKELETON_EXTENSION,
};
use crate::core::library::{self, Entry};
use crate::core::migrate;
use crate::core::skmesh::{self, SkeletalMeshAsset, SKMESH_EXTENSION};
use crate::core::validate::Issue;
use crate::core::{
    self, evaluate_world_transforms, AnimSequenceId, AnimationClip, BoneTrack, Keyframe, Library,
    MeshLibrary, MeshLink, SkeletalMeshId, Skeleton, SkeletonId, SkeletonLink, Transform, Vec3,
};
use crate::rendering::{TimelinePanel, ViewportPanel};

use super::transform_inputs::{TransformGroup, TransformInputs};

/// Which asset the panel is editing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorMode {
    /// A `.skeleton`: hierarchy, properties and viewport, no timeline.
    Skeleton,
    /// An `.animseq`: timeline, viewport and hierarchy over its skeleton.
    Sequence,
    /// A `.skmesh`: the mesh skinned to its skeleton, with weights, joints,
    /// material slots and statistics, no timeline.
    Mesh,
}

/// What kind of thing a path opened in the editor is.
enum Opened {
    Skeleton,
    Sequence,
    Mesh,
    /// An old `.skelanim` folder, converted on open.
    Legacy,
}

impl Opened {
    fn of(path: &Path) -> Option<Self> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some(SKELETON_EXTENSION) => Some(Self::Skeleton),
            Some(ANIMSEQ_EXTENSION) => Some(Self::Sequence),
            Some(SKMESH_EXTENSION) => Some(Self::Mesh),
            _ if migrate::is_legacy_folder(path) => Some(Self::Legacy),
            _ => None,
        }
    }
}

/// Maximum time difference, in seconds, for a keyframe to be considered "at"
/// the playhead (and thus the properties panel's "Key" indicators to show as
/// keyed / editable).
const KEYFRAME_EPS: f32 = 1e-4;

/// Playback transport state for the timeline.
pub struct Playback {
    /// Current playhead position, in seconds.
    pub time: f32,
    pub playing: bool,
    /// Wrap back to the start of the play range instead of stopping at the end.
    pub looping: bool,
    /// Playback rate multiplier (1.0 = real time).
    pub speed: f32,
}

/// Playback rates the transport cycles through.
pub const PLAYBACK_SPEEDS: [f32; 5] = [0.25, 0.5, 1.0, 1.5, 2.0];

/// What the 3D viewport draws, toggled from its overlay toolbar.
#[derive(Clone, Copy)]
pub struct ViewOptions {
    pub grid: bool,
    pub bones: bool,
    /// Pivot dots on every joint.
    pub joints: bool,
    /// Ghosted bones where they are hidden behind other geometry.
    pub xray: bool,
    /// Translucent poses a few frames either side of the playhead.
    pub onion_skin: bool,
    /// The selected bone's path through the play range.
    pub motion_path: bool,
    /// The skeletal mesh, skinned by the pose (when there is one).
    pub mesh: bool,
    /// Color the mesh by the selected bone's weights.
    pub weights: bool,
    /// Per-bone colliders and joint limits from the matching `.physasset`.
    pub physics: bool,
    /// LOD currently shown in the viewport; explicitly selectable independent
    /// of zoom so artists can inspect all imported levels.
    pub mesh_lod: usize,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            grid: true,
            bones: true,
            joints: true,
            xray: true,
            onion_skin: false,
            motion_path: true,
            mesh: true,
            weights: true,
            physics: true,
            mesh_lod: 0,
        }
    }
}

/// One copied keyframe: its track, time relative to the earliest copied key,
/// and pose.
#[derive(Clone)]
pub struct ClipKey {
    pub bone_id: String,
    pub dt: f32,
    pub transform: Transform,
}

/// Snapshot taken when a keyframe drag begins; every move is computed from it
/// so the drag is lossless and can be cancelled.
pub struct KeyDrag {
    tracks: Vec<BoneTrack>,
    selection: Vec<(String, usize)>,
    anchor: (String, usize),
    min_time: f32,
    was_dirty: bool,
    pub moved: bool,
}

#[derive(Default)]
pub struct DebugOverlay {
    pub show_tracers: bool,
}

/// Per-bone tracer trail state: positions recorded each frame during playback.
pub struct TracerState {
    pub trails: HashMap<String, Vec<Vec3>>,
    pub colors: HashMap<String, [f32; 4]>,
}

impl TracerState {
    pub fn new(skeleton: &Skeleton) -> Self {
        let colors = skeleton
            .bones
            .iter()
            .map(|b| (b.id.clone(), Self::bone_color(&b.id)))
            .collect();
        Self {
            trails: HashMap::new(),
            colors,
        }
    }

    fn bone_color(id: &str) -> [f32; 4] {
        let h = id
            .bytes()
            .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
        let hue = ((h % 997) as f32 * 0.6180339887).fract() * 360.0;
        let s = 0.65 + ((h >> 8) & 7) as f32 * 0.05;
        let l = 0.50 + ((h >> 11) & 7) as f32 * 0.05;
        let c = (1.0_f32 - (2.0 * l - 1.0_f32).abs()) * s;
        let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
        let m = l - c / 2.0;
        let (r, g, b) = match (hue as u32) / 60 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        [r + m, g + m, b + m, 0.85]
    }
}

pub struct SkeletalAnimEditorPanel {
    pub(crate) focus_handle: FocusHandle,
    pub(crate) workspace: Option<Entity<ui::workspace::Workspace>>,

    pub mode: EditorMode,
    /// The `.skeleton`, `.animseq` or `.skmesh` file being edited, if opened from disk.
    pub current_asset_path: Option<PathBuf>,
    pub is_dirty: bool,

    /// The bones on screen. In `Skeleton` mode this is the asset being edited;
    /// in `Sequence` mode it is a copy of the referenced skeleton (empty while
    /// that is unresolved) and is never written back.
    pub skeleton: Skeleton,
    /// Bumped whenever `skeleton` is replaced, so snapshots of it refresh.
    pub skeleton_revision: u64,
    pub(crate) hierarchy_revision: u64,
    /// The sequence being edited; empty in `Skeleton` mode.
    pub animation: AnimationClip,

    /// `Skeleton` mode: the edited skeleton's own id.
    pub asset_id: Option<SkeletonId>,
    /// `Sequence` mode: the sequence's own stable id.
    pub animation_id: Option<AnimSequenceId>,
    /// `Sequence` and `Mesh` modes: the skeleton the asset refers to. Kept as is
    /// even when nothing in the project has this id.
    pub skeleton_ref: Option<SkeletonId>,
    /// What `skeleton_ref` currently resolves to.
    pub link: SkeletonLink,
    /// Skeletons in the project, as of the last scan.
    pub library: Library,
    pub project_root: PathBuf,

    /// `Mesh` mode: the edited mesh's own id.
    pub mesh_id: Option<SkeletalMeshId>,
    /// The skeletal mesh on screen: the one being edited in `Mesh` mode, the
    /// loaded preview mesh otherwise. Shared so a preview can be handed to the
    /// viewport cheaply; `Mesh` mode edits it through `Arc::make_mut`.
    pub mesh: Option<Arc<SkeletalMeshAsset>>,
    /// Bumped whenever `mesh` is replaced or edited.
    pub mesh_revision: u64,
    /// `Skeleton` and `Sequence` modes: the mesh previewed with the asset.
    /// Kept as is even when nothing in the project has this id.
    pub preview_mesh_ref: Option<SkeletalMeshId>,
    /// What `preview_mesh_ref` currently resolves to.
    pub preview_link: MeshLink,
    /// Skeletal meshes in the project, as of the last scan.
    pub mesh_library: MeshLibrary,
    /// File the preview `mesh` was loaded from, so a rescan that finds the
    /// same file does not load it again.
    pub(crate) preview_loaded_from: Option<PathBuf>,
    /// Problems with the asset and its links; recomputed on changes.
    pub issues: Vec<Issue>,
    /// One-off message under the toolbar (e.g. that an old asset was converted).
    pub notice: Option<String>,
    pub(crate) skeleton_picker: Option<Entity<ui_common::MeshAssetPicker>>,
    pub(crate) mesh_picker: Option<Entity<ui_common::MeshAssetPicker>>,
    /// `Mesh` mode: one material picker per slot.
    pub(crate) slot_pickers: Vec<Entity<ui_common::MeshAssetPicker>>,
    /// Counts library scans, so a stale one finishing late is ignored.
    pub(crate) scan_generation: u64,
    /// Counts preview-mesh loads, for the same reason.
    pub(crate) mesh_generation: u64,

    pub(crate) sockets: skeletal_assets::sockets::Sockets,
    pub(crate) selected_socket: Option<String>,
    pub(crate) hierarchy_history:
        plugin_editor_api::SharedHistory<super::hierarchy_history::HierarchyEdit>,
    pub(crate) checking_hierarchy: bool,
    pub(crate) saving_hierarchy: bool,
    pub(crate) saved_hierarchy: Skeleton,
    pub(crate) saved_asset_id: Option<SkeletonId>,
    pub(crate) saved_sockets: skeletal_assets::sockets::Sockets,
    pub(crate) saved_preview_ref: Option<SkeletalMeshId>,
    pub selected_bone: Option<String>,
    pub expanded_bones: HashSet<String>,
    /// "Primary" selected keyframe, used by the properties panel.
    pub selected_keyframe: Option<(String, usize)>,
    /// Full multi-selection of keyframes (box/marquee select), as
    /// `(bone_id, keyframe_index)` pairs. Always a superset containing
    /// `selected_keyframe` when non-empty.
    pub selected_keyframes: HashSet<(String, usize)>,
    pub playback: Playback,

    /// Inclusive frame range to play/loop, e.g. `(-1, 10)` for an 11-frame
    /// clip with one frame of pre-roll. Editable in the timeline's top bar.
    /// The area outside this range is hatched out in the timeline.
    pub play_range: (i32, i32),

    pub(crate) viewport_panel: Option<Entity<ViewportPanel>>,
    pub(crate) timeline_panel: Option<Entity<TimelinePanel>>,

    pub transform_inputs: TransformInputs,
    pub debug: DebugOverlay,
    pub view_options: ViewOptions,
    pub tracer_state: TracerState,
    /// Bone under the viewport cursor, for hover highlighting.
    pub hovered_bone: Option<String>,
    pub clipboard: Vec<ClipKey>,

    pub(crate) subscriptions: Vec<Subscription>,
}

impl SkeletalAnimEditorPanel {
    /// Create a new sequence editor populated with a sample skeleton and a
    /// running animation that refers to it by id. Nothing is on disk.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (skeleton, sequence) = core::sample::sample_assets();
        let entry = Entry {
            id: skeleton.id.clone(),
            path: PathBuf::new(),
            name: "Humanoid (sample)".to_string(),
            skeleton: Arc::new(skeleton.skeleton()),
            sockets: Default::default(),
        };
        let mut panel = Self::new_internal(
            EditorMode::Sequence,
            skeleton.skeleton(),
            sequence.clip,
            None,
            window,
            cx,
        );
        panel.skeleton_ref = Some(entry.id.clone());
        panel.link = SkeletonLink::Resolved(entry);
        panel.refresh_issues();
        panel
    }

    /// Open a `.skeleton`, `.animseq` or `.skmesh` file. An old `.skelanim` folder is
    /// first converted to a pair of such files next to it (leaving the folder
    /// alone), and the new sequence is opened.
    pub fn new_with_path(
        file_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Self> {
        match Opened::of(&file_path) {
            Some(Opened::Skeleton) => Self::open_skeleton(file_path, window, cx),
            Some(Opened::Sequence) => Self::open_sequence(file_path, None, window, cx),
            Some(Opened::Mesh) => Self::open_mesh(file_path, window, cx),
            Some(Opened::Legacy) => {
                let migrated = migrate::migrate_folder(&file_path)?;
                let name = |p: &Path| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                };
                let notice = format!(
                    "{} {} as {} and {}. The original folder was left in place.",
                    if migrated.reused {
                        "Opened the earlier conversion of"
                    } else {
                        "Converted"
                    },
                    name(&file_path),
                    name(&migrated.skeleton_path),
                    name(&migrated.sequence_path),
                );
                Self::open_sequence(migrated.sequence_path, Some(notice), window, cx)
            }
            None => anyhow::bail!(
                "{} is not a skeleton, skeletal mesh, animation sequence or old skeletal animation",
                file_path.display()
            ),
        }
    }

    fn open_skeleton(
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Self> {
        let text = assets::read_text(&path)?;
        let had_id = SkeletonAsset::peek_id(&text).is_some();
        let asset =
            SkeletonAsset::parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;

        let mut panel = Self::new_internal(
            EditorMode::Skeleton,
            asset.skeleton(),
            AnimationClip::bind_pose(),
            Some(path.clone()),
            window,
            cx,
        );
        panel.sockets = asset.sockets.clone();
        panel.saved_sockets = asset.sockets.clone();
        panel.saved_preview_ref = asset.preview_mesh.clone();
        panel.asset_id = Some(asset.id.clone());
        panel.project_root = library::project_root(&path);
        panel.mesh_library = MeshLibrary::empty(panel.project_root.clone());
        panel.set_preview_ref_unscanned(asset.preview_mesh.clone());
        // A new file has no id yet; give it one now so it can be referenced
        // before the first save.
        if !had_id {
            if let Err(err) = assets::save_skeleton_asset(&path, &asset) {
                log::warn!("Failed to write the id into {path:?}: {err}");
                panel.is_dirty = true;
            }
        }
        panel.saved_asset_id = (!panel.is_dirty).then(|| asset.id.clone());
        remember_asset_id(&panel.project_root, &asset.id.to_string(), &path);
        panel.refresh_issues();
        if panel.preview_mesh_ref.is_some() {
            panel.rescan_library(cx);
        }
        Ok(panel)
    }

    fn open_sequence(
        path: PathBuf,
        notice: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Self> {
        let text = assets::read_text(&path)?;
        let had_id = AnimSeqAsset::peek_id(&text).is_some();
        let asset = AnimSeqAsset::parse(&text)
            .map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))?;
        // The skeleton arrives when the project scan finishes.
        let mut panel = Self::new_internal(
            EditorMode::Sequence,
            Skeleton::default(),
            asset.clip.clone(),
            Some(path.clone()),
            window,
            cx,
        );
        panel.animation_id = Some(asset.id.clone());
        panel.project_root = library::project_root(&path);
        panel.library = Library::empty(panel.project_root.clone());
        panel.link = match &asset.skeleton {
            Some(id) => SkeletonLink::Resolving(id.clone()),
            None => SkeletonLink::Unset,
        };
        panel.skeleton_ref = asset.skeleton.clone();
        panel.mesh_library = MeshLibrary::empty(panel.project_root.clone());
        panel.set_preview_ref_unscanned(asset.preview_mesh.clone());
        panel.notice = notice;
        if !had_id {
            if let Err(error) = assets::save_animseq_asset(&path, &asset) {
                log::warn!("Failed to write the id into {path:?}: {error}");
                panel.is_dirty = true;
            }
        }
        remember_asset_id(&panel.project_root, &asset.id.to_string(), &path);
        panel.refresh_issues();
        if panel.skeleton_ref.is_some() || panel.preview_mesh_ref.is_some() {
            panel.rescan_library(cx);
        }
        Ok(panel)
    }

    fn open_mesh(
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Self> {
        let asset = skmesh::load_skmesh_asset(&path)?;
        // The skeleton arrives when the project scan finishes; until then the
        // mesh is drawn as stored.
        let mut panel = Self::new_internal(
            EditorMode::Mesh,
            Skeleton::default(),
            AnimationClip::bind_pose(),
            Some(path.clone()),
            window,
            cx,
        );
        panel.project_root = library::project_root(&path);
        panel.library = Library::empty(panel.project_root.clone());
        panel.link = match &asset.skeleton {
            Some(id) => SkeletonLink::Resolving(id.clone()),
            None => SkeletonLink::Unset,
        };
        panel.skeleton_ref = asset.skeleton.clone();
        panel.mesh_id = Some(asset.id.clone());
        panel.mesh = Some(Arc::new(asset));
        panel.mesh_revision += 1;
        panel.refresh_issues();
        if panel.skeleton_ref.is_some() {
            panel.rescan_library(cx);
        }
        Ok(panel)
    }

    fn new_internal(
        mode: EditorMode,
        skeleton: Skeleton,
        animation: AnimationClip,
        asset_path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let transform_inputs = TransformInputs::new(window, cx);
        let mut subscriptions = Vec::new();
        for input in transform_inputs.all() {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |this, _input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => this.apply_transform_inputs(window, cx),
                    InputEvent::Focus | InputEvent::Blur | InputEvent::PressEnter { .. } => {
                        plugin_editor_api::undo::lock(&this.hierarchy_history).seal();
                    }
                    _ => {}
                },
            ));
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |this, input, event: &NumberInputEvent, window, cx| {
                    let NumberInputEvent::Step { action, fine } = event;
                    let step = if *fine { 0.1 } else { 1.0 };
                    input.update(cx, |input, cx| {
                        let value = input.value().parse::<f32>().unwrap_or(0.0);
                        let new_value = match action {
                            StepAction::Increment => value + step,
                            StepAction::Decrement => value - step,
                        };
                        input.set_value(format!("{:.3}", new_value), window, cx);
                    });
                    this.apply_transform_inputs(window, cx);
                },
            ));
        }

        let root_id = skeleton.root_bones().first().map(|b| b.id.clone());
        let default_play_range = (
            0,
            (animation.duration * animation.fps).round().max(0.0) as i32,
        );
        let tracer_state = TracerState::new(&skeleton);

        let mut panel = Self {
            focus_handle: cx.focus_handle(),
            workspace: None,
            mode,
            current_asset_path: asset_path,
            is_dirty: false,
            saved_hierarchy: skeleton.clone(),
            saved_asset_id: None,
            skeleton,
            skeleton_revision: 0,
            hierarchy_revision: 0,
            animation,
            asset_id: None,
            animation_id: (mode == EditorMode::Sequence).then(AnimSequenceId::new),
            skeleton_ref: None,
            link: SkeletonLink::Unset,
            library: Library::default(),
            project_root: PathBuf::new(),
            mesh_id: None,
            mesh: None,
            mesh_revision: 0,
            preview_mesh_ref: None,
            preview_link: MeshLink::Unset,
            mesh_library: MeshLibrary::default(),
            preview_loaded_from: None,
            issues: Vec::new(),
            notice: None,
            skeleton_picker: None,
            mesh_picker: None,
            slot_pickers: Vec::new(),
            scan_generation: 0,
            mesh_generation: 0,
            selected_socket: None,
            sockets: Default::default(),
            saved_sockets: Default::default(),
            saved_preview_ref: None,
            hierarchy_history: Arc::new(std::sync::Mutex::new(
                plugin_editor_api::History::default(),
            )),
            checking_hierarchy: false,
            saving_hierarchy: false,
            selected_bone: None,
            expanded_bones: HashSet::new(),
            selected_keyframe: None,
            selected_keyframes: HashSet::new(),
            playback: Playback {
                time: 0.0,
                playing: false,
                looping: true,
                speed: 1.0,
            },
            play_range: default_play_range,
            viewport_panel: None,
            timeline_panel: None,
            transform_inputs,
            debug: DebugOverlay::default(),
            view_options: ViewOptions::default(),
            tracer_state,
            hovered_bone: None,
            clipboard: Vec::new(),
            subscriptions,
        };

        if let Some(root_id) = root_id {
            panel.expanded_bones.insert(root_id);
        }
        // The weight heat map is what a mesh is edited for; a preview mesh is only
        // there for orientation and should not change color with the selection.
        panel.view_options.weights = mode == EditorMode::Mesh;

        panel
    }

    /// Select a bone (or clear selection), refreshing the properties panel.
    /// The keyframe selection is resynced to whatever (if anything) the new
    /// bone has keyed at the current playhead time.
    pub fn select_bone(
        &mut self,
        bone_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = &bone_id {
            self.expand_ancestors(id);
        }
        plugin_editor_api::undo::lock(&self.hierarchy_history).seal();
        self.selected_socket = None;
        self.selected_bone = bone_id.filter(|id| self.skeleton.bone(id).is_some());
        self.resync_selected_keyframe();
        self.sync_transform_inputs(window, cx);
        cx.notify();
    }

    /// Select a single keyframe on a bone's track, replacing any existing
    /// multi-selection, and refresh the properties panel.
    pub fn select_keyframe(
        &mut self,
        keyframe: Option<(String, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_socket = None;
        self.selected_keyframes.clear();
        if let Some(key) = &keyframe {
            self.selected_keyframes.insert(key.clone());
        }
        self.selected_keyframe = keyframe;
        self.sync_transform_inputs(window, cx);
        cx.notify();
    }

    /// Toggle whether `key` is part of the multi-selection (shift-click).
    pub fn toggle_keyframe_selection(
        &mut self,
        key: (String, usize),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_socket = None;
        if !self.selected_keyframes.remove(&key) {
            self.selected_keyframes.insert(key.clone());
        }
        self.selected_keyframe = self.selected_keyframes.iter().next().cloned();
        self.sync_transform_inputs(window, cx);
        cx.notify();
    }

    /// Replace the keyframe multi-selection wholesale (used by box-select).
    /// If `additive` is true, `keys` are merged into the existing selection
    /// instead of replacing it.
    pub fn set_selected_keyframes(
        &mut self,
        keys: HashSet<(String, usize)>,
        additive: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_socket = None;
        if additive {
            self.selected_keyframes.extend(keys);
        } else {
            self.selected_keyframes = keys;
        }
        self.selected_keyframe = self.selected_keyframes.iter().next().cloned();
        self.sync_transform_inputs(window, cx);
        cx.notify();
    }

    /// The index of `bone_id`'s keyframe at the current playhead time (within
    /// `KEYFRAME_EPS`), if any. Used to drive the properties panel's "Key"
    /// indicators and to gate transform editing.
    pub fn keyframe_at_playhead(&self, bone_id: &str) -> Option<usize> {
        self.animation.track(bone_id).and_then(|t| {
            t.keyframes
                .iter()
                .position(|k| (k.time - self.playback.time).abs() < KEYFRAME_EPS)
        })
    }

    /// Recompute `selected_keyframe`/`selected_keyframes` from the selected
    /// bone and current playhead time: if the bone has a keyframe at the
    /// playhead, it becomes the (sole) selection; otherwise the keyframe
    /// selection is cleared. Does not touch `selected_bone` or notify.
    fn resync_selected_keyframe(&mut self) {
        let key = self.selected_bone.as_ref().and_then(|bone_id| {
            self.keyframe_at_playhead(bone_id)
                .map(|idx| (bone_id.clone(), idx))
        });
        self.selected_keyframes.clear();
        if let Some(key) = &key {
            self.selected_keyframes.insert(key.clone());
        }
        self.selected_keyframe = key;
    }

    /// Insert (or update) a keyframe on the selected bone's track at the
    /// current playhead time, capturing the bone's current pose so the
    /// insertion doesn't visibly change anything. Creates the track if the
    /// bone doesn't have one yet. No-op if no bone is selected.
    pub fn insert_keyframe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let Some(bone_id) = self.selected_bone.clone() else {
            return;
        };
        let time = self.playback.time;

        // Capture the pose to insert *before* taking a mutable borrow of the
        // track: the existing animated pose (if any), else the bind pose.
        let transform = self
            .animation
            .track(&bone_id)
            .and_then(|t| t.sample(time))
            .unwrap_or_else(|| {
                self.skeleton
                    .bone(&bone_id)
                    .map(|b| b.bind_transform.clone())
                    .unwrap_or_default()
            });

        if self.animation.track(&bone_id).is_none() {
            self.animation.tracks.push(BoneTrack {
                bone_id: bone_id.clone(),
                keyframes: Vec::new(),
            });
        }
        let track = self.animation.track_mut(&bone_id).unwrap();

        // Keyframes at (nearly) the same time are replaced in place rather
        // than duplicated.
        let index = match track
            .keyframes
            .iter()
            .position(|k| (k.time - time).abs() < KEYFRAME_EPS)
        {
            Some(i) => {
                track.keyframes[i].transform = transform;
                i
            }
            None => {
                let pos = track
                    .keyframes
                    .iter()
                    .position(|k| k.time > time)
                    .unwrap_or(track.keyframes.len());
                track
                    .keyframes
                    .insert(pos, Keyframe::linear(time, transform));
                pos
            }
        };

        self.is_dirty = true;
        self.select_keyframe(Some((bone_id, index)), window, cx);
    }

    /// Remove every keyframe in the current multi-selection from its track.
    /// No-op if nothing is selected.
    pub fn delete_selected_keyframes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        if self.selected_keyframes.is_empty() {
            return;
        }

        let mut by_track: HashMap<String, Vec<usize>> = HashMap::new();
        for (bone_id, index) in &self.selected_keyframes {
            by_track.entry(bone_id.clone()).or_default().push(*index);
        }

        for (bone_id, mut indices) in by_track {
            if let Some(track) = self.animation.track_mut(&bone_id) {
                // Remove highest indices first so earlier removals don't
                // shift the positions of indices still pending removal.
                indices.sort_unstable_by(|a, b| b.cmp(a));
                indices.dedup();
                for index in indices {
                    if index < track.keyframes.len() {
                        track.keyframes.remove(index);
                    }
                }
            }
        }

        // Drop tracks left with no keyframes, so the timeline doesn't show
        // an empty row for a bone that's no longer animated.
        self.animation.tracks.retain(|t| !t.keyframes.is_empty());

        self.is_dirty = true;
        self.select_keyframe(None, window, cx);
    }

    /// Remove the selected bone's keyframe at the current playhead time, if
    /// any. No-op if no bone is selected or it has no keyframe there.
    pub fn delete_keyframe_at_playhead(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let Some(bone_id) = self.selected_bone.clone() else {
            return;
        };
        let Some(index) = self.keyframe_at_playhead(&bone_id) else {
            return;
        };
        if let Some(track) = self.animation.track_mut(&bone_id) {
            track.keyframes.remove(index);
        }
        self.animation.tracks.retain(|t| !t.keyframes.is_empty());

        self.is_dirty = true;
        self.resync_selected_keyframe();
        self.sync_transform_inputs(window, cx);
        cx.notify();
    }

    // ── Frame navigation ───────────────────────────────────────────────────

    /// Frame the playhead is on.
    pub fn current_frame(&self) -> i32 {
        self.animation.time_to_frame(self.playback.time)
    }

    /// Move the playhead to a whole `frame`, clamped to the play range.
    pub fn goto_frame(&mut self, frame: i32, window: &mut Window, cx: &mut Context<Self>) {
        let time = self.animation.frame_to_time(frame);
        self.seek(time, window, cx);
    }

    /// Step the playhead by `delta` frames.
    pub fn step_frame(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        self.playback.playing = false;
        let frame = self.current_frame() + delta;
        self.goto_frame(frame, window, cx);
    }

    /// Jump to the first frame of the play range.
    pub fn goto_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.playback.playing = false;
        self.goto_frame(self.play_range.0, window, cx);
    }

    /// Jump to the last frame of the play range.
    pub fn goto_end(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.playback.playing = false;
        self.goto_frame(self.play_range.1, window, cx);
    }

    /// Key times that previous/next-keyframe navigation considers: the
    /// selected bone's track when it has one, otherwise every track.
    fn navigation_times(&self) -> Vec<f32> {
        let tracks: Vec<&BoneTrack> = match self
            .selected_bone
            .as_ref()
            .and_then(|id| self.animation.track(id))
        {
            Some(track) => vec![track],
            None => self.animation.tracks.iter().collect(),
        };
        let mut times: Vec<f32> = tracks
            .iter()
            .flat_map(|t| t.keyframes.iter().map(|k| k.time))
            .collect();
        times.sort_by(|a, b| a.total_cmp(b));
        times
    }

    /// Move the playhead to the next keyframe after it, if there is one.
    pub fn goto_next_keyframe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = self.playback.time;
        if let Some(t) = self
            .navigation_times()
            .into_iter()
            .find(|t| *t > now + KEYFRAME_EPS)
        {
            self.playback.playing = false;
            self.seek(t, window, cx);
        }
    }

    /// Move the playhead to the closest keyframe before it, if there is one.
    pub fn goto_prev_keyframe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = self.playback.time;
        if let Some(t) = self
            .navigation_times()
            .into_iter()
            .rev()
            .find(|t| *t < now - KEYFRAME_EPS)
        {
            self.playback.playing = false;
            self.seek(t, window, cx);
        }
    }

    /// Cycle through [`PLAYBACK_SPEEDS`].
    pub fn cycle_speed(&mut self, cx: &mut Context<Self>) {
        let next = PLAYBACK_SPEEDS
            .iter()
            .position(|s| (*s - self.playback.speed).abs() < 1e-3)
            .map_or(2, |i| (i + 1) % PLAYBACK_SPEEDS.len());
        self.playback.speed = PLAYBACK_SPEEDS[next];
        cx.notify();
    }

    /// Turn the per-bone tracer trails on or off, starting from a clean slate.
    pub fn toggle_tracers(&mut self, cx: &mut Context<Self>) {
        self.debug.show_tracers = !self.debug.show_tracers;
        if self.debug.show_tracers {
            self.tracer_state = TracerState::new(&self.skeleton);
        }
        cx.notify();
    }

    pub fn toggle_loop(&mut self, cx: &mut Context<Self>) {
        self.playback.looping = !self.playback.looping;
        cx.notify();
    }

    // ── Keyframe editing ───────────────────────────────────────────────────

    /// Index of `bone_id`'s key at `time`, if one exists.
    fn key_index_at(&self, bone_id: &str, time: f32) -> Option<usize> {
        self.animation.track(bone_id).and_then(|t| {
            t.keyframes
                .iter()
                .position(|k| (k.time - time).abs() < KEYFRAME_EPS)
        })
    }

    /// Set `bone_id`'s pose at `time`, creating the track or key as needed.
    /// Returns the key's index.
    fn upsert_key(&mut self, bone_id: &str, time: f32, transform: Transform) -> usize {
        if self.animation.track(bone_id).is_none() {
            self.animation.tracks.push(BoneTrack {
                bone_id: bone_id.to_string(),
                keyframes: Vec::new(),
            });
        }
        let track = self.animation.track_mut(bone_id).unwrap();
        match track
            .keyframes
            .iter()
            .position(|k| (k.time - time).abs() < KEYFRAME_EPS)
        {
            Some(i) => {
                track.keyframes[i].transform = transform;
                i
            }
            None => {
                let pos = track
                    .keyframes
                    .iter()
                    .position(|k| k.time > time)
                    .unwrap_or(track.keyframes.len());
                track
                    .keyframes
                    .insert(pos, Keyframe::linear(time, transform));
                pos
            }
        }
    }

    /// Key or un-key one specific bone at the playhead (the per-track button
    /// in the timeline gutter).
    pub fn toggle_key_for_bone(
        &mut self,
        bone_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.editable() {
            return;
        }
        let time = self.playback.time;
        if let Some(index) = self.key_index_at(bone_id, time) {
            if let Some(track) = self.animation.track_mut(bone_id) {
                track.keyframes.remove(index);
            }
            self.animation.tracks.retain(|t| !t.keyframes.is_empty());
            self.is_dirty = true;
            self.resync_selected_keyframe();
            self.sync_transform_inputs(window, cx);
            cx.notify();
        } else {
            let pose = self
                .animation
                .local_transform(&self.skeleton, bone_id, time);
            let index = self.upsert_key(bone_id, time, pose);
            self.is_dirty = true;
            self.selected_bone = Some(bone_id.to_string());
            self.select_keyframe(Some((bone_id.to_string(), index)), window, cx);
        }
    }

    /// Select every keyframe of every track.
    pub fn select_all_keyframes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let all: HashSet<(String, usize)> = self
            .animation
            .tracks
            .iter()
            .flat_map(|t| (0..t.keyframes.len()).map(move |k| (t.bone_id.clone(), k)))
            .collect();
        self.set_selected_keyframes(all, false, window, cx);
    }

    /// Select every key sitting at `time` across all tracks (summary row).
    pub fn select_keys_at_time(
        &mut self,
        time: f32,
        additive: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hits: HashSet<(String, usize)> = self
            .animation
            .tracks
            .iter()
            .flat_map(|t| {
                t.keyframes
                    .iter()
                    .enumerate()
                    .filter(move |(_, k)| (k.time - time).abs() < KEYFRAME_EPS)
                    .map(move |(i, _)| (t.bone_id.clone(), i))
            })
            .collect();
        self.set_selected_keyframes(hits, additive, window, cx);
    }

    /// Begin dragging the current keyframe selection, anchored on `anchor`.
    pub fn begin_key_drag(&self, anchor: (String, usize)) -> Option<KeyDrag> {
        if !self.editable() {
            return None;
        }
        if self.selected_keyframes.is_empty() {
            return None;
        }
        let selection: Vec<(String, usize)> = self.selected_keyframes.iter().cloned().collect();
        let min_time = selection
            .iter()
            .filter_map(|(bone, i)| {
                self.animation
                    .track(bone)?
                    .keyframes
                    .get(*i)
                    .map(|k| k.time)
            })
            .fold(f32::MAX, f32::min);
        Some(KeyDrag {
            tracks: self.animation.tracks.clone(),
            selection,
            anchor,
            min_time: if min_time.is_finite() { min_time } else { 0.0 },
            was_dirty: self.is_dirty,
            moved: false,
        })
    }

    /// Time of the drag's anchor key when the drag began.
    pub fn key_drag_anchor_time(&self, drag: &KeyDrag) -> f32 {
        drag.tracks
            .iter()
            .find(|t| t.bone_id == drag.anchor.0)
            .and_then(|t| t.keyframes.get(drag.anchor.1))
            .map_or(0.0, |k| k.time)
    }

    /// Lay the dragged keys out `delta` seconds from where they started. The
    /// result is always derived from the drag snapshot, so this can be called
    /// repeatedly with a changing delta. Keys can't be pushed before time 0,
    /// and a moved key replaces any other key it lands on.
    pub fn apply_key_drag(
        &mut self,
        drag: &mut KeyDrag,
        delta: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = delta.max(-drag.min_time);
        drag.moved |= delta.abs() > KEYFRAME_EPS;

        let selected: HashSet<(String, usize)> = drag.selection.iter().cloned().collect();
        let mut tracks = Vec::with_capacity(drag.tracks.len());
        let mut new_selection = HashSet::new();
        let mut new_primary = None;

        for track in &drag.tracks {
            let mut keys: Vec<(Keyframe, bool, usize)> = Vec::with_capacity(track.keyframes.len());
            let moved_times: Vec<f32> = track
                .keyframes
                .iter()
                .enumerate()
                .filter(|(i, _)| selected.contains(&(track.bone_id.clone(), *i)))
                .map(|(_, k)| k.time + delta)
                .collect();
            for (i, kf) in track.keyframes.iter().enumerate() {
                if selected.contains(&(track.bone_id.clone(), i)) {
                    keys.push((
                        Keyframe {
                            time: kf.time + delta,
                            ..kf.clone()
                        },
                        true,
                        i,
                    ));
                } else if !moved_times
                    .iter()
                    .any(|t| (t - kf.time).abs() < KEYFRAME_EPS)
                {
                    keys.push((kf.clone(), false, i));
                }
            }
            keys.sort_by(|a, b| a.0.time.total_cmp(&b.0.time));
            for (new_index, (_, moved, old_index)) in keys.iter().enumerate() {
                if *moved {
                    new_selection.insert((track.bone_id.clone(), new_index));
                    if (track.bone_id.as_str(), *old_index)
                        == (drag.anchor.0.as_str(), drag.anchor.1)
                    {
                        new_primary = Some((track.bone_id.clone(), new_index));
                    }
                }
            }
            tracks.push(BoneTrack {
                bone_id: track.bone_id.clone(),
                keyframes: keys.into_iter().map(|(k, _, _)| k).collect(),
            });
        }

        self.animation.tracks = tracks;
        self.selected_keyframes = new_selection;
        self.selected_keyframe =
            new_primary.or_else(|| self.selected_keyframes.iter().next().cloned());
        if drag.moved {
            self.is_dirty = true;
        }
        self.sync_transform_inputs(window, cx);
        cx.notify();
    }

    /// Finish a drag: grow the clip if keys were pushed past its end.
    pub fn finish_key_drag(&mut self, drag: KeyDrag, cx: &mut Context<Self>) {
        if drag.moved {
            let last = self
                .animation
                .tracks
                .iter()
                .filter_map(|t| t.keyframes.last().map(|k| k.time))
                .fold(0.0_f32, f32::max);
            if last > self.animation.duration {
                self.animation.duration = last;
            }
        }
        cx.notify();
    }

    /// Abandon a drag, restoring the keys exactly as they were.
    pub fn cancel_key_drag(&mut self, drag: KeyDrag, window: &mut Window, cx: &mut Context<Self>) {
        self.animation.tracks = drag.tracks;
        self.selected_keyframes = drag.selection.into_iter().collect();
        self.selected_keyframe = Some(drag.anchor).filter(|k| self.selected_keyframes.contains(k));
        self.is_dirty = drag.was_dirty;
        self.sync_transform_inputs(window, cx);
        cx.notify();
    }

    /// Copy the selected keys to the editor clipboard.
    pub fn copy_selected_keyframes(&mut self) {
        let mut keys: Vec<(String, f32, Transform)> = self
            .selected_keyframes
            .iter()
            .filter_map(|(bone, i)| {
                let kf = self.animation.track(bone)?.keyframes.get(*i)?;
                Some((bone.clone(), kf.time, kf.transform.clone()))
            })
            .collect();
        if keys.is_empty() {
            return;
        }
        keys.sort_by(|a, b| a.1.total_cmp(&b.1));
        let origin = keys[0].1;
        self.clipboard = keys
            .into_iter()
            .map(|(bone_id, time, transform)| ClipKey {
                bone_id,
                dt: time - origin,
                transform,
            })
            .collect();
    }

    /// Cut = copy + delete.
    pub fn cut_selected_keyframes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_selected_keyframes();
        self.delete_selected_keyframes(window, cx);
    }

    /// Paste the clipboard with its earliest key on the playhead.
    pub fn paste_keyframes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        if self.clipboard.is_empty() {
            return;
        }
        let origin = self.animation.snap_time(self.playback.time);
        let pasted: Vec<(String, f32)> = self
            .clipboard
            .clone()
            .into_iter()
            .map(|key| {
                let time = origin + key.dt;
                self.upsert_key(&key.bone_id, time, key.transform);
                (key.bone_id, time)
            })
            .collect();

        // Indices shift as keys are inserted, so resolve them at the end.
        let selection: HashSet<(String, usize)> = pasted
            .iter()
            .filter_map(|(bone, time)| self.key_index_at(bone, *time).map(|i| (bone.clone(), i)))
            .collect();
        let last = pasted.iter().map(|(_, t)| *t).fold(0.0_f32, f32::max);
        if last > self.animation.duration {
            self.animation.duration = last;
        }
        self.is_dirty = true;
        self.set_selected_keyframes(selection, false, window, cx);
    }

    /// Set the start of the playback range (inclusive frame number).
    pub fn set_play_range_start(&mut self, start: i32, cx: &mut Context<Self>) {
        self.play_range.0 = start;
        cx.notify();
    }

    /// Set the end of the playback range (inclusive frame number).
    pub fn set_play_range_end(&mut self, end: i32, cx: &mut Context<Self>) {
        self.play_range.1 = end;
        cx.notify();
    }

    /// The `(start, end)` playhead bounds in seconds, derived from
    /// `play_range` (inclusive frame range) and the clip's `fps`.
    pub(crate) fn play_range_seconds(&self) -> (f32, f32) {
        let fps = self.animation.fps.max(1.0);
        let (start_frame, end_frame) = self.play_range;
        let start = start_frame as f32 / fps;
        let end = ((end_frame + 1) as f32 / fps).max(start);
        (start, end)
    }

    /// Move the playhead to `time`, clamped to the playback range, resyncing
    /// the selected keyframe and properties panel to the new time.
    pub fn seek(&mut self, time: f32, window: &mut Window, cx: &mut Context<Self>) {
        let (start, end) = self.play_range_seconds();
        self.playback.time = time.clamp(start, end);
        self.resync_selected_keyframe();
        self.sync_transform_inputs(window, cx);
        cx.notify();
    }

    /// Toggle whether `bone_id`'s children are shown in the hierarchy panel.
    pub fn toggle_bone_expanded(&mut self, bone_id: &str, cx: &mut Context<Self>) {
        if !self.expanded_bones.remove(bone_id) {
            self.expanded_bones.insert(bone_id.to_string());
        }
        cx.notify();
    }

    pub fn is_bone_expanded(&self, bone_id: &str) -> bool {
        self.expanded_bones.contains(bone_id)
    }

    /// Expand every bone that has children.
    pub fn expand_all_bones(&mut self, cx: &mut Context<Self>) {
        self.expanded_bones
            .extend(self.skeleton.bones.iter().filter_map(|b| b.parent.clone()));
        self.expanded_bones
            .extend(self.sockets.iter().map(|s| s.bone_id.clone()));
        cx.notify();
    }

    /// Collapse the whole tree down to its roots.
    pub fn collapse_all_bones(&mut self, cx: &mut Context<Self>) {
        self.expanded_bones.clear();
        cx.notify();
    }

    /// Start or stop clip playback. While playing, the playhead advances at
    /// real time and loops back to the start at the end of the clip.
    pub fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if self.mode != EditorMode::Sequence {
            return;
        }
        self.playback.playing = !self.playback.playing;
        if self.playback.playing {
            // Pressing play on the final frame of a one-shot restarts it.
            let (start, end) = self.play_range_seconds();
            if self.playback.time >= end - 1.0 / self.animation.fps_clamped() * 0.5 {
                self.playback.time = start;
            }
            if self.debug.show_tracers {
                self.tracer_state.trails.clear();
            }
            let weak = cx.weak_entity();
            cx.spawn(async move |_, cx| {
                const FRAME: std::time::Duration = std::time::Duration::from_millis(16);
                loop {
                    smol::Timer::after(FRAME).await;
                    let still_playing = weak.update(cx, |this, cx| {
                        if !this.playback.playing {
                            return false;
                        }
                        let (start, end) = this.play_range_seconds();
                        let span = (end - start).max(0.001);
                        this.playback.time += FRAME.as_secs_f32() * this.playback.speed;
                        let wrapped = this.playback.time >= end;
                        if wrapped {
                            if this.playback.looping {
                                this.playback.time = start + (this.playback.time - start) % span;
                            } else {
                                // One-shot: park on the last frame and stop.
                                this.playback.time =
                                    (end - 1.0 / this.animation.fps_clamped()).max(start);
                                this.playback.playing = false;
                            }
                        }
                        if this.playback.time < start {
                            this.playback.time = start;
                        }
                        if this.debug.show_tracers {
                            let world = evaluate_world_transforms(
                                &this.skeleton,
                                &this.animation,
                                this.playback.time,
                            );
                            for bone in &this.skeleton.bones {
                                if let Some(m) = world.get(&bone.id) {
                                    let pos = m.transform_point(Vec3::ZERO);
                                    this.tracer_state
                                        .trails
                                        .entry(bone.id.clone())
                                        .or_default()
                                        .push(pos);
                                }
                            }
                            // Sliding window: keep at most one animation loop worth of frames
                            let max_len = (span / FRAME.as_secs_f32()).ceil() as usize;
                            if max_len > 0 {
                                for trail in this.tracer_state.trails.values_mut() {
                                    while trail.len() > max_len {
                                        trail.remove(0);
                                    }
                                }
                            }
                        }
                        cx.notify();
                        this.playback.playing
                    });
                    match still_playing {
                        Ok(true) => {}
                        _ => break,
                    }
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// The transform currently shown in the properties panel: the selected
    /// keyframe's transform, the selected bone's bind pose, or identity.
    pub fn current_transform(&self) -> Transform {
        if let Some(socket) = self
            .selected_socket
            .as_deref()
            .and_then(|id| self.sockets.get(id))
        {
            return socket.local_transform.clone();
        }
        if let Some((bone_id, index)) = &self.selected_keyframe {
            if let Some(kf) = self
                .animation
                .track(bone_id)
                .and_then(|t| t.keyframes.get(*index))
            {
                return kf.transform.clone();
            }
        }
        if let Some(bone_id) = &self.selected_bone {
            if let Some(bone) = self.skeleton.bone(bone_id) {
                return bone.bind_transform.clone();
            }
        }
        Transform::IDENTITY
    }

    pub(crate) fn sync_transform_inputs(&self, window: &mut Window, cx: &mut App) {
        let transform = self.current_transform();
        self.transform_inputs
            .set_from_transform(&transform, window, cx);
    }

    /// Write the properties panel's nine inputs back into the selected
    /// keyframe's transform (or the selected bone's bind pose if no keyframe
    /// is selected).
    pub fn apply_transform_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.transform_editable() {
            return;
        }
        let Some(changed) = self.transform_inputs.take_changed(cx) else {
            return;
        };
        let Some(transform) =
            self.transform_inputs
                .apply_changes(self.current_transform(), changed, cx)
        else {
            return;
        };
        if self.mode == EditorMode::Skeleton {
            self.apply_hierarchy_command(
                super::hierarchy_edit::HierarchyCommand::Transform(transform),
                false,
                window,
                cx,
            );
            return;
        }

        if let Some((bone_id, index)) = self.selected_keyframe.clone() {
            if let Some(track) = self.animation.track_mut(&bone_id) {
                if let Some(kf) = track.keyframes.get_mut(index) {
                    kf.transform = transform;
                    self.is_dirty = true;
                }
            }
        } else if let Some(bone_id) = self.selected_bone.clone() {
            if let Some(bone) = self.skeleton.bone_mut(&bone_id) {
                bone.bind_transform = transform;
                self.is_dirty = true;
            }
        }

        cx.notify();
    }

    /// Return one vector group (translation, rotation or scale) of the
    /// selection to its default.
    pub fn reset_transform_group(
        &mut self,
        group: TransformGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.transform_editable() {
            return;
        }
        let mut transform = self.current_transform();
        let value = group.default_value();
        let default = Vec3::new(value, value, value);
        match group {
            TransformGroup::Translation => transform.translation = default,
            TransformGroup::Rotation => transform.rotation = default,
            TransformGroup::Scale => transform.scale = default,
        }
        plugin_editor_api::undo::lock(&self.hierarchy_history).seal();
        if self.mode == EditorMode::Skeleton {
            self.apply_hierarchy_command(
                super::hierarchy_edit::HierarchyCommand::Transform(transform),
                true,
                window,
                cx,
            );
        } else if let Some((bone, index)) = &self.selected_keyframe {
            if let Some(key) = self
                .animation
                .track_mut(bone)
                .and_then(|t| t.keyframes.get_mut(*index))
            {
                key.transform = transform;
                self.is_dirty = true;
            }
        }
        plugin_editor_api::undo::lock(&self.hierarchy_history).seal();
        self.sync_transform_inputs(window, cx);
        cx.notify();
    }

    /// World-space position of `bone_id`'s joint at the playhead.
    pub fn world_position(&self, bone_id: &str) -> Option<Vec3> {
        core::bone_world_transform(&self.skeleton, &self.animation, bone_id, self.playback.time)
            .map(|m| m.transform_point(Vec3::ZERO))
    }

    /// Save, logging a failure instead of returning it (button / shortcut handlers).
    pub fn save_and_report(&mut self, cx: &mut Context<Self>) {
        if self.mode == EditorMode::Skeleton {
            self.save_hierarchy_async(cx);
            return;
        }
        if let Err(err) = self.save() {
            self.notice = Some(format!("Could not save: {err}"));
            log::error!("Failed to save skeletal animation asset: {err}");
        } else {
            self.notice = None;
        }
        cx.notify();
    }

    /// Persist the asset to `current_asset_path`, if set: the bones for a
    /// skeleton, the clip and its skeleton id for a sequence. A sequence whose
    /// skeleton can't be found keeps its reference as it was.
    pub fn save(&mut self) -> anyhow::Result<()> {
        let Some(path) = self.current_asset_path.clone() else {
            return Ok(());
        };
        match self.mode {
            EditorMode::Skeleton => {
                self.check_saved_hierarchy_references()
                    .map_err(anyhow::Error::msg)?;
                self.save_hierarchy_validated()?;
                return Ok(());
            }
            EditorMode::Sequence => {
                let mut asset =
                    AnimSeqAsset::new(self.skeleton_ref.clone(), self.animation.clone())
                        .with_preview_mesh(self.preview_mesh_ref.clone());
                asset.id = self
                    .animation_id
                    .get_or_insert_with(AnimSequenceId::new)
                    .clone();
                assets::save_animseq_asset(&path, &asset)?;
                remember_asset_id(&library::project_root(&path), &asset.id.to_string(), &path);
            }
            EditorMode::Mesh => {
                if let Some(mesh) = self.mesh.as_deref() {
                    let mut mesh = mesh.clone();
                    mesh.skeleton = self.skeleton_ref.clone();
                    skmesh::save_skmesh_asset(&path, &mesh)?;
                }
            }
        }
        self.is_dirty = false;
        Ok(())
    }

    pub(crate) fn save_hierarchy_validated(&mut self) -> anyhow::Result<()> {
        let Some(path) = &self.current_asset_path else {
            return Ok(());
        };
        let id = self.asset_id.clone().unwrap_or_default();
        let asset = SkeletonAsset {
            version: assets::FORMAT_VERSION,
            id: id.clone(),
            bones: self.skeleton.bones.clone(),
            sockets: self.sockets.clone(),
            preview_mesh: self.preview_mesh_ref.clone(),
        };
        assets::save_skeleton_asset(path, &asset)?;
        self.asset_id = Some(id);
        plugin_editor_api::undo::lock(&self.hierarchy_history).mark_saved();
        self.saved_hierarchy = self.skeleton.clone();
        self.saved_asset_id = self.asset_id.clone();
        self.saved_sockets = self.sockets.clone();
        self.saved_preview_ref = self.preview_mesh_ref.clone();
        self.is_dirty = false;
        Ok(())
    }

    /// Whether the data on screen may be changed. A sequence is read-only
    /// until its skeleton resolves, because there is no pose to edit against.
    pub fn editable(&self) -> bool {
        match self.mode {
            EditorMode::Skeleton => true,
            EditorMode::Sequence => matches!(self.link, SkeletonLink::Resolved(_)),
            // Nothing on the timeline or in the pose; the mesh has its own edits.
            EditorMode::Mesh => false,
        }
    }

    /// Whether the properties panel's transform inputs write anywhere. A
    /// skeleton edits bind poses; a sequence only edits its keyframes, since
    /// the bind pose belongs to the skeleton asset.
    pub fn transform_editable(&self) -> bool {
        if let Some(id) = &self.selected_socket {
            return self.mode == EditorMode::Skeleton && self.sockets.get(id).is_some();
        }
        self.editable()
            && match self.mode {
                EditorMode::Skeleton => self
                    .selected_bone
                    .as_deref()
                    .is_some_and(|id| self.skeleton.bone(id).is_some()),
                EditorMode::Sequence => self.selected_keyframe.is_some(),
                EditorMode::Mesh => false,
            }
    }

    /// Mark `bone_id` and all of its ancestors as expanded in the hierarchy
    /// panel so a programmatic selection (e.g. from the viewport) is visible.
    pub(crate) fn expand_ancestors(&mut self, bone_id: &str) {
        let mut current = self.skeleton.bone(bone_id).and_then(|b| b.parent.clone());
        let mut visited = HashSet::new();
        while let Some(id) = current {
            if !visited.insert(id.clone()) {
                break;
            }
            self.expanded_bones.insert(id.clone());
            current = self.skeleton.bone(&id).and_then(|b| b.parent.clone());
        }
    }
}

fn remember_asset_id(root: &Path, id: &str, path: &Path) {
    let kind = match path.extension().and_then(|extension| extension.to_str()) {
        Some("skeleton") => "skeleton",
        Some("animseq") => "animseq",
        Some("skmesh") => "skmesh",
        _ => return,
    };
    if let Ok(Some(index)) = engine_fs::open_project_file_index(root) {
        if let Err(error) = index
            .refresh_path(path)
            .and_then(|_| index.record_typed_asset_id(kind, id, path))
        {
            log::debug!("could not cache asset id for {}: {error:#}", path.display());
        }
    }
}

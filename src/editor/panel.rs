//! `SkeletalAnimEditorPanel` — the top-level editor entity.
//!
//! Owns the skeleton/animation data model, selection state, playback state,
//! and the nine transform `NumberInput`s shown in the properties panel. The
//! dock workspace (center viewport, bone hierarchy, properties, timeline) is
//! built lazily on first render in [`super::workspace`].

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::*;
use ui::input::{InputEvent, NumberInputEvent, StepAction};

use crate::core::{self, AnimationClip, BoneTrack, Keyframe, Skeleton, Transform};
use crate::rendering::{TimelinePanel, ViewportPanel};

use super::transform_inputs::TransformInputs;

/// Maximum time difference, in seconds, for a keyframe to be considered "at"
/// the playhead (and thus the properties panel's "Key" indicators to show as
/// keyed / editable).
const KEYFRAME_EPS: f32 = 1e-4;

/// Playback transport state for the timeline.
pub struct Playback {
    /// Current playhead position, in seconds.
    pub time: f32,
    pub playing: bool,
}

pub struct SkeletalAnimEditorPanel {
    pub(crate) focus_handle: FocusHandle,
    pub(crate) workspace: Option<Entity<ui::workspace::Workspace>>,

    /// Folder containing `skeleton.json` / `animation.json`, if opened from disk.
    pub current_asset_path: Option<PathBuf>,
    pub is_dirty: bool,

    pub skeleton: Skeleton,
    pub animation: AnimationClip,

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

    subscriptions: Vec<Subscription>,
}

impl SkeletalAnimEditorPanel {
    /// Create a new editor populated with the built-in sample skeleton and
    /// running animation.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let skeleton = core::sample::humanoid_skeleton();
        let animation = core::sample::run_animation(&skeleton);
        Self::new_internal(skeleton, animation, None, window, cx)
    }

    /// Create an editor for an on-disk asset folder containing
    /// `skeleton.json` and `animation.json`.
    pub fn new_with_path(
        file_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Self> {
        let skeleton = core::serialization::load_skeleton(&file_path)?;
        let animation = core::serialization::load_animation(&file_path)?;
        Ok(Self::new_internal(skeleton, animation, Some(file_path), window, cx))
    }

    fn new_internal(
        skeleton: Skeleton,
        animation: AnimationClip,
        asset_path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let transform_inputs = TransformInputs::new(window, cx);
        let mut subscriptions = Vec::new();
        for input in transform_inputs.all() {
            subscriptions.push(cx.subscribe_in(input, window, |this, _input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.apply_transform_inputs(window, cx);
                }
            }));
            subscriptions.push(cx.subscribe_in(input, window, |this, input, event: &NumberInputEvent, window, cx| {
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
            }));
        }

        let root_id = skeleton.root_bones().first().map(|b| b.id.clone());
        let default_play_range = (0, (animation.duration * animation.fps).round().max(0.0) as i32);

        let mut panel = Self {
            focus_handle: cx.focus_handle(),
            workspace: None,
            current_asset_path: asset_path,
            is_dirty: false,
            skeleton,
            animation,
            selected_bone: None,
            expanded_bones: HashSet::new(),
            selected_keyframe: None,
            selected_keyframes: HashSet::new(),
            playback: Playback { time: 0.0, playing: false },
            play_range: default_play_range,
            viewport_panel: None,
            timeline_panel: None,
            transform_inputs,
            subscriptions,
        };

        if let Some(root_id) = root_id {
            panel.expanded_bones.insert(root_id);
        }

        panel
    }

    /// Select a bone (or clear selection), refreshing the properties panel.
    /// The keyframe selection is resynced to whatever (if anything) the new
    /// bone has keyed at the current playhead time.
    pub fn select_bone(&mut self, bone_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = &bone_id {
            self.expand_ancestors(id);
        }
        self.selected_bone = bone_id;
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
        self.animation
            .track(bone_id)
            .and_then(|t| t.keyframes.iter().position(|k| (k.time - self.playback.time).abs() < KEYFRAME_EPS))
    }

    /// Recompute `selected_keyframe`/`selected_keyframes` from the selected
    /// bone and current playhead time: if the bone has a keyframe at the
    /// playhead, it becomes the (sole) selection; otherwise the keyframe
    /// selection is cleared. Does not touch `selected_bone` or notify.
    fn resync_selected_keyframe(&mut self) {
        let key = self
            .selected_bone
            .as_ref()
            .and_then(|bone_id| self.keyframe_at_playhead(bone_id).map(|idx| (bone_id.clone(), idx)));
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
        let index = match track.keyframes.iter().position(|k| (k.time - time).abs() < KEYFRAME_EPS) {
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
                track.keyframes.insert(pos, Keyframe { time, transform });
                pos
            }
        };

        self.is_dirty = true;
        self.select_keyframe(Some((bone_id, index)), window, cx);
    }

    /// Remove every keyframe in the current multi-selection from its track.
    /// No-op if nothing is selected.
    pub fn delete_selected_keyframes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn play_range_seconds(&self) -> (f32, f32) {
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

    /// Start or stop clip playback. While playing, the playhead advances at
    /// real time and loops back to the start at the end of the clip.
    pub fn toggle_play(&mut self, cx: &mut Context<Self>) {
        self.playback.playing = !self.playback.playing;
        if self.playback.playing {
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
                        this.playback.time += FRAME.as_secs_f32();
                        if this.playback.time >= end {
                            this.playback.time = start + (this.playback.time - start) % span;
                        }
                        cx.notify();
                        true
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

    fn sync_transform_inputs(&self, window: &mut Window, cx: &mut App) {
        let transform = self.current_transform();
        self.transform_inputs.set_from_transform(&transform, window, cx);
    }

    /// Write the properties panel's nine inputs back into the selected
    /// keyframe's transform (or the selected bone's bind pose if no keyframe
    /// is selected).
    pub fn apply_transform_inputs(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let transform = self.transform_inputs.to_transform(cx);

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

    /// Persist the skeleton and animation to `current_asset_path`, if set.
    pub fn save(&mut self) -> anyhow::Result<()> {
        let Some(path) = self.current_asset_path.clone() else {
            return Ok(());
        };
        core::serialization::save_skeleton(&path, &self.skeleton)?;
        core::serialization::save_animation(&path, &self.animation)?;
        self.is_dirty = false;
        Ok(())
    }

    /// Mark `bone_id` and all of its ancestors as expanded in the hierarchy
    /// panel so a programmatic selection (e.g. from the viewport) is visible.
    fn expand_ancestors(&mut self, bone_id: &str) {
        let mut current = self.skeleton.bone(bone_id).and_then(|b| b.parent.clone());
        while let Some(id) = current {
            self.expanded_bones.insert(id.clone());
            current = self.skeleton.bone(&id).and_then(|b| b.parent.clone());
        }
    }
}

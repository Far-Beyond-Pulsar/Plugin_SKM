//! `SkeletalAnimEditorPanel` — the top-level editor entity.
//!
//! Owns the skeleton/animation data model, selection state, playback state,
//! and the nine transform `NumberInput`s shown in the properties panel. The
//! dock workspace (center viewport, bone hierarchy, properties, timeline) is
//! built lazily on first render in [`super::workspace`].

use std::collections::HashSet;
use std::path::PathBuf;

use gpui::*;
use ui::input::{InputEvent, NumberInputEvent, StepAction};

use crate::core::{self, AnimationClip, Skeleton, Transform};
use crate::rendering::{TimelinePanel, ViewportPanel};

use super::transform_inputs::TransformInputs;

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
    /// idle animation.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let skeleton = core::sample::humanoid_skeleton();
        let animation = core::sample::idle_animation(&skeleton);
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
    pub fn select_bone(&mut self, bone_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_keyframe = None;
        self.selected_keyframes.clear();
        if let Some(id) = &bone_id {
            self.expand_ancestors(id);
        }
        self.selected_bone = bone_id;
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

    /// Move the playhead to `time`, clamped to the clip's duration.
    pub fn seek(&mut self, time: f32, cx: &mut Context<Self>) {
        self.playback.time = time.clamp(0.0, self.animation.duration.max(0.0));
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
                        let duration = this.animation.duration.max(0.001);
                        this.playback.time += FRAME.as_secs_f32();
                        if this.playback.time >= duration {
                            this.playback.time %= duration;
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

//! Montage runtime: compiled montages and the per-component set of running
//! instances.
//!
//! A Montage overlays its slot track poses onto whatever the graph produces at
//! the matching Slot node; it never replaces the graph. Instances blend in and
//! out, obey the montage's interruption policy within its group, follow section
//! links, and report one deterministic result when they end.
//!
//! Precedence where several instances share a slot: lower priority applies
//! first, so the highest priority ends up on top (ties: the older instance
//! first). Bone masks limit what a track drives; curves a montage supplies
//! override the graph's, scaled by blend weight; root motion follows the
//! montage's `root_motion` setting.

use std::sync::Arc;

use crate::montage::{AnimationMontageAsset, BlendCurve, BlendSettings, InterruptPolicy, MontageRootMotion};

use super::clip::CompiledClip;
use super::pose::{Pose, RootMotion, RuntimeSkeleton};

#[derive(Clone, Debug)]
pub struct CompiledSegment {
    pub id: String,
    pub start: f32,
    pub clip: Arc<CompiledClip>,
    pub clip_start: f32,
    pub clip_end: f32,
    pub rate: f32,
    pub loop_count: u32,
    /// Length on the montage timeline.
    pub length: f32,
}

#[derive(Clone, Debug)]
pub struct CompiledTrack {
    pub id: String,
    pub slot: String,
    pub group: String,
    pub segments: Vec<CompiledSegment>,
    /// Per-bone weights, `None` for a full-body track.
    pub mask: Option<Vec<f32>>,
}

#[derive(Clone, Debug)]
pub struct CompiledSection {
    pub id: String,
    pub name: String,
    pub start: f32,
    /// End on the timeline: the next section start in time order, else the montage end.
    pub end: f32,
    pub next: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct CompiledMontage {
    pub id: String,
    pub name: String,
    pub tracks: Vec<CompiledTrack>,
    pub sections: Vec<CompiledSection>,
    pub length: f32,
    pub blend_in: BlendSettings,
    pub blend_out: BlendSettings,
    pub blend_out_time: Option<f32>,
    pub group: String,
    pub interrupt: InterruptPolicy,
    pub priority: i32,
    pub root_motion: MontageRootMotion,
    /// Notifies and notify states the Montage itself owns, on the Montage timeline.
    pub notifies: Vec<crate::notifies::AnimationNotify>,
    pub notify_states: Vec<crate::notify_schema::AnimationNotifyState>,
    pub notify_schemas: Vec<crate::notify_schema::NotifyPayloadSchema>,
}

impl CompiledMontage {
    /// `clip_of` resolves each segment's sequence; a segment without a clip is
    /// an error (the montage cannot be played as authored).
    pub fn compile(
        asset: &AnimationMontageAsset,
        skel: &RuntimeSkeleton,
        clip_of: &mut dyn FnMut(&str) -> Option<Arc<CompiledClip>>,
    ) -> Result<Self, String> {
        let mut tracks = Vec::new();
        for t in &asset.tracks {
            let mut segments = Vec::new();
            for s in &t.segments {
                let seq = s.sequence.as_ref().ok_or_else(|| format!("segment {} has no sequence", s.id))?;
                let clip = clip_of(seq.as_str()).ok_or_else(|| format!("segment {} uses sequence {} which could not be loaded", s.id, seq.as_str()))?;
                let clip_end = s.clip_end.unwrap_or(clip.duration).min(clip.duration);
                let rate = if s.play_rate.abs() > 1e-6 { s.play_rate.abs() } else { 1.0 };
                let length = s.timeline_length(clip.duration).max(0.0);
                segments.push(CompiledSegment { id: s.id.clone(), start: s.start, clip, clip_start: s.clip_start.max(0.0), clip_end, rate, loop_count: s.loop_count.max(1), length });
            }
            segments.sort_by(|a, b| a.start.total_cmp(&b.start));
            tracks.push(CompiledTrack {
                id: t.id.clone(),
                slot: t.slot.clone(),
                group: t.group.clone(),
                segments,
                mask: match t.mask.as_ref() {
                    Some(m) => Some(m.weights_in(&skeleton_for_mask(skel), &skel.mask_library).map_err(|e| format!("slot `{}`: {e}", t.slot))?),
                    None => None,
                },
            });
        }
        let length = tracks.iter().flat_map(|t| &t.segments).map(|s| s.start + s.length).fold(0.0, f32::max);
        let mut order: Vec<usize> = (0..asset.sections.len()).collect();
        order.sort_by(|&a, &b| asset.sections[a].start.total_cmp(&asset.sections[b].start).then(a.cmp(&b)));
        let mut sections: Vec<CompiledSection> = asset
            .sections
            .iter()
            .map(|s| CompiledSection { id: s.id.clone(), name: s.name.clone(), start: s.start, end: length, next: None })
            .collect();
        for (n, &i) in order.iter().enumerate() {
            if let Some(&after) = order.get(n + 1) {
                sections[i].end = asset.sections[after].start;
            }
        }
        for (i, s) in asset.sections.iter().enumerate() {
            sections[i].next = s.next.as_ref().and_then(|n| asset.sections.iter().position(|o| &o.id == n));
        }
        Ok(Self {
            id: asset.id.as_str().to_string(),
            name: asset.name.clone(),
            tracks,
            sections,
            length,
            blend_in: asset.blend_in,
            blend_out: asset.blend_out,
            blend_out_time: asset.blend_out_time,
            group: asset.group.clone(),
            interrupt: asset.interrupt,
            priority: asset.priority,
            root_motion: asset.root_motion,
            notifies: asset.notifies.clone(),
            notify_states: asset.notify_states.clone(),
            notify_schemas: asset.notify_schemas.clone(),
        })
    }

    pub fn section_index(&self, name_or_id: &str) -> Option<usize> {
        self.sections.iter().position(|s| s.name == name_or_id || s.id == name_or_id)
    }

    pub fn has_slot(&self, slot: &str, group: &str) -> bool {
        self.tracks.iter().any(|t| t.slot == slot && t.group == group)
    }
}

/// `BoneMaskSpec::weights` wants a `Skeleton`; rebuild the minimal one from ids.
fn skeleton_for_mask(skel: &RuntimeSkeleton) -> crate::skeleton::Skeleton {
    crate::skeleton::Skeleton {
        bones: (0..skel.len())
            .map(|i| crate::skeleton::Bone {
                id: skel.ids[i].clone(),
                name: skel.names[i].clone(),
                parent: skel.parents[i].map(|p| skel.ids[p].clone()),
                bind_transform: crate::math::Transform::IDENTITY,
            })
            .collect(),
    }
}

/// Stable identity of one playback of a montage on one component.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MontageHandle(pub u64);

/// How an instance ended; delivered exactly once per instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MontageResult {
    /// Played to the end of its last section and blended out.
    Completed,
    /// Stopped by another montage under the group's interruption policy.
    Interrupted { by: MontageHandle },
    /// Stopped by a stop command.
    Stopped,
    /// Removed without blending (teardown, reload that dropped the montage).
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MontageEventKind {
    Started,
    SectionChanged { from: Option<String>, to: String },
    /// The blend-in finished: the Montage is at full weight.
    BlendedIn,
    BlendingOut,
    Ended(MontageResult),
}

#[derive(Clone, Debug, PartialEq)]
pub struct MontageEvent {
    pub handle: MontageHandle,
    pub montage: String,
    pub kind: MontageEventKind,
}

/// Why a play command did not start an instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayRefused {
    /// The group is busy and the montage's policy is `RejectIfBusy`.
    GroupBusy { by: MontageHandle },
    /// `StartIfHigherPriority` and something of equal or higher priority plays.
    LowerPriority { by: MontageHandle },
    UnknownSection(String),
    InvalidRate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    BlendingIn,
    Playing,
    BlendingOut,
}

#[derive(Clone, Debug)]
pub struct MontageInstance {
    pub handle: MontageHandle,
    pub montage: Arc<CompiledMontage>,
    /// Position on the montage timeline, seconds.
    pub position: f64,
    pub rate: f32,
    pub paused: bool,
    pub priority: i32,
    pub phase: Phase,
    pub weight: f32,
    blend_elapsed: f32,
    blend_from: f32,
    blend_total: f32,
    blend_curve: BlendCurve,
    pub section: Option<usize>,
    /// `(section, next)` overrides set at runtime.
    overrides: Vec<(usize, usize)>,
    /// Timeline interval advanced by the latest update: first run start to last run end.
    pub last_span: (f64, f64),
    /// Continuous stretches of the playhead in the latest update. A section link
    /// (or a jump) starts a new run, so root motion and notifies never read the
    /// skipped time. A paused instance reports one empty run (it is still live).
    pub runs: Vec<(f64, f64)>,
    started_serial: u64,
    pending_result: Option<MontageResult>,
}

impl MontageInstance {
    pub fn section_name(&self) -> Option<&str> {
        self.section.map(|s| self.montage.sections[s].name.as_str())
    }

    fn next_of(&self, section: usize) -> Option<usize> {
        self.overrides.iter().rev().find(|(s, _)| *s == section).map(|(_, n)| *n).or(self.montage.sections[section].next)
    }

    /// The section reverse playback continues into from the start of `section`:
    /// the section itself if it links to itself (a loop plays back as a loop),
    /// else the first section (in asset order) whose link leads here.
    fn previous_of(&self, section: usize) -> Option<usize> {
        if self.next_of(section) == Some(section) {
            return Some(section);
        }
        (0..self.montage.sections.len()).find(|&s| s != section && self.next_of(s) == Some(section))
    }

    fn begin_blend_out(&mut self, settings: BlendSettings, scale: f32) {
        if self.phase == Phase::BlendingOut {
            return;
        }
        self.phase = Phase::BlendingOut;
        self.blend_from = self.weight;
        self.blend_elapsed = 0.0;
        self.blend_total = (settings.duration * scale).max(0.0);
        self.blend_curve = settings.curve;
    }
}

/// Everything running on one component.
#[derive(Clone, Debug, Default)]
pub struct MontageSet {
    /// `NoRootMotionExtraction`: root bone motion stays in the pose instead of being
    /// extracted (set by the owner of the set).
    pub root_motion_in_pose: bool,
    instances: Vec<MontageInstance>,
    next_handle: u64,
    serial: u64,
    events: Vec<MontageEvent>,
    finished: Vec<MontageInstance>,
}

impl MontageSet {
    pub fn instances(&self) -> &[MontageInstance] {
        &self.instances
    }

    /// Instances that ended in the latest update, as they were when they ended (their
    /// final stretch of playback still counts for notifies).
    pub fn finished(&self) -> &[MontageInstance] {
        &self.finished
    }

    pub fn get(&self, handle: MontageHandle) -> Option<&MontageInstance> {
        self.instances.iter().find(|i| i.handle == handle)
    }

    fn get_mut(&mut self, handle: MontageHandle) -> Option<&mut MontageInstance> {
        self.instances.iter_mut().find(|i| i.handle == handle)
    }

    pub fn is_playing(&self, montage_id: &str) -> bool {
        self.instances.iter().any(|i| i.montage.id == montage_id)
    }

    /// Events since the last call, in the order they happened.
    pub fn drain_events(&mut self) -> Vec<MontageEvent> {
        std::mem::take(&mut self.events)
    }

    /// Start a montage. `start_section` begins at that section. The group's
    /// interruption policy decides what happens to running instances.
    pub fn play(&mut self, montage: Arc<CompiledMontage>, rate: f32, start_section: Option<&str>) -> Result<MontageHandle, PlayRefused> {
        if !rate.is_finite() || rate == 0.0 {
            return Err(PlayRefused::InvalidRate);
        }
        let section = match start_section {
            Some(name) => Some(montage.section_index(name).ok_or_else(|| PlayRefused::UnknownSection(name.to_string()))?),
            None => (!montage.sections.is_empty()).then(|| {
                // the earliest section is where a plain play begins
                (0..montage.sections.len()).min_by(|&a, &b| montage.sections[a].start.total_cmp(&montage.sections[b].start).then(a.cmp(&b))).unwrap_or(0)
            }),
        };
        let handle = MontageHandle(self.next_handle);
        let in_group: Vec<(MontageHandle, i32)> =
            self.instances.iter().filter(|i| i.montage.group == montage.group).map(|i| (i.handle, i.priority)).collect();
        match montage.interrupt {
            InterruptPolicy::RejectIfBusy => {
                if let Some(&(by, _)) = in_group.first() {
                    return Err(PlayRefused::GroupBusy { by });
                }
            }
            InterruptPolicy::StartIfHigherPriority => {
                if let Some(&(by, _)) = in_group.iter().find(|(_, p)| *p >= montage.priority) {
                    return Err(PlayRefused::LowerPriority { by });
                }
                self.interrupt_all(&in_group, handle);
            }
            InterruptPolicy::ReplaceGroup => self.interrupt_all(&in_group, handle),
            InterruptPolicy::Overlap => {}
        }
        self.next_handle += 1;
        self.serial += 1;
        let start = section.map_or(0.0, |s| montage.sections[s].start as f64);
        let blend_in = montage.blend_in;
        let instance = MontageInstance {
            handle,
            position: start,
            rate,
            paused: false,
            priority: montage.priority,
            phase: if blend_in.duration > 0.0 { Phase::BlendingIn } else { Phase::Playing },
            weight: if blend_in.duration > 0.0 { 0.0 } else { 1.0 },
            blend_elapsed: 0.0,
            blend_from: 0.0,
            blend_total: blend_in.duration,
            blend_curve: blend_in.curve,
            section,
            overrides: Vec::new(),
            last_span: (start, start),
            runs: vec![(start, start)],
            started_serial: self.serial,
            pending_result: None,
            montage: montage.clone(),
        };
        self.events.push(MontageEvent { handle, montage: montage.id.clone(), kind: MontageEventKind::Started });
        if blend_in.duration <= 0.0 {
            self.events.push(MontageEvent { handle, montage: montage.id.clone(), kind: MontageEventKind::BlendedIn });
        }
        if let Some(s) = section {
            self.events.push(MontageEvent {
                handle,
                montage: montage.id.clone(),
                kind: MontageEventKind::SectionChanged { from: None, to: montage.sections[s].name.clone() },
            });
        }
        self.instances.push(instance);
        Ok(handle)
    }

    fn interrupt_all(&mut self, victims: &[(MontageHandle, i32)], by: MontageHandle) {
        for &(h, _) in victims {
            self.stop_with(h, None, MontageResult::Interrupted { by });
        }
    }

    fn stop_with(&mut self, handle: MontageHandle, blend: Option<f32>, result: MontageResult) -> bool {
        let Some(i) = self.instances.iter().position(|i| i.handle == handle) else { return false };
        let inst = &mut self.instances[i];
        let settings = BlendSettings { duration: blend.unwrap_or(inst.montage.blend_out.duration), curve: inst.montage.blend_out.curve };
        if inst.phase == Phase::BlendingOut {
            // already ending: a harder stop may shorten it but never lengthen it
            if let Some(d) = blend {
                let remaining = (inst.blend_total - inst.blend_elapsed).max(0.0);
                if d < remaining {
                    inst.blend_total = inst.blend_elapsed + d;
                }
            }
            if inst.pending_result.is_none() {
                inst.pending_result = Some(result);
            }
            return true;
        }
        inst.begin_blend_out(settings, 1.0);
        inst.pending_result = Some(result);
        self.events.push(MontageEvent { handle, montage: inst.montage.id.clone(), kind: MontageEventKind::BlendingOut });
        true
    }

    /// Stop one instance, blending out over `blend_out` (the montage's own
    /// duration when `None`). Returns whether the instance existed.
    pub fn stop(&mut self, handle: MontageHandle, blend_out: Option<f32>) -> bool {
        self.stop_with(handle, blend_out, MontageResult::Stopped)
    }

    /// Stop everything in a group.
    pub fn stop_group(&mut self, group: &str, blend_out: Option<f32>) -> usize {
        let hs: Vec<_> = self.instances.iter().filter(|i| i.montage.group == group).map(|i| i.handle).collect();
        hs.iter().filter(|&&h| self.stop_with(h, blend_out, MontageResult::Stopped)).count()
    }

    /// Stop everything.
    pub fn stop_all(&mut self, blend_out: Option<f32>) -> usize {
        let hs: Vec<_> = self.instances.iter().map(|i| i.handle).collect();
        hs.iter().filter(|&&h| self.stop_with(h, blend_out, MontageResult::Stopped)).count()
    }

    /// Remove every instance without blending. Each reports `Cancelled`.
    pub fn cancel_all(&mut self) {
        for inst in std::mem::take(&mut self.instances) {
            self.events.push(MontageEvent { handle: inst.handle, montage: inst.montage.id.clone(), kind: MontageEventKind::Ended(MontageResult::Cancelled) });
        }
    }

    pub fn pause(&mut self, handle: MontageHandle, paused: bool) -> bool {
        self.get_mut(handle).map(|i| i.paused = paused).is_some()
    }

    pub fn set_rate(&mut self, handle: MontageHandle, rate: f32) -> Result<(), PlayRefused> {
        if !rate.is_finite() || rate == 0.0 {
            return Err(PlayRefused::InvalidRate);
        }
        self.get_mut(handle).map(|i| i.rate = rate).ok_or(PlayRefused::InvalidRate)
    }

    pub fn set_priority(&mut self, handle: MontageHandle, priority: i32) -> bool {
        self.get_mut(handle).map(|i| i.priority = priority).is_some()
    }

    /// Jump to the start of a section. Does not raise gameplay events for the
    /// skipped time.
    pub fn jump_to_section(&mut self, handle: MontageHandle, section: &str) -> Result<(), PlayRefused> {
        let inst = self.get_mut(handle).ok_or_else(|| PlayRefused::UnknownSection(section.to_string()))?;
        let idx = inst.montage.section_index(section).ok_or_else(|| PlayRefused::UnknownSection(section.to_string()))?;
        let from = inst.section_name().map(str::to_string);
        inst.position = inst.montage.sections[idx].start as f64;
        inst.last_span = (inst.position, inst.position);
        inst.runs = vec![(inst.position, inst.position)];
        inst.section = Some(idx);
        let (montage, to) = (inst.montage.id.clone(), inst.montage.sections[idx].name.clone());
        self.events.push(MontageEvent { handle, montage, kind: MontageEventKind::SectionChanged { from, to } });
        Ok(())
    }

    /// Change what follows `section` for this instance only.
    pub fn set_next_section(&mut self, handle: MontageHandle, section: &str, next: &str) -> Result<(), PlayRefused> {
        let inst = self.get_mut(handle).ok_or_else(|| PlayRefused::UnknownSection(section.to_string()))?;
        let a = inst.montage.section_index(section).ok_or_else(|| PlayRefused::UnknownSection(section.to_string()))?;
        let b = inst.montage.section_index(next).ok_or_else(|| PlayRefused::UnknownSection(next.to_string()))?;
        inst.overrides.push((a, b));
        Ok(())
    }

    /// Seek without raising anything: used by scrubbing.
    pub fn seek(&mut self, handle: MontageHandle, position: f64) -> bool {
        let Some(inst) = self.get_mut(handle) else { return false };
        inst.position = position.clamp(0.0, inst.montage.length as f64);
        inst.last_span = (inst.position, inst.position);
        inst.runs = vec![(inst.position, inst.position)];
        inst.section = section_at(&inst.montage, inst.position);
        true
    }

    /// Advance every instance by `dt` seconds.
    pub fn update(&mut self, dt: f32) {
        let dt = dt.max(0.0);
        self.finished.clear();
        let mut ended = Vec::new();
        for inst in &mut self.instances {
            // blend weight
            if inst.blend_total <= 0.0 {
                match inst.phase {
                    Phase::BlendingIn => {
                        inst.phase = Phase::Playing;
                        inst.weight = 1.0;
                        self.events.push(MontageEvent { handle: inst.handle, montage: inst.montage.id.clone(), kind: MontageEventKind::BlendedIn });
                    }
                    Phase::BlendingOut => inst.weight = 0.0,
                    Phase::Playing => {}
                }
            } else if inst.phase != Phase::Playing {
                inst.blend_elapsed += dt;
                let t = (inst.blend_elapsed / inst.blend_total).clamp(0.0, 1.0);
                let shaped = inst.blend_curve.evaluate(t);
                if inst.phase == Phase::BlendingIn {
                    inst.weight = shaped;
                    if t >= 1.0 {
                        inst.phase = Phase::Playing;
                        inst.weight = 1.0;
                        self.events.push(MontageEvent { handle: inst.handle, montage: inst.montage.id.clone(), kind: MontageEventKind::BlendedIn });
                    }
                } else {
                    inst.weight = inst.blend_from * (1.0 - shaped);
                }
            }
            if inst.phase == Phase::BlendingOut && (inst.weight <= 1e-6 || inst.blend_elapsed >= inst.blend_total) {
                inst.runs = vec![(inst.position, inst.position)];
                ended.push(inst.handle);
                continue;
            }
            // playback
            let from = inst.position;
            let mut runs: Vec<(f64, f64)> = Vec::new();
            let mut run_start = inst.position;
            if !inst.paused {
                let mut remaining = dt as f64 * inst.rate as f64;
                let mut guard = 0;
                while remaining.abs() > 0.0 && guard < 64 {
                    guard += 1;
                    let forward = remaining > 0.0;
                    let (lo, hi) = match inst.section {
                        Some(s) => (inst.montage.sections[s].start as f64, inst.montage.sections[s].end as f64),
                        None => (0.0, inst.montage.length as f64),
                    };
                    if forward {
                        let room = (hi - inst.position).max(0.0);
                        if remaining < room {
                            inst.position += remaining;
                            remaining = 0.0;
                        } else {
                            // reach the section end
                            remaining -= room;
                            inst.position = hi;
                            match inst.section.and_then(|s| inst.next_of(s)) {
                                Some(next) => {
                                    let from_name = inst.section_name().map(str::to_string);
                                    runs.push((run_start, hi));
                                    inst.section = Some(next);
                                    inst.position = inst.montage.sections[next].start as f64;
                                    run_start = inst.position;
                                    let (montage, to) = (inst.montage.id.clone(), inst.montage.sections[next].name.clone());
                                    self.events.push(MontageEvent { handle: inst.handle, montage, kind: MontageEventKind::SectionChanged { from: from_name, to } });
                                    if remaining <= 0.0 || inst.montage.sections[next].end <= inst.montage.sections[next].start {
                                        break;
                                    }
                                }
                                None => {
                                    // end of the montage: blend out from here
                                    remaining = 0.0;
                                    if inst.phase != Phase::BlendingOut {
                                        let settings = inst.montage.blend_out;
                                        inst.begin_blend_out(settings, 1.0);
                                        inst.pending_result = Some(MontageResult::Completed);
                                        self.events.push(MontageEvent { handle: inst.handle, montage: inst.montage.id.clone(), kind: MontageEventKind::BlendingOut });
                                    }
                                }
                            }
                        }
                    } else {
                        // reverse: play back to the section start, then follow the
                        // links backwards (a section that links to this one); with
                        // none, the Montage has been played back to its start
                        let room = (inst.position - lo).max(0.0);
                        if -remaining < room {
                            inst.position += remaining;
                            remaining = 0.0;
                        } else {
                            remaining += room;
                            inst.position = lo;
                            match inst.section.and_then(|s| inst.previous_of(s)) {
                                Some(prev) => {
                                    let from_name = inst.section_name().map(str::to_string);
                                    runs.push((run_start, lo));
                                    inst.section = Some(prev);
                                    inst.position = inst.montage.sections[prev].end as f64;
                                    run_start = inst.position;
                                    let (montage, to) = (inst.montage.id.clone(), inst.montage.sections[prev].name.clone());
                                    self.events.push(MontageEvent { handle: inst.handle, montage, kind: MontageEventKind::SectionChanged { from: from_name, to } });
                                    if remaining >= 0.0 || inst.montage.sections[prev].end <= inst.montage.sections[prev].start {
                                        break;
                                    }
                                }
                                None => {
                                    remaining = 0.0;
                                    if inst.phase != Phase::BlendingOut {
                                        let settings = inst.montage.blend_out;
                                        inst.begin_blend_out(settings, 1.0);
                                        inst.pending_result = Some(MontageResult::Completed);
                                        self.events.push(MontageEvent { handle: inst.handle, montage: inst.montage.id.clone(), kind: MontageEventKind::BlendingOut });
                                    }
                                }
                            }
                        }
                    }
                }
                // blend out early when configured
                if let (Some(lead), Phase::Playing | Phase::BlendingIn) = (inst.montage.blend_out_time, inst.phase) {
                    let at_last = inst.section.is_none_or(|s| inst.next_of(s).is_none());
                    if at_last && inst.position >= (inst.montage.length - lead.max(0.0)) as f64 && inst.rate > 0.0 {
                        let settings = inst.montage.blend_out;
                        inst.begin_blend_out(settings, 1.0);
                        inst.pending_result = Some(MontageResult::Completed);
                        self.events.push(MontageEvent { handle: inst.handle, montage: inst.montage.id.clone(), kind: MontageEventKind::BlendingOut });
                    }
                }
            }
            if inst.phase == Phase::BlendingOut && inst.blend_total <= 0.0 {
                inst.weight = 0.0;
                ended.push(inst.handle);
            }
            runs.push((run_start, inst.position));
            inst.last_span = (runs.first().map_or(from, |r| r.0), inst.position);
            inst.runs = runs;
            if inst.section.is_none() {
                inst.section = section_at(&inst.montage, inst.position);
            }
        }
        for handle in ended {
            if let Some(i) = self.instances.iter().position(|i| i.handle == handle) {
                let inst = self.instances.remove(i);
                self.finished.push(inst.clone());
                self.events.push(MontageEvent {
                    handle,
                    montage: inst.montage.id.clone(),
                    kind: MontageEventKind::Ended(inst.pending_result.unwrap_or(MontageResult::Completed)),
                });
            }
        }
    }

    /// Instances that have a track on `(slot, group)`, ordered so later entries
    /// are applied on top: ascending priority, then start order.
    pub fn for_slot(&self, slot: &str, group: &str) -> Vec<&MontageInstance> {
        let mut v: Vec<&MontageInstance> = self.instances.iter().filter(|i| i.montage.has_slot(slot, group) && i.weight > 0.0).collect();
        v.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.started_serial.cmp(&b.started_serial)));
        v
    }

    /// Whether some instance drives the whole body on this slot at full weight,
    /// so the slot's source pose cannot show.
    pub fn covers_slot(&self, slot: &str, group: &str) -> bool {
        self.for_slot(slot, group).iter().any(|i| i.weight >= 1.0 - 1e-6 && i.montage.tracks.iter().any(|t| t.slot == slot && t.group == group && t.mask.is_none() && segment_at(t, i.position).is_some()))
    }

    /// Overlay every instance on `(slot, group)` onto `base`.
    pub fn apply_to_slot(&self, skel: &RuntimeSkeleton, slot: &str, group: &str, base: Pose) -> Pose {
        let mut pose = base;
        for inst in self.for_slot(slot, group) {
            for track in inst.montage.tracks.iter().filter(|t| t.slot == slot && t.group == group) {
                let Some((seg, clip_time)) = segment_at(track, inst.position) else { continue };
                let mut sample = seg.clip.sample(skel, clip_time);
                let supplies_motion = inst.montage.root_motion == MontageRootMotion::Montage && !self.root_motion_in_pose;
                if supplies_motion {
                    seg.clip.remove_root_motion(skel, &mut sample.bones, clip_time);
                }
                let w = inst.weight.clamp(0.0, 1.0);
                for (b, bone) in pose.bones.iter_mut().enumerate() {
                    let bw = w * track.mask.as_ref().map_or(1.0, |m| m.get(b).copied().unwrap_or(0.0));
                    if bw > 0.0 {
                        *bone = bone.blend(&sample.bones[b], bw);
                    }
                }
                // curves the montage supplies override the graph's, scaled by weight
                for (name, v) in &sample.curves {
                    let e = pose.curves.entry(name.clone()).or_insert(0.0);
                    *e += (*v - *e) * w;
                }
                match inst.montage.root_motion {
                    MontageRootMotion::Montage if self.root_motion_in_pose => {}
                    MontageRootMotion::Montage => {
                        let delta = montage_root_motion(inst, track, skel);
                        pose.root_motion = pose.root_motion.blend(&delta, w);
                        pose.montage_root_motion = pose.montage_root_motion.then(&delta.scaled(w));
                    }
                    MontageRootMotion::Graph => {}
                    MontageRootMotion::Ignore => pose.root_motion = pose.root_motion.blend(&RootMotion::IDENTITY, w),
                }
            }
        }
        pose
    }
}

/// Root motion of a track over the instance's latest advance.
fn montage_root_motion(inst: &MontageInstance, track: &CompiledTrack, skel: &RuntimeSkeleton) -> RootMotion {
    // each continuous run on its own: the time skipped by a section link or a
    // jump contributes nothing
    inst.runs.iter().fold(RootMotion::IDENTITY, |acc, &(a, b)| acc.then(&run_root_motion(track, skel, a, b)))
}

fn run_root_motion(track: &CompiledTrack, skel: &RuntimeSkeleton, a: f64, b: f64) -> RootMotion {
    let mut total = RootMotion::IDENTITY;
    if a == b {
        return total;
    }
    let (lo, hi) = (a.min(b), a.max(b));
    for seg in &track.segments {
        let seg_start = seg.start as f64;
        let seg_end = seg_start + seg.length as f64;
        let (s, e) = (lo.max(seg_start), hi.min(seg_end));
        if e <= s {
            continue;
        }
        let clip_len = (seg.clip_end - seg.clip_start).max(1e-6) as f64;
        let to_clip = |t: f64| -> f64 { (t - seg_start) * seg.rate as f64 };
        // unbounded cycles inside the trimmed range: each cycle maps onto [clip_start, clip_end)
        let (ps, pe) = (to_clip(s), to_clip(e));
        let rm = |from: f64, to: f64| -> RootMotion {
            // accumulate cycle by cycle so the loop seam composes
            let mut acc = RootMotion::IDENTITY;
            let mut cursor = from;
            while cursor < to - 1e-9 {
                let cycle = (cursor / clip_len).floor();
                let cycle_end = ((cycle + 1.0) * clip_len).min(to);
                let local_from = seg.clip_start as f64 + (cursor - cycle * clip_len);
                let local_to = seg.clip_start as f64 + (cycle_end - cycle * clip_len);
                acc = acc.then(&seg.clip.root_motion_between(skel, local_from, local_to, false));
                cursor = cycle_end;
            }
            acc
        };
        let seg_rm = rm(ps, pe);
        total = total.then(&seg_rm);
    }
    if b < a { total.inverse() } else { total }
}

fn section_at(montage: &CompiledMontage, position: f64) -> Option<usize> {
    montage
        .sections
        .iter()
        .enumerate()
        .filter(|(_, s)| s.start as f64 <= position + 1e-9)
        .max_by(|a, b| a.1.start.total_cmp(&b.1.start).then(b.0.cmp(&a.0)))
        .map(|(i, _)| i)
}

/// The segment of `track` covering `position`, and the clip time inside it.
/// Time inside a looping segment wraps; after the last frame the pose holds.
fn segment_at(track: &CompiledTrack, position: f64) -> Option<(&CompiledSegment, f32)> {
    let seg = track.segments.iter().rev().find(|s| position >= s.start as f64 - 1e-9 && position <= (s.start + s.length) as f64 + 1e-9)?;
    let clip_len = (seg.clip_end - seg.clip_start).max(1e-6);
    let local = ((position - seg.start as f64) * seg.rate as f64).max(0.0);
    let t = if local >= (clip_len * seg.loop_count as f32) as f64 {
        seg.clip_end
    } else {
        seg.clip_start + (local % clip_len as f64) as f32
    };
    Some((seg, t))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::montage::{MontageSection, MontageSegment, SlotTrack};
    use crate::runtime::clip::tests::{clip_asset, translate_track};
    use crate::runtime::pose::tests::chain_skeleton;
    use glam::Vec3;
    use pulsar_reflection::AssetRef;

    pub(crate) fn skel() -> RuntimeSkeleton {
        RuntimeSkeleton::new("s", &chain_skeleton(3)).unwrap()
    }

    /// A montage with one track on `slot`: a 2 s clip moving `bone` from x=0 to x=2.
    pub(crate) fn montage(skel: &RuntimeSkeleton, name: &str, slot: &str, bone: &str, mask_from: Option<&str>) -> Arc<CompiledMontage> {
        let mut asset = AnimationMontageAsset::default();
        asset.name = name.into();
        let seq: AssetRef<pulsar_reflection::AnimationSequenceAssetKind> = AssetRef::new();
        let mut track = SlotTrack::new(slot, "DefaultGroup");
        track.segments.push(MontageSegment::new(Some(seq.clone()), 0.0));
        if let Some(b) = mask_from {
            track.mask = Some(crate::bone_mask::BoneMaskSpec { branches: vec![crate::bone_mask::MaskBranch { bone: b.into(), depth: None, weight: 1.0 }], default_weight: 0.0, ..Default::default() });
        }
        asset.tracks.push(track);
        asset.blend_in = BlendSettings { duration: 0.0, curve: BlendCurve::Linear };
        asset.blend_out = BlendSettings { duration: 0.0, curve: BlendCurve::Linear };
        let mut seqasset = clip_asset(name, 2.0, vec![translate_track(bone, [0.0, 1.0, 0.0], [2.0, 1.0, 0.0], 2.0)]);
        seqasset.features.root_motion.enabled = false;
        let clip = Arc::new(CompiledClip::compile(&seqasset, skel));
        Arc::new(CompiledMontage::compile(&asset, skel, &mut |_| Some(clip.clone())).unwrap())
    }

    fn with(m: &Arc<CompiledMontage>, f: impl FnOnce(&mut CompiledMontage)) -> Arc<CompiledMontage> {
        let mut c = (**m).clone();
        f(&mut c);
        Arc::new(c)
    }

    #[test]
    fn a_named_skeleton_mask_drives_a_slot_track_and_an_unknown_name_is_refused() {
        use crate::bone_mask::{BoneMaskSpec, MaskBranch, NamedBoneMask};
        let plain = skel();
        let inline = montage(&plain, "m", "DefaultSlot", "b2", Some("b2"));
        let library = vec![NamedBoneMask { name: "Tip".into(), spec: BoneMaskSpec { branches: vec![MaskBranch { bone: "b2".into(), depth: None, weight: 1.0 }], ..Default::default() } }];
        let with_library = skel().with_mask_library(library);
        let mut asset = AnimationMontageAsset::default();
        let mut track = SlotTrack::new("DefaultSlot", "DefaultGroup");
        track.segments.push(MontageSegment::new(Some(AssetRef::new()), 0.0));
        track.mask = Some(BoneMaskSpec::named("Tip"));
        asset.tracks.push(track);
        let clip = Arc::new(CompiledClip::compile(&clip_asset("c", 2.0, vec![translate_track("b2", [0.0, 1.0, 0.0], [2.0, 1.0, 0.0], 2.0)]), &with_library));
        let named = CompiledMontage::compile(&asset, &with_library, &mut |_| Some(clip.clone())).unwrap();
        assert_eq!(named.tracks[0].mask, inline.tracks[0].mask, "the named mask resolves to the inline weights");
        let err = CompiledMontage::compile(&asset, &plain, &mut |_| Some(clip.clone())).unwrap_err();
        assert!(err.contains("Tip"), "{err}");
    }

    fn results(set: &mut MontageSet) -> Vec<(MontageHandle, MontageResult)> {
        set.drain_events().into_iter().filter_map(|e| if let MontageEventKind::Ended(r) = e.kind { Some((e.handle, r)) } else { None }).collect()
    }

    #[test]
    fn plays_blends_and_completes_with_one_result() {
        let skel = skel();
        let m = with(&montage(&skel, "a", "DefaultSlot", "b2", None), |m| {
            m.blend_in = BlendSettings { duration: 0.5, curve: BlendCurve::Linear };
            m.blend_out = BlendSettings { duration: 0.5, curve: BlendCurve::Linear };
        });
        let mut set = MontageSet::default();
        let h = set.play(m, 1.0, None).unwrap();
        set.update(0.25);
        assert!((set.get(h).unwrap().weight - 0.5).abs() < 1e-5);
        set.update(0.25);
        assert_eq!(set.get(h).unwrap().phase, Phase::Playing);
        set.update(1.5);
        assert_eq!(set.get(h).unwrap().phase, Phase::BlendingOut);
        set.update(0.25);
        assert!((set.get(h).unwrap().weight - 0.5).abs() < 1e-5);
        set.update(0.25);
        assert!(set.get(h).is_none());
        assert_eq!(results(&mut set), [(h, MontageResult::Completed)]);
        assert!(set.drain_events().is_empty(), "delivered once");
    }

    #[test]
    fn group_policies_decide_who_plays() {
        let skel = skel();
        let base = montage(&skel, "m", "DefaultSlot", "b2", None);
        let mut set = MontageSet::default();
        let first = set.play(base.clone(), 1.0, None).unwrap();
        let second = set.play(base.clone(), 1.0, None).unwrap();
        set.update(0.0);
        assert_eq!(results(&mut set), [(first, MontageResult::Interrupted { by: second })]);
        let reject = with(&base, |m| m.interrupt = InterruptPolicy::RejectIfBusy);
        assert_eq!(set.play(reject, 1.0, None), Err(PlayRefused::GroupBusy { by: second }));
        let low = with(&base, |m| {
            m.interrupt = InterruptPolicy::StartIfHigherPriority;
            m.priority = 0;
        });
        assert!(matches!(set.play(low, 1.0, None), Err(PlayRefused::LowerPriority { .. })));
        let high = with(&base, |m| {
            m.interrupt = InterruptPolicy::StartIfHigherPriority;
            m.priority = 5;
        });
        let h = set.play(high, 1.0, None).unwrap();
        set.update(0.0);
        assert!(set.get(second).is_none() && set.get(h).is_some());
        let overlap = with(&base, |m| m.interrupt = InterruptPolicy::Overlap);
        let o = set.play(overlap, 1.0, None).unwrap();
        assert!(set.get(h).is_some() && set.get(o).is_some());
        let other = with(&base, |m| m.group = "Other".into());
        set.play(other, 1.0, None).unwrap();
        assert!(set.get(h).is_some());
        assert_eq!(set.play(base.clone(), 0.0, None), Err(PlayRefused::InvalidRate));
    }

    #[test]
    fn explicit_stop_blends_out_and_reports_stopped_pause_and_rate_work() {
        let skel = skel();
        let m = with(&montage(&skel, "a", "DefaultSlot", "b2", None), |m| m.blend_out = BlendSettings { duration: 0.4, curve: BlendCurve::Linear });
        let mut set = MontageSet::default();
        let h = set.play(m, 2.0, None).unwrap();
        set.update(0.25);
        assert!((set.get(h).unwrap().position - 0.5).abs() < 1e-6, "rate scales time");
        set.pause(h, true);
        set.update(1.0);
        assert!((set.get(h).unwrap().position - 0.5).abs() < 1e-6, "paused");
        set.pause(h, false);
        assert!(set.stop(h, None));
        set.update(0.2);
        assert!((set.get(h).unwrap().weight - 0.5).abs() < 1e-5);
        set.update(0.2);
        assert_eq!(results(&mut set), [(h, MontageResult::Stopped)]);
        assert!(!set.stop(h, None), "stopping an ended instance reports false");
    }

    fn sectioned(skel: &RuntimeSkeleton) -> Arc<CompiledMontage> {
        let mut asset = AnimationMontageAsset::default();
        let mut track = SlotTrack::new("DefaultSlot", "DefaultGroup");
        track.segments.push(MontageSegment::new(Some(AssetRef::new()), 0.0));
        asset.tracks.push(track);
        asset.blend_in.duration = 0.0;
        asset.blend_out.duration = 0.0;
        asset.sections = vec![
            MontageSection { id: "s1".into(), name: "Start".into(), start: 0.0, next: Some("s2".into()), extra: Default::default() },
            MontageSection { id: "s2".into(), name: "Loop".into(), start: 0.5, next: Some("s2".into()), extra: Default::default() },
            MontageSection { id: "s3".into(), name: "End".into(), start: 1.5, next: None, extra: Default::default() },
        ];
        let clip = Arc::new(CompiledClip::compile(&clip_asset("c", 2.0, vec![]), skel));
        Arc::new(CompiledMontage::compile(&asset, skel, &mut |_| Some(clip.clone())).unwrap())
    }

    #[test]
    fn sections_follow_links_jumps_and_next_overrides() {
        let skel = skel();
        let m = sectioned(&skel);
        let mut set = MontageSet::default();
        let h = set.play(m.clone(), 1.0, None).unwrap();
        set.update(0.6);
        assert_eq!(set.get(h).unwrap().section_name(), Some("Loop"));
        set.update(2.0);
        assert_eq!(set.get(h).unwrap().section_name(), Some("Loop"));
        let p = set.get(h).unwrap().position;
        assert!((0.5..1.5).contains(&p), "{p}");
        set.set_next_section(h, "Loop", "End").unwrap();
        set.update(1.0);
        assert_eq!(set.get(h).unwrap().section_name(), Some("End"));
        set.update(1.0);
        assert!(set.get(h).is_none(), "End has no next: the montage completes");
        assert_eq!(results(&mut set).last().map(|r| r.1), Some(MontageResult::Completed));

        let h2 = set.play(m.clone(), 1.0, Some("End")).unwrap();
        assert!((set.get(h2).unwrap().position - 1.5).abs() < 1e-6);
        set.jump_to_section(h2, "Start").unwrap();
        assert_eq!(set.get(h2).unwrap().position, 0.0);
        assert_eq!(set.jump_to_section(h2, "Nope"), Err(PlayRefused::UnknownSection("Nope".into())));
        assert_eq!(set.play(m, 1.0, Some("Nope")), Err(PlayRefused::UnknownSection("Nope".into())));
    }

    #[test]
    fn a_masked_upper_body_montage_layers_over_locomotion_in_one_pose() {
        let skel = skel();
        let base = {
            let mut p = skel.bind_pose();
            p.bones[1].translation.x = 5.0;
            p
        };
        let m = montage(&skel, "wave", "UpperBody", "b2", Some("b2"));
        let mut set = MontageSet::default();
        set.play(m, 1.0, None).unwrap();
        set.update(1.0);
        let out = set.apply_to_slot(&skel, "UpperBody", "DefaultGroup", base.clone());
        assert!((out.bones[1].translation.x - 5.0).abs() < 1e-5, "outside the mask the locomotion stays");
        assert!((out.bones[2].translation.x - 1.0).abs() < 1e-4, "inside the mask the montage plays: {}", out.bones[2].translation.x);
        let other = set.apply_to_slot(&skel, "FullBody", "DefaultGroup", base.clone());
        assert_eq!(other.bones, base.bones);
        assert!(!set.covers_slot("UpperBody", "DefaultGroup"), "masked tracks never cover the source");
    }

    #[test]
    fn overlapping_instances_apply_by_priority_and_root_motion_follows_the_montage_mode() {
        let skel = skel();
        let low = with(&montage(&skel, "low", "S", "b2", None), |m| {
            m.interrupt = InterruptPolicy::Overlap;
            m.priority = 0;
        });
        let high = with(&montage(&skel, "high", "S", "b2", None), |m| {
            m.interrupt = InterruptPolicy::Overlap;
            m.priority = 9;
        });
        let mut set = MontageSet::default();
        let hh = set.play(high, 1.0, None).unwrap();
        set.update(0.5);
        let hl = set.play(low, 1.0, None).unwrap();
        set.update(0.5);
        let order: Vec<_> = set.for_slot("S", "DefaultGroup").iter().map(|i| i.handle).collect();
        assert_eq!(order, [hl, hh], "priority, not start order, decides the top");
        let out = set.apply_to_slot(&skel, "S", "DefaultGroup", skel.bind_pose());
        assert!((out.bones[2].translation.x - 1.0).abs() < 1e-4);
        assert!(set.covers_slot("S", "DefaultGroup"));

        let mut moving = (*montage(&skel, "rm", "R", "b0", None)).clone();
        let mut asset = clip_asset("rm", 2.0, vec![translate_track("b0", [0.0; 3], [0.0, 0.0, 4.0], 2.0)]);
        asset.features.root_motion.enabled = true;
        for t in &mut moving.tracks {
            for s in &mut t.segments {
                s.clip = Arc::new(CompiledClip::compile(&asset, &skel));
            }
        }
        let mut base = skel.bind_pose();
        base.root_motion.translation = Vec3::new(1.0, 0.0, 0.0);
        for (mode, expect) in [(MontageRootMotion::Montage, Vec3::new(0.0, 0.0, 2.0)), (MontageRootMotion::Graph, Vec3::new(1.0, 0.0, 0.0)), (MontageRootMotion::Ignore, Vec3::ZERO)] {
            let mut m = moving.clone();
            m.root_motion = mode;
            let mut set = MontageSet::default();
            set.play(Arc::new(m), 1.0, None).unwrap();
            set.update(1.0);
            let out = set.apply_to_slot(&skel, "R", "DefaultGroup", base.clone());
            assert!((out.root_motion.translation - expect).length() < 1e-3, "{mode:?}: {:?}", out.root_motion.translation);
        }
    }

    #[test]
    fn seeking_raises_nothing_and_cancel_all_reports_cancelled() {
        let skel = skel();
        let m = montage(&skel, "a", "DefaultSlot", "b2", None);
        let mut set = MontageSet::default();
        let h = set.play(m, 1.0, None).unwrap();
        set.drain_events();
        assert!(set.seek(h, 1.5));
        assert!(set.drain_events().is_empty(), "seeking raises nothing");
        assert_eq!(set.get(h).unwrap().last_span, (1.5, 1.5));
        set.cancel_all();
        assert_eq!(results(&mut set), [(h, MontageResult::Cancelled)]);
        assert!(set.instances().is_empty());
    }

    #[test]
    fn blend_in_completion_is_reported_once() {
        let skel = skel();
        let m = with(&montage(&skel, "a", "DefaultSlot", "b2", None), |m| m.blend_in = BlendSettings { duration: 0.5, curve: BlendCurve::Linear });
        let mut set = MontageSet::default();
        let h = set.play(m, 1.0, None).unwrap();
        let kinds = |set: &mut MontageSet| set.drain_events().into_iter().map(|e| e.kind).collect::<Vec<_>>();
        assert_eq!(kinds(&mut set), [MontageEventKind::Started]);
        set.update(0.25);
        assert!(kinds(&mut set).is_empty());
        set.update(0.25);
        assert_eq!(kinds(&mut set), [MontageEventKind::BlendedIn]);
        set.update(0.25);
        assert!(kinds(&mut set).is_empty());
        let instant = with(&montage(&skel, "b", "DefaultSlot", "b2", None), |m| m.group = "Other".into());
        set.play(instant, 1.0, None).unwrap();
        assert_eq!(kinds(&mut set), [MontageEventKind::Started, MontageEventKind::BlendedIn], "no blend: full weight at once");
        assert!(set.get(h).is_none() || set.get(h).is_some());
    }

    #[test]
    fn section_links_start_a_new_run_and_contiguous_ones_do_not_skip_time() {
        let skel = skel();
        let mut set = MontageSet::default();
        let h = set.play(sectioned(&skel), 1.0, None).unwrap();
        set.update(0.6);
        // Start (0..0.5) links to Loop (starting at 0.5): one stretch of time, two runs
        let runs = set.get(h).unwrap().runs.clone();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0], (0.0, 0.5));
        assert!((runs[1].0 - 0.5).abs() < 1e-9 && (runs[1].1 - 0.6).abs() < 1e-6, "{runs:?}");
        set.update(2.0);
        let runs = &set.get(h).unwrap().runs;
        assert!(runs.len() > 1, "the loop link restarts the playhead: {runs:?}");
        let covered: f64 = runs.iter().map(|(a, b)| b - a).sum();
        assert!((covered - 2.0).abs() < 1e-6, "every run together plays exactly the elapsed time: {covered}");
    }

    #[test]
    fn reverse_playback_follows_section_links_backwards_and_completes_at_the_start() {
        let skel = skel();
        let m = sectioned(&skel);
        let mut set = MontageSet::default();
        let h = set.play(m.clone(), -1.0, Some("End")).unwrap();
        assert!(set.get(h).is_some());
        // End has no predecessor yet: Loop links to itself
        set.update(0.25);
        assert_eq!(results(&mut set), [(h, MontageResult::Completed)]);

        let h = set.play(m, -1.0, Some("End")).unwrap();
        set.set_next_section(h, "Loop", "End").unwrap();
        set.update(0.25);
        assert_eq!(set.get(h).unwrap().section_name(), Some("Loop"), "End is entered from Loop");
        assert!((set.get(h).unwrap().position - 1.25).abs() < 1e-6);
        set.update(1.0);
        assert_eq!(set.get(h).unwrap().section_name(), Some("Start"), "Start links to Loop");
        assert!((set.get(h).unwrap().position - 0.25).abs() < 1e-6, "{}", set.get(h).unwrap().position);
        assert_eq!(set.get(h).unwrap().runs.len(), 2);
        set.update(1.0);
        assert!(set.get(h).is_none() || set.get(h).unwrap().phase == Phase::BlendingOut);
        assert_eq!(results(&mut set).last().map(|r| r.1), Some(MontageResult::Completed));
    }

    #[test]
    fn a_jump_leaves_the_skipped_time_out_of_root_motion_and_spans() {
        let skel = skel();
        let mut moving = (*sectioned(&skel)).clone();
        let mut asset = clip_asset("rm", 2.0, vec![translate_track("b0", [0.0; 3], [0.0, 0.0, 4.0], 2.0)]);
        asset.features.root_motion.enabled = true;
        let clip = Arc::new(CompiledClip::compile(&asset, &skel));
        for t in &mut moving.tracks {
            t.slot = "R".into();
            for s in &mut t.segments {
                s.clip = clip.clone();
            }
        }
        moving.root_motion = MontageRootMotion::Montage;
        let m = Arc::new(moving);
        let mut set = MontageSet::default();
        let h = set.play(m, 1.0, Some("End")).unwrap();
        set.jump_to_section(h, "Start").unwrap();
        set.update(0.5);
        let out = set.apply_to_slot(&skel, "R", "DefaultGroup", skel.bind_pose());
        assert!((out.root_motion.translation.z - 1.0).abs() < 1e-3, "only the half second played: {:?}", out.root_motion.translation);
        assert!((out.montage_root_motion.translation.z - 1.0).abs() < 1e-3);
        // extraction off: the motion stays in the pose and nothing is extracted
        set.root_motion_in_pose = true;
        let out = set.apply_to_slot(&skel, "R", "DefaultGroup", skel.bind_pose());
        assert!(out.root_motion.is_identity() && out.montage_root_motion.is_identity());
    }
}

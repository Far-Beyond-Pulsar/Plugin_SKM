//! Montage editor model: slot tracks of Sequence segments, section markers and
//! links, blend and interrupt settings, and the notify lanes.
//!
//! Sequence durations are editor-resolved data (read from the referenced
//! `.animseq` files by the view) and are held in [`MontageModel::durations`],
//! never in the montage file.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use pulsar_reflection::{AnimationSequenceAssetKind, AssetRef};
use skeletal_assets::diagnostics::Diagnostic;
use skeletal_assets::montage::{
    AnimationMontageAsset, BlendCurve, InterruptPolicy, MontageRootMotion, MontageSection, MontageSegment, SlotTrack,
};
use skeletal_assets::notifies::{NotifyTarget, NotifyValue};

use super::document::AnimDocument;
use super::notify_edit::{lane_layout, LaneEntry, LaneMetrics, LanePlacement, NotifyKind, NotifyView};
use super::selection::AnimSelection;
use super::store::DocStore;

/// Segment edges snap to other edges within this many seconds.
pub const SNAP_SECONDS: f32 = 0.03;

/// A drawn link from one section to the section it plays next.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionLink {
    pub from: String,
    pub to: String,
    pub from_time: f32,
    pub to_time: f32,
    /// The link goes back in time (a loop).
    pub backwards: bool,
}

/// What a slot track contributes at a montage time.
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveSegment {
    pub track: String,
    pub slot: String,
    pub segment: String,
    /// Time inside the source clip.
    pub clip_time: f32,
}

pub struct MontageModel {
    pub doc: AnimDocument<AnimationMontageAsset>,
    pub selection: AnimSelection,
    /// Sequence id to clip duration, filled by the view from the project.
    pub durations: HashMap<String, f32>,
}

impl MontageModel {
    pub fn open(store: Arc<dyn DocStore>, path: &Path, default_text: &str) -> Result<Self, String> {
        let doc = AnimDocument::<AnimationMontageAsset>::open(store, path, default_text)?;
        Ok(Self { doc, selection: AnimSelection::None, durations: HashMap::new() })
    }

    pub fn asset(&self) -> &AnimationMontageAsset {
        self.doc.value()
    }

    pub fn duration_of(&self, sequence: &AssetRef<AnimationSequenceAssetKind>) -> Option<f32> {
        self.durations.get(sequence.as_str()).copied()
    }

    /// Montage length: the furthest segment end (segments with unknown clip
    /// durations count only their start).
    pub fn length(&self) -> f32 {
        self.asset().length(|s| self.duration_of(s))
    }

    /// Length used as the notify timeline limit; never zero so a new montage
    /// can still hold markers.
    pub fn timeline_limit(&self) -> f32 {
        self.length().max(1.0)
    }

    pub fn segment_span(&self, segment: &MontageSegment) -> (f32, f32) {
        let len = segment.sequence.as_ref().and_then(|s| self.duration_of(s)).map_or(0.0, |d| segment.timeline_length(d));
        (segment.start, segment.start + len)
    }

    // ---- selection ------------------------------------------------------

    pub fn select_segment(&mut self, track: &str, id: &str) -> bool {
        if self.asset().tracks.iter().any(|t| t.id == track && t.segments.iter().any(|s| s.id == id)) {
            self.selection = AnimSelection::MontageSegment { track: track.into(), id: id.into() };
            true
        } else {
            false
        }
    }
    pub fn select_slot(&mut self, track: &str) -> bool {
        if self.asset().tracks.iter().any(|t| t.id == track) {
            self.selection = AnimSelection::MontageSlot { track: track.into() };
            true
        } else {
            false
        }
    }
    pub fn select_section(&mut self, id: &str) -> bool {
        if self.asset().sections.iter().any(|s| s.id == id) {
            self.selection = AnimSelection::MontageSection { id: id.into() };
            true
        } else {
            false
        }
    }
    pub fn select_notify(&mut self, id: &str) -> bool {
        let a = self.asset();
        if a.notifies.iter().any(|n| n.id == id) {
            self.selection = AnimSelection::Notify { id: id.into() };
            true
        } else if a.notify_states.iter().any(|n| n.id == id) {
            self.selection = AnimSelection::NotifyState { id: id.into() };
            true
        } else {
            false
        }
    }

    fn track_index(&self, id: &str) -> Option<usize> {
        self.asset().tracks.iter().position(|t| t.id == id)
    }
    fn locate_segment(&self, id: &str) -> Option<(usize, usize)> {
        self.asset().tracks.iter().enumerate().find_map(|(t, tr)| tr.segments.iter().position(|s| s.id == id).map(|s| (t, s)))
    }

    // ---- slot tracks ----------------------------------------------------

    /// Add a slot track. Slot and group names are plain names, and a
    /// `(group, slot)` pair appears once.
    pub fn add_track(&mut self, slot: &str, group: &str) -> Result<String, String> {
        let (slot, group) = (slot.trim(), group.trim());
        let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ' ');
        if !plain(slot) || !plain(group) {
            return Err("slot and group names use letters, digits, spaces and underscores".into());
        }
        if self.asset().tracks.iter().any(|t| t.slot == slot && t.group == group) {
            return Err(format!("slot `{slot}` in group `{group}` already has a track"));
        }
        let track = SlotTrack::new(slot, group);
        let id = track.id.clone();
        self.doc.apply("Add slot track", |a| a.tracks.push(track));
        self.selection = AnimSelection::MontageSlot { track: id.clone() };
        Ok(id)
    }

    pub fn rename_track(&mut self, track: &str, slot: &str, group: &str) -> Result<(), String> {
        let index = self.track_index(track).ok_or("no such track")?;
        let (slot, group) = (slot.trim(), group.trim());
        let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ' ');
        if !plain(slot) || !plain(group) {
            return Err("slot and group names use letters, digits, spaces and underscores".into());
        }
        if self.asset().tracks.iter().enumerate().any(|(i, t)| i != index && t.slot == slot && t.group == group) {
            return Err(format!("slot `{slot}` in group `{group}` already has a track"));
        }
        let (slot, group) = (slot.to_string(), group.to_string());
        self.doc.edit("Rename slot", Some(&format!("slot:{track}")), |a| {
            a.tracks[index].slot = slot;
            a.tracks[index].group = group;
            (true, ())
        });
        Ok(())
    }

    /// Limit a slot track to the skeleton mask `name` (`None`: the whole pose). The name is
    /// checked against `library` so a track never points at a mask the skeleton lacks.
    pub fn set_track_mask(&mut self, track: &str, name: Option<&str>, library: &[skeletal_assets::bone_mask::NamedBoneMask]) -> Result<(), String> {
        let index = self.track_index(track).ok_or("no such track")?;
        let mask = match name {
            Some(name) => {
                if !library.iter().any(|m| m.name == name) {
                    return Err(format!("the skeleton has no mask named `{name}`"));
                }
                Some(skeletal_assets::bone_mask::BoneMaskSpec::named(name))
            }
            None => None,
        };
        if self.asset().tracks[index].mask == mask {
            return Err("unchanged".into());
        }
        self.doc.edit("Slot mask", Some(&format!("slotmask:{track}")), |a| {
            a.tracks[index].mask = mask;
            (true, ())
        });
        Ok(())
    }

    pub fn delete_track(&mut self, track: &str) -> bool {
        let Some(index) = self.track_index(track) else { return false };
        self.doc.apply("Delete slot track", |a| {
            a.tracks.remove(index);
        });
        if matches!(&self.selection, AnimSelection::MontageSlot { track: t } | AnimSelection::MontageSegment { track: t, .. } if t == track) {
            self.selection = AnimSelection::None;
        }
        true
    }

    // ---- segments -------------------------------------------------------

    /// Append a segment to a track, after the last segment on it.
    pub fn add_segment(&mut self, track: &str, sequence: Option<AssetRef<AnimationSequenceAssetKind>>) -> Result<String, String> {
        let index = self.track_index(track).ok_or("no such track")?;
        let start = self.asset().tracks[index].segments.iter().map(|s| self.segment_span(s).1).fold(0.0, f32::max);
        let segment = MontageSegment::new(sequence, start);
        let id = segment.id.clone();
        self.doc.apply("Add segment", |a| a.tracks[index].segments.push(segment));
        self.selection = AnimSelection::MontageSegment { track: track.into(), id: id.clone() };
        Ok(id)
    }

    /// Where `start` lands after snapping the segment's start or end to the
    /// time zero, the playhead-independent edges of other segments and sections.
    pub fn snap_time(&self, segment: &str, start: f32) -> f32 {
        let (Some((t, s)), true) = (self.locate_segment(segment), start.is_finite()) else { return start.max(0.0) };
        let me = &self.asset().tracks[t].segments[s];
        let (my_start, my_end) = self.segment_span(me);
        let length = my_end - my_start;
        let mut targets = vec![0.0f32];
        for (ti, track) in self.asset().tracks.iter().enumerate() {
            for (si, other) in track.segments.iter().enumerate() {
                if (ti, si) == (t, s) {
                    continue;
                }
                let (a, b) = self.segment_span(other);
                targets.extend([a, b]);
            }
        }
        targets.extend(self.asset().sections.iter().map(|s| s.start));
        let mut best = start.max(0.0);
        let mut best_distance = SNAP_SECONDS;
        for target in targets {
            // snap the start to the target, and the end to the target
            for candidate in [target, target - length] {
                let d = (candidate - start).abs();
                if candidate >= 0.0 && d < best_distance {
                    best = candidate;
                    best_distance = d;
                }
            }
        }
        best
    }

    /// Move a segment on the timeline, optionally to another track. One drag is
    /// one undo step. Returns whether anything changed.
    pub fn move_segment(&mut self, segment: &str, start: f32, to_track: Option<&str>, snap: bool) -> bool {
        let Some((t, s)) = self.locate_segment(segment) else { return false };
        let start = if snap { self.snap_time(segment, start) } else { start.max(0.0) };
        let dest = match to_track {
            Some(id) => match self.track_index(id) {
                Some(i) => i,
                None => return false,
            },
            None => t,
        };
        let key = format!("move:{segment}");
        let id = segment.to_string();
        let moved = self.doc.edit("Move segment", Some(&key), |a| {
            let mut seg = a.tracks[t].segments.remove(s);
            let changed = seg.start != start || dest != t;
            seg.start = start;
            if dest == t {
                a.tracks[t].segments.insert(s, seg);
            } else {
                a.tracks[dest].segments.push(seg);
            }
            (changed, changed)
        });
        if moved {
            if let Some(dest_track) = self.asset().tracks.get(dest).map(|t| t.id.clone()) {
                self.selection = AnimSelection::MontageSegment { track: dest_track, id };
            }
        }
        moved
    }

    pub fn end_gesture(&mut self) {
        self.doc.end_gesture();
    }

    /// Change a segment's order within its track and repack the segments back
    /// to back from the first segment's start (a ripple reorder).
    pub fn reorder_segment(&mut self, segment: &str, new_index: usize) -> bool {
        let Some((t, s)) = self.locate_segment(segment) else { return false };
        let count = self.asset().tracks[t].segments.len();
        let new_index = new_index.min(count - 1);
        if new_index == s {
            return false;
        }
        let spans: Vec<f32> = self.asset().tracks[t].segments.iter().map(|g| self.segment_span(g)).map(|(a, b)| b - a).collect();
        let origin = self.asset().tracks[t].segments.iter().map(|g| g.start).fold(f32::MAX, f32::min);
        let mut order: Vec<usize> = (0..count).collect();
        let moved = order.remove(s);
        order.insert(new_index, moved);
        self.doc.apply("Reorder segment", |a| {
            let old = std::mem::take(&mut a.tracks[t].segments);
            let mut at = origin;
            for i in order {
                let mut seg = old[i].clone();
                seg.start = at;
                at += spans[i];
                a.tracks[t].segments.push(seg);
            }
        });
        true
    }

    /// Trim the source clip range of a segment. `clip_end: None` plays to the
    /// clip's end. The new range must lie inside the clip when its duration is known.
    pub fn trim_segment(&mut self, segment: &str, clip_start: f32, clip_end: Option<f32>) -> Result<(), String> {
        let (t, s) = self.locate_segment(segment).ok_or("no such segment")?;
        if !clip_start.is_finite() || clip_start < 0.0 {
            return Err("trim start must be >= 0".into());
        }
        if let Some(end) = clip_end {
            if !end.is_finite() || end <= clip_start {
                return Err("trim end must be after the trim start".into());
            }
        }
        if let Some(d) = self.asset().tracks[t].segments[s].sequence.as_ref().and_then(|q| self.duration_of(q)) {
            if clip_start >= d {
                return Err(format!("trim start is past the clip end ({d:.3} s)"));
            }
        }
        let clip_end = match (clip_end, self.asset().tracks[t].segments[s].sequence.as_ref().and_then(|q| self.duration_of(q))) {
            (Some(e), Some(d)) if e >= d => None,
            (e, _) => e,
        };
        self.doc.edit("Trim segment", Some(&format!("trim:{segment}")), |a| {
            let seg = &mut a.tracks[t].segments[s];
            seg.clip_start = clip_start;
            seg.clip_end = clip_end;
            (true, ())
        });
        Ok(())
    }

    pub fn set_segment_rate(&mut self, segment: &str, rate: f32) -> Result<(), String> {
        let (t, s) = self.locate_segment(segment).ok_or("no such segment")?;
        if !rate.is_finite() || rate <= 0.0 {
            return Err("play rate must be positive".into());
        }
        self.doc.edit("Set segment rate", Some(&format!("rate:{segment}")), |a| {
            a.tracks[t].segments[s].play_rate = rate;
            (true, ())
        });
        Ok(())
    }

    pub fn set_segment_loops(&mut self, segment: &str, loops: u32) -> Result<(), String> {
        let (t, s) = self.locate_segment(segment).ok_or("no such segment")?;
        if loops == 0 {
            return Err("a segment plays at least once".into());
        }
        self.doc.edit("Set segment loops", Some(&format!("loops:{segment}")), |a| {
            a.tracks[t].segments[s].loop_count = loops;
            (true, ())
        });
        Ok(())
    }

    /// Whether a looping segment ends and begins its notify states again every cycle (default:
    /// states that cover the whole trimmed range run across the seam).
    pub fn set_segment_restart_states(&mut self, segment: &str, restart: bool) -> Result<(), String> {
        let (t, s) = self.locate_segment(segment).ok_or("no such segment")?;
        if self.asset().tracks[t].segments[s].restart_states_on_loop == restart {
            return Err("unchanged".into());
        }
        self.doc.edit("Set segment loop states", Some(&format!("loopstates:{segment}")), |a| {
            a.tracks[t].segments[s].restart_states_on_loop = restart;
            (true, ())
        });
        Ok(())
    }

    pub fn set_segment_sequence(&mut self, segment: &str, sequence: Option<AssetRef<AnimationSequenceAssetKind>>) -> bool {
        let Some((t, s)) = self.locate_segment(segment) else { return false };
        self.doc.edit("Set segment sequence", None, |a| {
            let changed = a.tracks[t].segments[s].sequence != sequence;
            a.tracks[t].segments[s].sequence = sequence;
            (changed, changed)
        })
    }

    pub fn delete_segment(&mut self, segment: &str) -> bool {
        let Some((t, s)) = self.locate_segment(segment) else { return false };
        self.doc.apply("Delete segment", |a| {
            a.tracks[t].segments.remove(s);
        });
        if matches!(&self.selection, AnimSelection::MontageSegment { id, .. } if id == segment) {
            self.selection = AnimSelection::None;
        }
        true
    }

    /// Split a segment at a montage time into two segments that play the same
    /// clip range back to back.
    pub fn split_segment(&mut self, segment: &str, at: f32) -> Result<String, String> {
        let (t, s) = self.locate_segment(segment).ok_or("no such segment")?;
        let seg = self.asset().tracks[t].segments[s].clone();
        let (start, end) = self.segment_span(&seg);
        if seg.loop_count != 1 {
            return Err("a looping segment cannot be split; set loops to 1 first".into());
        }
        if !(at > start + 1e-3 && at < end - 1e-3) {
            return Err("split inside the segment".into());
        }
        let clip_at = seg.clip_start + (at - start) * seg.play_rate;
        let mut second = MontageSegment::new(seg.sequence.clone(), at);
        second.clip_start = clip_at;
        second.clip_end = seg.clip_end;
        second.play_rate = seg.play_rate;
        let id = second.id.clone();
        self.doc.apply("Split segment", |a| {
            a.tracks[t].segments[s].clip_end = Some(clip_at);
            a.tracks[t].segments.insert(s + 1, second);
        });
        Ok(id)
    }

    // ---- sections -------------------------------------------------------

    fn unique_section_name(&self, base: &str) -> String {
        let taken = |n: &str| self.asset().sections.iter().any(|s| s.name == n);
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap_or_else(|| base.to_string())
    }

    pub fn add_section(&mut self, name: &str, start: f32) -> String {
        let name = self.unique_section_name(if name.trim().is_empty() { "Section" } else { name.trim() });
        let section = MontageSection { id: uuid::Uuid::new_v4().to_string(), name, start: start.max(0.0), next: None, extra: Default::default() };
        let id = section.id.clone();
        self.doc.apply("Add section", |a| a.sections.push(section));
        self.selection = AnimSelection::MontageSection { id: id.clone() };
        id
    }

    pub fn move_section(&mut self, id: &str, start: f32) -> bool {
        let Some(i) = self.asset().sections.iter().position(|s| s.id == id) else { return false };
        let start = start.max(0.0);
        self.doc.edit("Move section", Some(&format!("section:{id}")), |a| {
            let changed = a.sections[i].start != start;
            a.sections[i].start = start;
            (changed, changed)
        })
    }

    pub fn rename_section(&mut self, id: &str, name: &str) -> Result<(), String> {
        let i = self.asset().sections.iter().position(|s| s.id == id).ok_or("no such section")?;
        let name = name.trim();
        if name.is_empty() {
            return Err("a section needs a name".into());
        }
        if self.asset().sections.iter().enumerate().any(|(j, s)| j != i && s.name == name) {
            return Err("another section has that name".into());
        }
        let name = name.to_string();
        self.doc.edit("Rename section", Some(&format!("sname:{id}")), |a| {
            a.sections[i].name = name;
            (true, ())
        });
        Ok(())
    }

    /// Link `from` to play `to` next (`None` ends the montage). A link back to
    /// an earlier section is a loop and allowed.
    pub fn link_section(&mut self, from: &str, to: Option<&str>) -> Result<(), String> {
        let i = self.asset().sections.iter().position(|s| s.id == from).ok_or("no such section")?;
        if let Some(to) = to {
            if !self.asset().sections.iter().any(|s| s.id == to) {
                return Err("the next section does not exist".into());
            }
        }
        let to = to.map(str::to_string);
        self.doc.edit("Link section", None, |a| {
            let changed = a.sections[i].next != to;
            a.sections[i].next = to;
            (changed, ())
        });
        Ok(())
    }

    pub fn delete_section(&mut self, id: &str) -> bool {
        let Some(i) = self.asset().sections.iter().position(|s| s.id == id) else { return false };
        self.doc.apply("Delete section", |a| {
            a.sections.remove(i);
            for s in &mut a.sections {
                if s.next.as_deref() == Some(id) {
                    s.next = None;
                }
            }
        });
        if matches!(&self.selection, AnimSelection::MontageSection { id: s } if s == id) {
            self.selection = AnimSelection::None;
        }
        true
    }

    /// Links to draw between section markers.
    pub fn section_links(&self) -> Vec<SectionLink> {
        let sections = &self.asset().sections;
        sections
            .iter()
            .filter_map(|s| {
                let next = s.next.as_ref().and_then(|n| sections.iter().find(|o| &o.id == n))?;
                Some(SectionLink { from: s.id.clone(), to: next.id.clone(), from_time: s.start, to_time: next.start, backwards: next.start <= s.start })
            })
            .collect()
    }

    /// Section names in play order from `from`.
    pub fn play_order(&self, from: &str) -> Vec<String> {
        self.asset().section_walk(from).into_iter().filter_map(|id| self.asset().sections.iter().find(|s| s.id == id).map(|s| s.name.clone())).collect()
    }

    /// Trim one edge of a segment on the timeline. Dragging the left edge to
    /// `time` moves the segment start and the clip start together, so the
    /// remaining frames stay where they are; the right edge moves the clip end.
    pub fn trim_edge(&mut self, segment: &str, left: bool, time: f32) -> Result<(), String> {
        let (t, s) = self.locate_segment(segment).ok_or("no such segment")?;
        let seg = self.asset().tracks[t].segments[s].clone();
        let duration = seg.sequence.as_ref().and_then(|q| self.duration_of(q));
        let (start, end) = self.segment_span(&seg);
        let min = 0.02;
        if seg.loop_count != 1 {
            return Err("a looping segment is trimmed by its loop count; set loops to 1 first".into());
        }
        if left {
            let time = time.clamp(0.0, end - min);
            let clip_start = (seg.clip_start + (time - start) * seg.play_rate).max(0.0);
            if duration.is_some_and(|d| clip_start >= d) {
                return Err("the trim starts after the clip ends".into());
            }
            self.doc.edit("Trim segment", Some(&format!("trimedge:{segment}:l")), |a| {
                let g = &mut a.tracks[t].segments[s];
                g.start = time;
                g.clip_start = clip_start;
                (true, ())
            });
        } else {
            let time = time.max(start + min);
            let clip_end = seg.clip_start + (time - start) * seg.play_rate;
            let clip_end = match duration {
                Some(d) if clip_end >= d => None,
                _ => Some(clip_end),
            };
            self.doc.edit("Trim segment", Some(&format!("trimedge:{segment}:r")), |a| {
                a.tracks[t].segments[s].clip_end = clip_end;
                (true, ())
            });
        }
        Ok(())
    }

    // ---- blend, policy ---------------------------------------------------

    pub fn set_blend(&mut self, blend_in: bool, duration: f32, curve: BlendCurve) -> Result<(), String> {
        if !duration.is_finite() || duration < 0.0 {
            return Err("blend duration must be >= 0".into());
        }
        self.doc.edit("Set blend", Some(if blend_in { "blend_in" } else { "blend_out" }), |a| {
            let b = if blend_in { &mut a.blend_in } else { &mut a.blend_out };
            b.duration = duration;
            b.curve = curve;
            (true, ())
        });
        Ok(())
    }

    pub fn set_blend_out_time(&mut self, time: Option<f32>) -> Result<(), String> {
        if time.is_some_and(|t| !t.is_finite() || t < 0.0) {
            return Err("blend-out trigger must be >= 0".into());
        }
        self.doc.edit("Set blend-out trigger", Some("blend_out_time"), |a| {
            let changed = a.blend_out_time != time;
            a.blend_out_time = time;
            (changed, ())
        });
        Ok(())
    }

    pub fn set_group(&mut self, group: &str) -> Result<(), String> {
        let group = group.trim();
        if group.is_empty() {
            return Err("the group needs a name".into());
        }
        let group = group.to_string();
        self.doc.edit("Set group", Some("group"), |a| {
            let changed = a.group != group;
            a.group = group;
            (changed, ())
        });
        Ok(())
    }

    pub fn set_interrupt(&mut self, policy: InterruptPolicy) -> bool {
        self.doc.edit("Set interrupt policy", None, |a| {
            let changed = a.interrupt != policy;
            a.interrupt = policy;
            (changed, changed)
        })
    }

    pub fn set_priority(&mut self, priority: i32) -> bool {
        self.doc.edit("Set priority", Some("priority"), |a| {
            let changed = a.priority != priority;
            a.priority = priority;
            (changed, changed)
        })
    }

    pub fn set_root_motion(&mut self, mode: MontageRootMotion) -> bool {
        self.doc.edit("Set root motion", None, |a| {
            let changed = a.root_motion != mode;
            a.root_motion = mode;
            (changed, changed)
        })
    }

    pub fn rename(&mut self, name: &str) -> bool {
        let name = name.trim().to_string();
        if name.is_empty() {
            return false;
        }
        self.doc.edit("Rename", Some("rename"), |a| {
            let changed = a.name != name;
            a.name = name;
            (changed, changed)
        })
    }

    // ---- notifies ---------------------------------------------------------

    /// Run a notify command inside one undo step; selection follows `id`.
    pub fn notify_edit<R>(&mut self, label: &str, key: Option<&str>, f: impl FnOnce(&mut NotifyView<'_>, f32) -> (bool, R)) -> R {
        let limit = self.timeline_limit();
        self.doc.edit(label, key, |a| {
            let mut view = NotifyView { notifies: &mut a.notifies, states: &mut a.notify_states, schemas: &mut a.notify_schemas };
            f(&mut view, limit)
        })
    }

    pub fn add_notify(&mut self, name: &str, time: f32) -> String {
        let id = self.notify_edit("Add notify", None, |v, limit| {
            let id = v.add_notify(name, time, limit);
            (true, id)
        });
        self.selection = AnimSelection::Notify { id: id.clone() };
        id
    }

    pub fn add_notify_state(&mut self, name: &str, start: f32, end: f32) -> String {
        let id = self.notify_edit("Add notify state", None, |v, limit| {
            let id = v.add_state(name, start, end, limit);
            (true, id)
        });
        self.selection = AnimSelection::NotifyState { id: id.clone() };
        id
    }

    pub fn move_notify(&mut self, id: &str, time: f32) -> bool {
        self.notify_edit("Move notify", Some(&format!("nmove:{id}")), |v, limit| {
            let c = v.move_to(id, time, limit);
            (c, c)
        })
    }

    pub fn resize_notify_state(&mut self, id: &str, start: Option<f32>, end: Option<f32>) -> bool {
        self.notify_edit("Resize notify state", Some(&format!("nsize:{id}")), |v, limit| {
            let c = v.resize_state(id, start, end, limit);
            (c, c)
        })
    }

    pub fn delete_notify(&mut self, id: &str) -> bool {
        let deleted = self.notify_edit("Delete notify", None, |v, _| {
            let c = v.delete(id);
            (c, c)
        });
        if deleted && matches!(&self.selection, AnimSelection::Notify { id: s } | AnimSelection::NotifyState { id: s } if s == id) {
            self.selection = AnimSelection::None;
        }
        deleted
    }

    pub fn rename_notify(&mut self, id: &str, name: &str) -> bool {
        self.notify_edit("Rename notify", Some(&format!("nname:{id}")), |v, _| {
            let c = v.rename(id, name);
            (c, c)
        })
    }

    pub fn set_notify_field(&mut self, id: &str, name: &str, value: NotifyValue) -> Result<bool, String> {
        let result = self.notify_edit("Edit notify payload", Some(&format!("nfield:{id}:{name}")), |v, _| match v.set_field(id, name, value) {
            Ok(c) => (c, Ok(c)),
            Err(e) => (false, Err(e)),
        });
        result
    }

    pub fn remove_notify_field(&mut self, id: &str, name: &str) -> bool {
        self.notify_edit("Remove notify field", None, |v, _| {
            let c = v.remove_field(id, name);
            (c, c)
        })
    }

    pub fn set_notify_target(&mut self, id: &str, target: NotifyTarget) -> Result<bool, String> {
        self.notify_edit("Set notify target", None, |v, _| match v.set_target(id, target) {
            Ok(c) => (c, Ok(c)),
            Err(e) => (false, Err(e)),
        })
    }

    pub fn set_notify_schema(&mut self, id: &str, schema: Option<&str>) -> Result<bool, String> {
        self.notify_edit("Set payload schema", None, |v, _| match v.set_schema(id, schema) {
            Ok(c) => (c, Ok(c)),
            Err(e) => (false, Err(e)),
        })
    }

    pub fn set_state_disabled(&mut self, id: &str, disabled: bool) -> bool {
        self.notify_edit("Toggle notify state", None, |v, _| {
            let c = v.set_disabled(id, disabled);
            (c, c)
        })
    }

    pub fn lane(&self, metrics: &LaneMetrics) -> (Vec<LanePlacement>, usize) {
        let a = self.asset();
        let mut entries: Vec<LaneEntry> =
            a.notifies.iter().map(|n| LaneEntry { id: n.id.clone(), kind: NotifyKind::Notify, start: n.time, end: n.time, label_chars: n.name.chars().count() }).collect();
        entries.extend(a.notify_states.iter().map(|s| LaneEntry { id: s.id.clone(), kind: NotifyKind::State, start: s.start, end: s.end, label_chars: s.name.chars().count() }));
        lane_layout(&entries, metrics)
    }

    // ---- preview data ----------------------------------------------------

    /// Segments that play at montage time `t`, with their clip time. This is the
    /// authored layout; the animated preview itself runs the real montage
    /// runtime on a transient component.
    pub fn active_segments(&self, t: f32) -> Vec<ActiveSegment> {
        let mut out = Vec::new();
        for track in &self.asset().tracks {
            for seg in &track.segments {
                let (start, end) = self.segment_span(seg);
                if t >= start && t < end {
                    let into = (t - start) * seg.play_rate;
                    let clip_len = seg.sequence.as_ref().and_then(|q| self.duration_of(q)).map_or(f32::MAX, |d| seg.clip_end.unwrap_or(d).min(d) - seg.clip_start);
                    let local = if clip_len > 0.0 && clip_len < f32::MAX { into % clip_len } else { into };
                    out.push(ActiveSegment { track: track.id.clone(), slot: track.slot.clone(), segment: seg.id.clone(), clip_time: seg.clip_start + local });
                }
            }
        }
        out
    }

    pub fn validation(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if let Err(message) = self.asset().validate_structure() {
            out.push(Diagnostic::error("montage.invalid", message));
        }
        out.extend(self.asset().diagnostics(|s| self.duration_of(s)));
        let limit = self.timeline_limit();
        let a = self.asset();
        // notify-record checks that need the montage length
        let mut notifies = a.notifies.clone();
        let mut states = a.notify_states.clone();
        let mut schemas = a.notify_schemas.clone();
        out.extend(NotifyView { notifies: &mut notifies, states: &mut states, schemas: &mut schemas }.diagnostics(limit));
        out
    }

    pub fn save(&mut self) -> anyhow::Result<super::document::SaveOutcome> {
        self.doc.save()
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::MemStore;
    use super::*;

    fn model() -> (MontageModel, MemStore) {
        let store = MemStore::default();
        let m = MontageModel::open(Arc::new(store.clone()), Path::new("m.montage"), &AnimationMontageAsset::default().to_text().unwrap()).unwrap();
        (m, store)
    }

    fn seq(m: &mut MontageModel, duration: f32) -> AssetRef<AnimationSequenceAssetKind> {
        let r = AssetRef::new();
        m.durations.insert(r.as_str().to_string(), duration);
        r
    }

    #[test]
    fn tracks_are_unique_per_group_and_slot_and_named_plainly() {
        let (mut m, _) = model();
        let t = m.add_track("UpperBody", "DefaultGroup").unwrap();
        assert!(m.add_track("UpperBody", "DefaultGroup").is_err());
        assert!(m.add_track("Upper-Body", "DefaultGroup").is_err());
        let _u = m.add_track("LowerBody", "DefaultGroup").unwrap();
        assert!(m.rename_track(&t, "LowerBody", "DefaultGroup").is_err());
        m.rename_track(&t, "UpperBody", "Combat").unwrap();
        assert!(m.delete_track(&t));
        assert_ne!(m.selection, AnimSelection::MontageSlot { track: t });
    }

    #[test]
    fn a_slot_track_follows_a_skeleton_mask_by_name_and_undoes() {
        use skeletal_assets::bone_mask::{BoneMaskSpec, NamedBoneMask};
        let (mut m, _) = model();
        let t = m.add_track("UpperBody", "DefaultGroup").unwrap();
        let library = vec![NamedBoneMask { name: "Upper".into(), spec: BoneMaskSpec::default() }];
        assert!(m.set_track_mask(&t, Some("Nope"), &library).is_err());
        m.set_track_mask(&t, Some("Upper"), &library).unwrap();
        assert_eq!(m.asset().tracks[0].mask, Some(BoneMaskSpec::named("Upper")));
        assert!(m.set_track_mask(&t, Some("Upper"), &library).is_err(), "unchanged");
        m.set_track_mask(&t, None, &library).unwrap();
        assert_eq!(m.asset().tracks[0].mask, None);
        assert!(m.asset().to_text().unwrap().contains("slot") );
    }

    #[test]
    fn segments_append_back_to_back_trim_and_report_their_span() {
        let (mut m, _) = model();
        let t = m.add_track("UpperBody", "DefaultGroup").unwrap();
        let (a, b) = (seq(&mut m, 1.0), seq(&mut m, 2.0));
        let s1 = m.add_segment(&t, Some(a)).unwrap();
        let s2 = m.add_segment(&t, Some(b)).unwrap();
        assert_eq!(m.asset().tracks[0].segments[1].start, 1.0, "second segment follows the first");
        assert_eq!(m.length(), 3.0);
        m.set_segment_rate(&s2, 2.0).unwrap();
        assert_eq!(m.length(), 2.0, "double speed halves the segment");
        m.set_segment_loops(&s1, 2).unwrap();
        m.set_segment_restart_states(&s1, true).unwrap();
        assert!(m.asset().tracks[0].segments[0].restart_states_on_loop && m.set_segment_restart_states(&s1, true).is_err());
        let text = m.asset().to_text().unwrap();
        assert!(AnimationMontageAsset::parse(&text).unwrap().tracks[0].segments[0].restart_states_on_loop, "authored restart survives save and reopen");
        assert!(m.set_segment_loops(&s1, 0).is_err() && m.set_segment_rate(&s1, -1.0).is_err());
        assert!(m.trim_segment(&s2, 5.0, None).is_err(), "past the clip end");
        assert!(m.trim_segment(&s2, 0.5, Some(0.5)).is_err(), "empty trim");
        m.trim_segment(&s2, 0.5, Some(9.0)).unwrap();
        assert_eq!(m.asset().tracks[0].segments[1].clip_end, None, "an end past the clip means the clip end");
        m.trim_segment(&s2, 0.5, Some(1.5)).unwrap();
        assert_eq!(m.asset().tracks[0].segments[1].timeline_length(2.0), 0.5);
    }

    #[test]
    fn dragging_snaps_to_neighbouring_edges_and_is_one_undo_step() {
        let (mut m, _) = model();
        let t = m.add_track("UpperBody", "DefaultGroup").unwrap();
        let (a, b) = (seq(&mut m, 1.0), seq(&mut m, 1.0));
        let s1 = m.add_segment(&t, Some(a)).unwrap();
        let s2 = m.add_segment(&t, Some(b)).unwrap();
        let _ = s1;
        for x in [2.5, 2.2, 1.9, 1.6, 1.32, 1.01] {
            m.move_segment(&s2, x, None, true);
        }
        m.end_gesture();
        assert_eq!(m.asset().tracks[0].segments[1].start, 1.0, "snapped to the first segment's end");
        assert!(m.doc.undo());
        assert_eq!(m.asset().tracks[0].segments[1].start, 1.0, "undo returns to before the drag (which began at 1.0)");
        let s = m.move_segment(&s2, 0.5, None, false);
        assert!(s);
        assert_eq!(m.asset().tracks[0].segments[1].start, 0.5);
    }

    #[test]
    fn trimming_an_edge_keeps_the_remaining_frames_in_place() {
        let (mut m, _) = model();
        let t = m.add_track("UpperBody", "DefaultGroup").unwrap();
        let a = seq(&mut m, 2.0);
        let s = m.add_segment(&t, Some(a)).unwrap();
        m.move_segment(&s, 1.0, None, false);
        m.trim_edge(&s, true, 1.5).unwrap();
        let g = m.asset().tracks[0].segments[0].clone();
        assert_eq!((g.start, g.clip_start), (1.5, 0.5), "clip frame 0.5 still plays at 1.5 s");
        m.trim_edge(&s, false, 2.5).unwrap();
        assert_eq!(m.asset().tracks[0].segments[0].clip_end, Some(1.5));
        m.trim_edge(&s, false, 9.0).unwrap();
        assert_eq!(m.asset().tracks[0].segments[0].clip_end, None, "dragging past the clip end means the clip end");
        assert!(m.trim_edge(&s, true, 99.0).is_ok(), "clamped below the end edge");
        m.set_segment_loops(&s, 2).unwrap();
        assert!(m.trim_edge(&s, true, 1.0).is_err(), "looping segments are trimmed by loops");
    }

    #[test]
    fn segments_move_between_tracks_and_reorder_with_ripple() {
        let (mut m, _) = model();
        let t1 = m.add_track("UpperBody", "DefaultGroup").unwrap();
        let t2 = m.add_track("LowerBody", "DefaultGroup").unwrap();
        let (a, b, c) = (seq(&mut m, 1.0), seq(&mut m, 2.0), seq(&mut m, 0.5));
        let s1 = m.add_segment(&t1, Some(a)).unwrap();
        let s2 = m.add_segment(&t1, Some(b)).unwrap();
        let s3 = m.add_segment(&t1, Some(c)).unwrap();
        assert!(m.reorder_segment(&s3, 0));
        let order: Vec<_> = m.asset().tracks[0].segments.iter().map(|s| (s.id.clone(), s.start)).collect();
        assert_eq!(order, vec![(s3.clone(), 0.0), (s1.clone(), 0.5), (s2.clone(), 1.5)]);
        assert!(m.move_segment(&s2, 0.0, Some(&t2), false));
        assert_eq!(m.asset().tracks[1].segments.len(), 1);
        assert_eq!(m.asset().tracks[0].segments.len(), 2);
        assert_eq!(m.selection, AnimSelection::MontageSegment { track: t2, id: s2 });
    }

    #[test]
    fn split_keeps_the_clip_range_continuous() {
        let (mut m, _) = model();
        let t = m.add_track("UpperBody", "DefaultGroup").unwrap();
        let a = seq(&mut m, 2.0);
        let s1 = m.add_segment(&t, Some(a)).unwrap();
        assert!(m.split_segment(&s1, 5.0).is_err());
        let s2 = m.split_segment(&s1, 0.5).unwrap();
        let segs = &m.asset().tracks[0].segments;
        assert_eq!((segs[0].clip_end, segs[1].clip_start, segs[1].start), (Some(0.5), 0.5, 0.5));
        assert_eq!(m.length(), 2.0, "total length unchanged");
        assert_eq!(segs[1].id, s2);
        assert!(m.validation().iter().all(|d| d.code != "montage.segment_overlap"));
    }

    #[test]
    fn sections_link_loop_and_unlink_when_deleted() {
        let (mut m, _) = model();
        let start = m.add_section("Start", 0.0);
        let reload = m.add_section("Reload", 0.7);
        let end = m.add_section("End", 1.4);
        let dup = m.add_section("Start", 2.0);
        assert_eq!(m.asset().sections.iter().find(|s| s.id == dup).unwrap().name, "Start 2", "names stay unique");
        m.link_section(&start, Some(&reload)).unwrap();
        m.link_section(&reload, Some(&end)).unwrap();
        assert!(m.link_section(&end, Some("ghost")).is_err());
        assert_eq!(m.play_order(&start), ["Start", "Reload", "End"]);
        m.link_section(&end, Some(&reload)).unwrap();
        assert_eq!(m.play_order(&start), ["Start", "Reload", "End"], "a loop is walked once");
        let links = m.section_links();
        assert_eq!(links.len(), 3);
        assert!(links.iter().find(|l| l.from == end).unwrap().backwards);
        assert!(m.rename_section(&end, "Reload").is_err());
        m.rename_section(&end, "Finish").unwrap();
        assert!(m.delete_section(&reload));
        assert!(m.asset().sections.iter().all(|s| s.next.as_deref() != Some(reload.as_str())), "links to a deleted section are cleared");
        m.link_section(&start, None).unwrap();
    }

    #[test]
    fn blend_policy_and_root_motion_are_validated_and_edit_the_document() {
        let (mut m, _) = model();
        assert!(m.set_blend(true, -1.0, BlendCurve::Linear).is_err());
        m.set_blend(true, 0.15, BlendCurve::EaseOut).unwrap();
        m.set_blend(false, 0.3, BlendCurve::Linear).unwrap();
        m.set_blend_out_time(Some(0.25)).unwrap();
        assert!(m.set_blend_out_time(Some(-1.0)).is_err());
        assert!(m.set_group("  ").is_err());
        m.set_group("Combat").unwrap();
        assert!(m.set_interrupt(InterruptPolicy::RejectIfBusy) && !m.set_interrupt(InterruptPolicy::RejectIfBusy));
        m.set_priority(3);
        m.set_root_motion(MontageRootMotion::Ignore);
        let a = m.asset();
        assert_eq!((a.blend_in.duration, a.blend_in.curve, a.blend_out.duration), (0.15, BlendCurve::EaseOut, 0.3));
        assert_eq!((a.blend_out_time, a.group.as_str(), a.priority, a.root_motion), (Some(0.25), "Combat", 3, MontageRootMotion::Ignore));
    }

    #[test]
    fn notify_lanes_edit_inside_the_montage_and_follow_its_length() {
        let (mut m, _) = model();
        let t = m.add_track("UpperBody", "DefaultGroup").unwrap();
        let a = seq(&mut m, 1.86);
        m.add_segment(&t, Some(a)).unwrap();
        let n = m.add_notify("Footstep", 5.0);
        assert!((m.asset().notifies[0].time - 1.86).abs() < 1e-5, "clamped to the montage length");
        let s = m.add_notify_state("Window", 0.2, 0.9);
        assert!(m.move_notify(&n, 0.8));
        assert!(m.resize_notify_state(&s, None, Some(1.2)));
        assert!(m.set_notify_field(&n, "surface", NotifyValue::String("Stone".into())).unwrap());
        assert!(m.set_notify_field(&n, "bad name", NotifyValue::Bool(true)).is_err());
        assert!(m.set_state_disabled(&s, true));
        let (placed, rows) = m.lane(&LaneMetrics::default());
        assert_eq!((placed.len(), rows >= 1), (2, true));
        assert!(m.delete_notify(&n) && m.selection != AnimSelection::Notify { id: n });
        // every notify field survives save and reopen
    }

    #[test]
    fn active_segments_report_the_clip_time_for_layering() {
        let (mut m, _) = model();
        let t1 = m.add_track("UpperBody", "DefaultGroup").unwrap();
        let t2 = m.add_track("LowerBody", "DefaultGroup").unwrap();
        let (a, b) = (seq(&mut m, 2.0), seq(&mut m, 2.0));
        let s = m.add_segment(&t1, Some(a)).unwrap();
        m.add_segment(&t2, Some(b)).unwrap();
        m.trim_segment(&s, 0.5, None).unwrap();
        let at = m.active_segments(0.25);
        assert_eq!(at.len(), 2);
        assert!((at.iter().find(|x| x.slot == "UpperBody").unwrap().clip_time - 0.75).abs() < 1e-5);
        assert!(m.active_segments(10.0).is_empty());
    }

    #[test]
    fn everything_authored_round_trips_and_selection_does_not() {
        let (mut m, store) = model();
        let t = m.add_track("UpperBody", "Combat").unwrap();
        let a = seq(&mut m, 1.86);
        let s = m.add_segment(&t, Some(a.clone())).unwrap();
        m.set_segment_rate(&s, 1.25).unwrap();
        m.set_segment_loops(&s, 2).unwrap();
        m.trim_segment(&s, 0.1, Some(1.0)).unwrap();
        let sec1 = m.add_section("Start", 0.0);
        let sec2 = m.add_section("Reload", 0.5);
        m.link_section(&sec1, Some(&sec2)).unwrap();
        let n = m.add_notify("Footstep", 0.4);
        m.set_notify_field(&n, "surface", NotifyValue::String("Stone".into())).unwrap();
        m.set_notify_target(&n, NotifyTarget::ClassVariable("Weapon".into())).unwrap();
        let st = m.add_notify_state("Window", 0.2, 0.6);
        m.set_state_disabled(&st, true);
        m.set_blend(true, 0.1, BlendCurve::Linear).unwrap();
        m.select_segment(&t, &s);
        m.save().unwrap();
        let text = store.text("m.montage").unwrap();
        assert!(!text.contains("selection"));
        let r = MontageModel::open(Arc::new(store), Path::new("m.montage"), "").unwrap();
        let a2 = r.asset();
        let seg = &a2.tracks[0].segments[0];
        assert_eq!((seg.play_rate, seg.loop_count, seg.clip_start, seg.clip_end, seg.sequence.clone()), (1.25, 2, 0.1, Some(1.0), Some(a)));
        assert_eq!(a2.sections[0].next.as_deref(), Some(sec2.as_str()));
        assert_eq!(a2.notifies[0].fields.len(), 1);
        assert_eq!(a2.notifies[0].target, NotifyTarget::ClassVariable("Weapon".into()));
        assert!(a2.notify_states[0].disabled);
        assert_eq!((a2.tracks[0].slot.as_str(), a2.tracks[0].group.as_str(), a2.blend_in.duration), ("UpperBody", "Combat", 0.1));
    }
}

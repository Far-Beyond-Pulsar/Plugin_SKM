//! The `.montage` asset: sequence segments laid on named slot tracks, with
//! sections, blend in/out, an interruption policy and montage-owned notifies.
//!
//! Version 2 replaces the flat segment list with slot tracks (each names a slot
//! and its slot group), gives segments/sections stable ids, and links sections
//! by id. Version 1 migrates on read: all segments go on one `DefaultSlot`
//! track laid end to end, and section `next_section` names become ids.

use pulsar_reflection::{AnimationMontageAssetKind, AnimationSequenceAssetKind, AssetRef, SkeletonAssetKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::blend_space::Migration;
use crate::bone_mask::BoneMaskSpec;
use crate::codec::{
    self, derived_uuid, ensure_finite, ensure_unique, from_value, read_envelope, CodecError, Extra,
    VersionPolicy,
};
use crate::diagnostics::Diagnostic;
use crate::notifies::AnimationNotify;
use crate::notify_schema::{check_payload, AnimationNotifyState, NotifyPayloadSchema};

pub const MONTAGE_EXTENSION: &str = "montage";
pub const MONTAGE_VERSION: u32 = 2;
pub const DEFAULT_SLOT: &str = "DefaultSlot";
pub const DEFAULT_SLOT_GROUP: &str = "DefaultGroup";
const POLICY: VersionPolicy = VersionPolicy {
    oldest: 1,
    current: MONTAGE_VERSION,
    unversioned_as: Some(1),
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlendCurve {
    Linear,
    EaseIn,
    EaseOut,
    #[default]
    EaseInOut,
}

impl BlendCurve {
    /// Weight at normalised time `t` in `[0, 1]`; 0 at the start, 1 at the end.
    pub fn evaluate(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::EaseIn => t * t,
            Self::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Self::EaseInOut => t * t * (3.0 - 2.0 * t),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlendSettings {
    pub duration: f32,
    #[serde(default)]
    pub curve: BlendCurve,
}

impl Default for BlendSettings {
    fn default() -> Self {
        Self {
            duration: 0.2,
            curve: BlendCurve::default(),
        }
    }
}

/// What starting this montage does when its group is already playing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptPolicy {
    /// Stop the running montages of the group (they blend out), then play.
    #[default]
    ReplaceGroup,
    /// Do not start if the group is playing; the play command reports failure.
    RejectIfBusy,
    /// Start only when no montage of the group has equal or higher priority.
    StartIfHigherPriority,
    /// Play alongside; slots layer in priority order.
    Overlap,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MontageRootMotion {
    /// The montage supplies root motion while it has weight on its slot.
    #[default]
    Montage,
    /// Root motion keeps coming from the graph.
    Graph,
    Ignore,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MontageSegment {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<AssetRef<AnimationSequenceAssetKind>>,
    /// Where the segment starts on the montage timeline, in seconds.
    pub start: f32,
    /// Trim of the source clip: [clip_start, clip_end); `None` end is the clip end.
    #[serde(default)]
    pub clip_start: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_end: Option<f32>,
    #[serde(default = "one")]
    pub play_rate: f32,
    /// How many times the trimmed clip plays back to back.
    #[serde(default = "one_loop")]
    pub loop_count: u32,
    #[serde(flatten)]
    pub extra: Extra,
}

fn one() -> f32 {
    1.0
}
fn one_loop() -> u32 {
    1
}

impl MontageSegment {
    pub fn new(sequence: Option<AssetRef<AnimationSequenceAssetKind>>, start: f32) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            sequence,
            start,
            clip_start: 0.0,
            clip_end: None,
            play_rate: 1.0,
            loop_count: 1,
            extra: Extra::new(),
        }
    }

    /// Length on the montage timeline, given the source clip duration.
    pub fn timeline_length(&self, clip_duration: f32) -> f32 {
        let end = self.clip_end.unwrap_or(clip_duration).min(clip_duration);
        (end - self.clip_start).max(0.0) * self.loop_count as f32 / self.play_rate
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlotTrack {
    pub id: String,
    /// The slot this track drives; a graph Slot node of this name plays it.
    pub slot: String,
    pub group: String,
    #[serde(default)]
    pub segments: Vec<MontageSegment>,
    /// Limits the track to part of the skeleton; `None` drives the whole pose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<BoneMaskSpec>,
}

impl SlotTrack {
    pub fn new(slot: impl Into<String>, group: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            slot: slot.into(),
            group: group.into(),
            segments: Vec::new(),
            mask: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MontageSection {
    pub id: String,
    pub name: String,
    pub start: f32,
    /// Section id played after this one; `None` ends the montage (or falls
    /// through to the next section in time order when `auto_advance` is set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnimationMontageAsset {
    pub version: u32,
    pub id: AssetRef<AnimationMontageAssetKind>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeleton: Option<AssetRef<SkeletonAssetKind>>,
    #[serde(default)]
    pub tracks: Vec<SlotTrack>,
    #[serde(default)]
    pub sections: Vec<MontageSection>,
    #[serde(default)]
    pub blend_in: BlendSettings,
    #[serde(default)]
    pub blend_out: BlendSettings,
    /// Start blending out this many seconds before the end; `None` blends out
    /// only when stopped or at the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_out_time: Option<f32>,
    /// Group whose interruption rules govern this montage.
    #[serde(default = "default_group")]
    pub group: String,
    #[serde(default)]
    pub interrupt: InterruptPolicy,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub root_motion: MontageRootMotion,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notifies: Vec<AnimationNotify>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_states: Vec<AnimationNotifyState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_schemas: Vec<NotifyPayloadSchema>,
    #[serde(flatten)]
    pub extra: Extra,
}

fn default_group() -> String {
    DEFAULT_SLOT_GROUP.into()
}

impl Default for AnimationMontageAsset {
    fn default() -> Self {
        Self {
            version: MONTAGE_VERSION,
            id: AssetRef::new(),
            name: "New Montage".into(),
            skeleton: None,
            tracks: Vec::new(),
            sections: Vec::new(),
            blend_in: BlendSettings::default(),
            blend_out: BlendSettings::default(),
            blend_out_time: None,
            group: default_group(),
            interrupt: InterruptPolicy::default(),
            priority: 0,
            root_motion: MontageRootMotion::default(),
            notifies: Vec::new(),
            notify_states: Vec::new(),
            notify_schemas: Vec::new(),
            extra: Extra::new(),
        }
    }
}

fn slot_name_ok(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ' ')
}

impl AnimationMontageAsset {
    pub fn parse(text: &str) -> Result<Self, CodecError> {
        Self::parse_migrating(text).map(|(asset, _)| asset)
    }

    pub fn parse_migrating(text: &str) -> Result<(Self, Migration), CodecError> {
        let (mut value, from) = read_envelope(text, POLICY)?;
        let mut migration = Migration {
            from_version: from,
            ..Default::default()
        };
        if from < 2 {
            migrate_v1(&mut value, &mut migration);
        }
        if value.get("id").is_none() {
            return Err(CodecError::MissingId);
        }
        let mut asset: Self = from_value(value)?;
        asset.version = MONTAGE_VERSION;
        asset.validate_structure().map_err(CodecError::Invalid)?;
        Ok((asset, migration))
    }

    pub fn to_text(&self) -> anyhow::Result<String> {
        self.validate_structure().map_err(|e| anyhow::anyhow!(e))?;
        let mut asset = self.clone();
        asset.version = MONTAGE_VERSION;
        Ok(serde_json::to_string_pretty(&asset)?)
    }

    pub fn peek_id(text: &str) -> Option<String> {
        codec::peek_uuid(text)
    }

    pub fn segments(&self) -> impl Iterator<Item = &MontageSegment> {
        self.tracks.iter().flat_map(|t| t.segments.iter())
    }

    /// Montage length, given each sequence's duration (`None` for unknown).
    /// Segments whose clip length is unknown contribute their start only.
    pub fn length(&self, duration_of: impl Fn(&AssetRef<AnimationSequenceAssetKind>) -> Option<f32>) -> f32 {
        self.segments()
            .map(|s| {
                let len = s
                    .sequence
                    .as_ref()
                    .and_then(&duration_of)
                    .map_or(0.0, |d| s.timeline_length(d));
                s.start + len
            })
            .fold(0.0, f32::max)
    }

    pub fn validate_structure(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Montage needs a name".into());
        }
        for blend in [&self.blend_in, &self.blend_out] {
            if !blend.duration.is_finite() || blend.duration < 0.0 {
                return Err("Blend durations must be >= 0".into());
            }
        }
        if let Some(t) = self.blend_out_time {
            if !t.is_finite() || t < 0.0 {
                return Err("Blend-out trigger time must be >= 0".into());
            }
        }
        if self.group.trim().is_empty() {
            return Err("Montage group cannot be empty".into());
        }
        ensure_unique("slot track", self.tracks.iter().map(|t| t.id.as_str()))?;
        ensure_unique("segment", self.segments().map(|s| s.id.as_str()))?;
        ensure_unique("section", self.sections.iter().map(|s| s.id.as_str()))?;
        let mut slots = std::collections::HashSet::new();
        for track in &self.tracks {
            if !slot_name_ok(&track.slot) || !slot_name_ok(&track.group) {
                return Err(format!("Slot `{}` / group `{}` need plain names", track.slot, track.group));
            }
            if !slots.insert((&track.group, &track.slot)) {
                return Err(format!(
                    "Slot `{}` in group `{}` has two tracks",
                    track.slot, track.group
                ));
            }
            if let Some(mask) = &track.mask {
                mask.validate()?;
            }
            for segment in &track.segments {
                if segment.id.trim().is_empty() {
                    return Err("Segment id cannot be empty".into());
                }
                ensure_finite("segment start", segment.start)?;
                ensure_finite("segment clip start", segment.clip_start)?;
                if segment.start < 0.0 || segment.clip_start < 0.0 {
                    return Err("Segment times must be >= 0".into());
                }
                if let Some(end) = segment.clip_end {
                    ensure_finite("segment clip end", end)?;
                    if end <= segment.clip_start {
                        return Err(format!("Segment `{}` has an empty trim", segment.id));
                    }
                }
                if !segment.play_rate.is_finite() || segment.play_rate <= 0.0 {
                    return Err(format!("Segment `{}` play rate must be positive", segment.id));
                }
                if segment.loop_count == 0 {
                    return Err(format!("Segment `{}` must play at least once", segment.id));
                }
            }
        }
        for section in &self.sections {
            if section.name.trim().is_empty() {
                return Err("Section needs a name".into());
            }
            ensure_finite("section start", section.start)?;
            if section.start < 0.0 {
                return Err("Section start must be >= 0".into());
            }
            if let Some(next) = &section.next {
                if !self.sections.iter().any(|s| &s.id == next) {
                    return Err(format!("Section `{}` links to a section that does not exist", section.name));
                }
            }
        }
        ensure_unique("section name", self.sections.iter().map(|s| s.name.as_str()))?;
        let mut ids: Vec<&str> = self.notifies.iter().map(|n| n.id.as_str()).collect();
        ids.extend(self.notify_states.iter().map(|n| n.id.as_str()));
        ensure_unique("notify", ids)?;
        ensure_unique("payload schema", self.notify_schemas.iter().map(|s| s.id.as_str()))?;
        for schema in &self.notify_schemas {
            schema.validate()?;
        }
        for notify in &self.notifies {
            if notify.id.trim().is_empty() || notify.name.trim().is_empty() {
                return Err("Notify ID and name cannot be empty".into());
            }
            if !notify.time.is_finite() || notify.time < 0.0 {
                return Err("Notify time must be >= 0".into());
            }
            check_payload(&self.notify_schemas, &notify.schema, &notify.fields)?;
        }
        for state in &self.notify_states {
            // A montage's length depends on its sequences, so only the
            // clip-independent rules apply here; range-in-montage is a
            // diagnostic once lengths are known.
            state.validate(f32::MAX)?;
            check_payload(&self.notify_schemas, &state.schema, &state.fields)?;
        }
        Ok(())
    }

    /// Section ids in the order a straight-through play visits them from
    /// `from`, following `next` links until the end or a repeat.
    pub fn section_walk(&self, from: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut at = self.sections.iter().find(|s| s.id == from);
        while let Some(section) = at {
            if out.contains(&section.id.as_str()) {
                break;
            }
            out.push(section.id.as_str());
            at = section
                .next
                .as_ref()
                .and_then(|n| self.sections.iter().find(|s| &s.id == n));
        }
        out
    }

    /// Authoring problems that do not stop the file loading.
    /// `duration_of` gives each sequence's clip duration when it is known.
    pub fn diagnostics(
        &self,
        duration_of: impl Fn(&AssetRef<AnimationSequenceAssetKind>) -> Option<f32>,
    ) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if self.tracks.iter().all(|t| t.segments.is_empty()) {
            out.push(Diagnostic::error("montage.empty", "Montage has no segments").repair("Add a segment to a slot track"));
        }
        for track in &self.tracks {
            let mut spans: Vec<(f32, f32, &str)> = Vec::new();
            for segment in &track.segments {
                let Some(sequence) = &segment.sequence else {
                    out.push(Diagnostic::error("montage.empty_segment", "Segment has no sequence").at(&segment.id));
                    continue;
                };
                let Some(duration) = duration_of(sequence) else { continue };
                if segment.clip_start >= duration {
                    out.push(Diagnostic::error("montage.trim_outside_clip", "Segment trim starts after the clip ends").at(&segment.id));
                }
                if segment.clip_end.is_some_and(|e| e > duration) {
                    out.push(Diagnostic::warning("montage.trim_clamped", "Segment trim end is past the clip end and will be clamped").at(&segment.id));
                }
                spans.push((segment.start, segment.start + segment.timeline_length(duration), &segment.id));
            }
            spans.sort_by(|a, b| a.0.total_cmp(&b.0));
            for pair in spans.windows(2) {
                if pair[1].0 < pair[0].1 - 1e-4 {
                    out.push(
                        Diagnostic::error("montage.segment_overlap", format!("Segments overlap on slot `{}`", track.slot))
                            .at(pair[1].2)
                            .repair("Move or trim one of the overlapping segments"),
                    );
                }
            }
        }
        let length = self.length(&duration_of);
        for section in &self.sections {
            if length > 0.0 && section.start > length {
                out.push(Diagnostic::error("montage.section_outside", "Section starts after the montage ends").at(&section.id));
            }
        }
        if let Some(first) = self.sections.iter().map(|s| s.start).reduce(f32::min) {
            if first > 1e-4 {
                out.push(Diagnostic::warning(
                    "montage.no_section_at_start",
                    "No section starts at 0; the span before the first section can only be entered by playing from the start",
                ));
            }
        }
        for section in &self.sections {
            let walk = self.section_walk(&section.id);
            if let Some(last) = walk.last().and_then(|id| self.sections.iter().find(|s| &s.id == id)) {
                if last.next.as_deref().is_some_and(|n| walk.contains(&n)) && walk.len() == self.sections.len() && self.sections.iter().all(|s| s.next.is_some()) {
                    out.push(Diagnostic::warning("montage.endless_loop", "Every section links onward, so the montage never ends unless stopped").at(&section.id));
                    break;
                }
            }
        }
        for state in &self.notify_states {
            if length > 0.0 && state.end > length {
                out.push(Diagnostic::error("montage.notify_state_outside", "Notify state ends after the montage ends").at(&state.id));
            }
        }
        for notify in &self.notifies {
            if length > 0.0 && notify.time > length {
                out.push(Diagnostic::error("montage.notify_outside", "Notify is after the montage ends").at(&notify.id));
            }
        }
        if let Some(t) = self.blend_out_time {
            if length > 0.0 && t > length {
                out.push(Diagnostic::warning("montage.blend_out_longer", "Blend-out trigger is longer than the montage"));
            }
        }
        out
    }
}

fn migrate_v1(value: &mut Value, migration: &mut Migration) {
    let Some(object) = value.as_object_mut() else { return };
    if !object.contains_key("id") {
        object.insert("id".into(), Value::String(uuid::Uuid::new_v4().to_string()));
        migration.id_assigned = true;
        migration.notes.push("File had no id; one was assigned and must be saved".into());
    }
    let namespace = object.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();

    // Segments: one DefaultSlot track, laid end to end. Version 1 stored only a
    // start offset into the clip, so the timeline position is not recoverable;
    // each segment starts where the previous nominally began plus nothing, and
    // the user re-times them in the editor.
    let old_segments = object.remove("segments").and_then(|s| match s {
        Value::Array(a) => Some(a),
        _ => None,
    }).unwrap_or_default();
    let mut segments = Vec::new();
    for (index, segment) in old_segments.into_iter().enumerate() {
        let Value::Object(mut old) = segment else { continue };
        let mut new = serde_json::Map::new();
        new.insert("id".into(), Value::String(derived_uuid(&namespace, &format!("segment:{index}"))));
        if let Some(sequence) = old.remove("sequence") {
            new.insert("sequence".into(), sequence);
        }
        // `start` in v1 is the offset into the clip.
        new.insert("start".into(), Value::from(0.0));
        new.insert("clip_start".into(), old.remove("start").unwrap_or(Value::from(0.0)));
        if let Some(rate) = old.remove("play_rate") {
            new.insert("play_rate".into(), rate);
        }
        // A looping v1 segment had no count; two plays keeps it observably
        // repeating without inventing an endless loop.
        if old.remove("looping").and_then(|v| v.as_bool()).unwrap_or(false) {
            new.insert("loop_count".into(), Value::from(2));
            migration.notes.push(format!("Segment {index} looped; migrated to loop_count 2"));
        }
        for (k, v) in old {
            new.insert(k, v);
        }
        segments.push(Value::Object(new));
    }
    if !segments.is_empty() {
        migration.notes.push("Segments moved to DefaultSlot track; timeline starts need review".into());
        let track = serde_json::json!({
            "id": derived_uuid(&namespace, "track:default"),
            "slot": DEFAULT_SLOT,
            "group": DEFAULT_SLOT_GROUP,
            "segments": segments,
        });
        object.insert("tracks".into(), Value::Array(vec![track]));
    }

    // Sections: names become ids; next links by name resolve to ids.
    if let Some(Value::Array(sections)) = object.get("sections").cloned() {
        let names: Vec<(usize, String)> = sections
            .iter()
            .enumerate()
            .filter_map(|(i, s)| Some((i, s.get("name")?.as_str()?.to_string())))
            .collect();
        let id_of = |name: &str| {
            names
                .iter()
                .find(|(_, n)| n == name)
                .map(|(i, _)| derived_uuid(&namespace, &format!("section:{i}")))
        };
        let mut migrated = Vec::new();
        for (index, section) in sections.into_iter().enumerate() {
            let Value::Object(mut old) = section else { continue };
            let mut new = serde_json::Map::new();
            new.insert("id".into(), Value::String(derived_uuid(&namespace, &format!("section:{index}"))));
            if let Some(name) = old.remove("name") {
                new.insert("name".into(), name);
            }
            new.insert("start".into(), old.remove("start").unwrap_or(Value::from(0.0)));
            if let Some(end) = old.remove("end") {
                // Version 2 derives a section's end from the next section.
                new.insert("legacy_end".into(), end);
            }
            if let Some(Value::String(next)) = old.remove("next_section") {
                match id_of(&next) {
                    Some(id) => {
                        new.insert("next".into(), Value::String(id));
                    }
                    None => migration.notes.push(format!("Section {index} linked to missing section `{next}`; link dropped")),
                }
            }
            for (k, v) in old {
                new.insert(k, v);
            }
            migrated.push(Value::Object(new));
        }
        object.insert("sections".into(), Value::Array(migrated));
    }
    object.insert("version".into(), Value::from(MONTAGE_VERSION));
}

crate::asset_file_io!(AnimationMontageAsset, load_montage, save_montage);

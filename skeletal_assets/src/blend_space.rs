//! The `.blendspace` asset: a 1D or 2D parameter space of animation samples.
//!
//! Version 2 adds stable sample ids, axis grid/edge policy and smoothing,
//! per-sample rate/loop/phase/sync settings and a space-level sync mode.
//! Version 1 files (a name, two axes and bare samples) migrate on read with
//! their asset id kept. Playhead, preview input and selection are editor
//! state and are not part of this file.

use pulsar_reflection::{AnimationSequenceAssetKind, AssetRef, BlendSpaceAssetKind, SkeletonAssetKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codec::{
    self, derived_uuid, ensure_finite, ensure_unique, from_value, read_envelope, CodecError, Extra,
    VersionPolicy,
};
use crate::diagnostics::Diagnostic;

pub const BLEND_SPACE_EXTENSION: &str = "blendspace";
pub const BLEND_SPACE_VERSION: u32 = 2;
const POLICY: VersionPolicy = VersionPolicy {
    oldest: 1,
    current: BLEND_SPACE_VERSION,
    unversioned_as: Some(1),
};

/// What an input outside `[min, max]` does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgePolicy {
    /// Hold at the nearest edge.
    #[default]
    Clamp,
    /// Wrap around, for cyclic inputs such as a direction angle.
    Wrap,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlendAxis {
    pub name: String,
    pub min: f32,
    pub max: f32,
    /// Editor snap and display divisions; also the 2D triangulation grid hint.
    #[serde(default = "default_divisions")]
    pub grid_divisions: u32,
    #[serde(default)]
    pub edge: EdgePolicy,
    /// Seconds over which the runtime input approaches its target; 0 is instant.
    #[serde(default)]
    pub smoothing_time: f32,
}

fn default_divisions() -> u32 {
    4
}

impl Default for BlendAxis {
    fn default() -> Self {
        Self {
            name: "Speed".into(),
            min: 0.0,
            max: 1.0,
            grid_divisions: default_divisions(),
            edge: EdgePolicy::Clamp,
            smoothing_time: 0.0,
        }
    }
}

impl BlendAxis {
    pub fn span(&self) -> f32 {
        self.max - self.min
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleLoop {
    /// Follow the sequence's own looping.
    #[default]
    Inherit,
    Loop,
    Once,
}

/// One clip placed at a coordinate in the blend space.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlendSample {
    /// Stable id; weights, sync leaders and preview selection refer to it.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<AssetRef<AnimationSequenceAssetKind>>,
    pub x: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
    #[serde(default = "one")]
    pub play_rate: f32,
    #[serde(default)]
    pub looping: SampleLoop,
    /// Start phase in `[0, 1)` of the sample's clip.
    #[serde(default)]
    pub phase_offset: f32,
    /// Samples sharing a marker group advance phase-aligned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_group: Option<String>,
}

fn one() -> f32 {
    1.0
}

impl BlendSample {
    pub fn new(sequence: Option<AssetRef<AnimationSequenceAssetKind>>, x: f32, y: Option<f32>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            sequence,
            x,
            y,
            play_rate: 1.0,
            looping: SampleLoop::Inherit,
            phase_offset: 0.0,
            sync_group: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    #[default]
    None,
    /// Normalised clip phase is kept equal across contributing samples.
    Phase,
    /// Sync markers of the contributing sequences are aligned.
    Markers,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BlendSpaceSync {
    #[serde(default)]
    pub mode: SyncMode,
    /// Sample whose clock leads; `None` follows the highest-weight sample.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
}

impl BlendSpaceSync {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlendSpaceAsset {
    pub version: u32,
    pub id: AssetRef<BlendSpaceAssetKind>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeleton: Option<AssetRef<SkeletonAssetKind>>,
    pub x_axis: BlendAxis,
    /// Present for a 2D space, absent for a 1D strip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_axis: Option<BlendAxis>,
    #[serde(default)]
    pub samples: Vec<BlendSample>,
    #[serde(default, skip_serializing_if = "BlendSpaceSync::is_default")]
    pub sync: BlendSpaceSync,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for BlendSpaceAsset {
    fn default() -> Self {
        Self {
            version: BLEND_SPACE_VERSION,
            id: AssetRef::new(),
            name: "New Blend Space".into(),
            skeleton: None,
            x_axis: BlendAxis::default(),
            y_axis: None,
            samples: Vec::new(),
            sync: BlendSpaceSync::default(),
            extra: Extra::new(),
        }
    }
}

/// What reading an older file changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Migration {
    pub from_version: u32,
    pub id_assigned: bool,
    pub notes: Vec<String>,
}

impl BlendSpaceAsset {
    pub fn is_2d(&self) -> bool {
        self.y_axis.is_some()
    }

    pub fn parse(text: &str) -> Result<Self, CodecError> {
        Self::parse_migrating(text).map(|(asset, _)| asset)
    }

    /// Parse any readable version, returning the migration that was applied.
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
        asset.version = BLEND_SPACE_VERSION;
        asset.validate_structure().map_err(CodecError::Invalid)?;
        Ok((asset, migration))
    }

    pub fn to_text(&self) -> anyhow::Result<String> {
        self.validate_structure().map_err(|e| anyhow::anyhow!(e))?;
        let mut asset = self.clone();
        asset.version = BLEND_SPACE_VERSION;
        Ok(serde_json::to_string_pretty(&asset)?)
    }

    pub fn peek_id(text: &str) -> Option<String> {
        codec::peek_uuid(text)
    }

    /// Rules a file must satisfy to be read at all.
    pub fn validate_structure(&self) -> Result<(), String> {
        for (label, axis) in std::iter::once(("X", Some(&self.x_axis))).chain(std::iter::once(("Y", self.y_axis.as_ref()))) {
            let Some(axis) = axis else { continue };
            if axis.name.trim().is_empty() {
                return Err(format!("{label} axis needs a name"));
            }
            ensure_finite("axis min", axis.min)?;
            ensure_finite("axis max", axis.max)?;
            if axis.min >= axis.max {
                return Err(format!("{label} axis needs min < max"));
            }
            if axis.grid_divisions == 0 {
                return Err(format!("{label} axis needs at least one grid division"));
            }
            if !axis.smoothing_time.is_finite() || axis.smoothing_time < 0.0 {
                return Err(format!("{label} axis smoothing must be >= 0"));
            }
        }
        if let Some(y) = &self.y_axis {
            if y.name == self.x_axis.name {
                return Err("Axis names must differ".into());
            }
        }
        ensure_unique("blend sample", self.samples.iter().map(|s| s.id.as_str()))?;
        for sample in &self.samples {
            if sample.id.trim().is_empty() {
                return Err("Sample id cannot be empty".into());
            }
            ensure_finite("sample x", sample.x)?;
            if self.is_2d() != sample.y.is_some() {
                return Err(format!(
                    "Sample `{}` has {} coordinate(s) but the space is {}",
                    sample.id,
                    if sample.y.is_some() { "two" } else { "one" },
                    if self.is_2d() { "2D" } else { "1D" }
                ));
            }
            if let Some(y) = sample.y {
                ensure_finite("sample y", y)?;
            }
            if !sample.play_rate.is_finite() || sample.play_rate <= 0.0 {
                return Err(format!("Sample `{}` play rate must be positive", sample.id));
            }
            if !sample.phase_offset.is_finite() || !(0.0..1.0).contains(&sample.phase_offset) {
                return Err(format!("Sample `{}` phase offset must be in [0, 1)", sample.id));
            }
            if sample.sync_group.as_deref().is_some_and(|g| g.trim().is_empty()) {
                return Err(format!("Sample `{}` has an empty sync group", sample.id));
            }
        }
        if let Some(leader) = &self.sync.leader {
            if !self.samples.iter().any(|s| &s.id == leader) {
                return Err(format!("Sync leader `{leader}` is not a sample"));
            }
        }
        Ok(())
    }

    /// Authoring problems that do not stop the file loading.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for sample in &self.samples {
            if sample.sequence.is_none() {
                out.push(
                    Diagnostic::error("blendspace.empty_sample", "Sample has no sequence")
                        .at(&sample.id)
                        .repair("Pick a sequence for this sample or delete it"),
                );
            }
            let in_x = (self.x_axis.min..=self.x_axis.max).contains(&sample.x);
            let in_y = match (&self.y_axis, sample.y) {
                (Some(axis), Some(y)) => (axis.min..=axis.max).contains(&y),
                _ => true,
            };
            if !in_x || !in_y {
                out.push(
                    Diagnostic::error("blendspace.sample_out_of_range", "Sample lies outside the axes")
                        .at(&sample.id)
                        .repair("Move the sample inside the axis range or widen the axis"),
                );
            }
        }
        for (i, a) in self.samples.iter().enumerate() {
            for b in &self.samples[i + 1..] {
                if a.x == b.x && a.y == b.y {
                    out.push(
                        Diagnostic::error(
                            "blendspace.duplicate_position",
                            "Two samples share one position, so their weights are ambiguous",
                        )
                        .at(&b.id),
                    );
                }
            }
        }
        let need = if self.is_2d() { 3 } else { 2 };
        if self.samples.len() < need {
            out.push(Diagnostic::warning(
                "blendspace.too_few_samples",
                format!("A {} blend space needs at least {need} samples to blend", if self.is_2d() { "2D" } else { "1D" }),
            ));
        } else if self.is_2d() && collinear(&self.samples) {
            out.push(Diagnostic::error(
                "blendspace.collinear",
                "All samples lie on one line, so no triangulation exists",
            ).repair("Move one sample off the line"));
        }
        if self.sync.mode != SyncMode::None
            && self.samples.iter().any(|s| s.sync_group.is_some())
            && self.sync.mode == SyncMode::Phase
        {
            out.push(Diagnostic::warning(
                "blendspace.sync_group_ignored",
                "Per-sample sync groups only apply with marker sync; phase sync aligns all samples",
            ));
        }
        out
    }
}

fn collinear(samples: &[BlendSample]) -> bool {
    let pts: Vec<(f64, f64)> = samples
        .iter()
        .map(|s| (s.x as f64, s.y.unwrap_or(0.0) as f64))
        .collect();
    let (ax, ay) = pts[0];
    let Some(&(bx, by)) = pts.iter().find(|&&p| p != pts[0]) else {
        return true;
    };
    pts.iter()
        .all(|&(px, py)| ((bx - ax) * (py - ay) - (by - ay) * (px - ax)).abs() < 1e-9)
}

fn migrate_v1(value: &mut Value, migration: &mut Migration) {
    let Some(object) = value.as_object_mut() else { return };
    if !object.contains_key("id") {
        let id = uuid::Uuid::new_v4().to_string();
        object.insert("id".into(), Value::String(id));
        migration.id_assigned = true;
        migration.notes.push("File had no id; one was assigned and must be saved".into());
    }
    let namespace = object
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if let Some(samples) = object.get_mut("samples").and_then(|s| s.as_array_mut()) {
        for (index, sample) in samples.iter_mut().enumerate() {
            if let Some(sample) = sample.as_object_mut() {
                // Deterministic: migrating the same bytes twice mints the same ids.
                sample
                    .entry("id")
                    .or_insert_with(|| Value::String(derived_uuid(&namespace, &format!("sample:{index}"))));
            }
        }
    }
    object.insert("version".into(), Value::from(BLEND_SPACE_VERSION));
}

crate::asset_file_io!(BlendSpaceAsset, load_blend_space, save_blend_space);

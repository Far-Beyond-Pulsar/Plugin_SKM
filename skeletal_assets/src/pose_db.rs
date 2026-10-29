//! The `.posedb` asset (Pose Search Database).
//!
//! The file holds only *authored* settings: which clips are indexed, how they
//! are sampled, and the feature schema a query is matched against. The built
//! search index is derived data: it lives outside the asset (under
//! `.pulsar/cache/posedb/`), is keyed by a hash of the settings that affect it
//! and of the content of every source clip, and is rebuilt whenever that key
//! changes. Nothing generated is ever written into this file or into a source
//! animation.

use std::path::{Path, PathBuf};

use pulsar_reflection::{AnimationSequenceAssetKind, AssetRef, BlendSpaceAssetKind, PoseDatabaseAssetKind, SkeletonAssetKind};
use serde::{Deserialize, Serialize};

use crate::codec::{self, ensure_finite, ensure_unique, from_value, read_envelope, sha256_hex, CodecError, Extra, VersionPolicy};
use crate::diagnostics::Diagnostic;
use crate::skeleton::Skeleton;

pub const POSE_DB_EXTENSION: &str = "posedb";
pub const POSE_DB_VERSION: u32 = 1;
const POLICY: VersionPolicy = VersionPolicy { oldest: 1, current: POSE_DB_VERSION, unversioned_as: None };

/// Which animation a source entry indexes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PoseSource {
    Sequence { sequence: AssetRef<AnimationSequenceAssetKind> },
    /// Every sample sequence of the blend space is indexed as its own clip.
    BlendSpace { blend_space: AssetRef<BlendSpaceAssetKind> },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: f32,
    pub end: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PoseDbSource {
    pub id: String,
    #[serde(flatten)]
    pub source: PoseSource,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Also index the left/right mirrored clip.
    #[serde(default)]
    pub mirrored: bool,
    /// Whether the clip is treated as looping for trajectory sampling.
    #[serde(default)]
    pub looping: bool,
    /// Time ranges (in clip seconds) that are never matched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclusions: Vec<TimeRange>,
    /// Added to the cost of every pose from this source; positive discourages.
    #[serde(default)]
    pub cost_bias: f32,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Normalization {
    None,
    /// Zero mean, unit variance over the database.
    #[default]
    Standardize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChannelKind {
    BonePosition { bone: String },
    BoneRotation { bone: String },
    BoneVelocity { bone: String },
    /// Root trajectory sampled at `offsets` seconds (negative = past).
    Trajectory {
        offsets: Vec<f32>,
        #[serde(default = "yes")]
        position: bool,
        #[serde(default = "yes")]
        facing: bool,
        #[serde(default)]
        velocity: bool,
    },
    Curve { curve: String },
    /// The clip's normalised gait phase, when its sequence has sync markers.
    Phase,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureChannel {
    pub id: String,
    pub name: String,
    pub weight: f32,
    #[serde(default)]
    pub normalization: Normalization,
    #[serde(flatten)]
    pub kind: ChannelKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sampling {
    /// Seconds between indexed poses.
    pub interval: f32,
    /// Poses closer than this to a clip end are not indexed, so a match always
    /// has room to play.
    #[serde(default)]
    pub end_margin: f32,
}

impl Default for Sampling {
    fn default() -> Self {
        Self { interval: 1.0 / 30.0, end_margin: 0.1 }
    }
}

/// Runtime matching settings. They do not affect the built index.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchSettings {
    #[serde(default = "default_candidates")]
    pub candidate_count: u32,
    #[serde(default)]
    pub continuing_pose_bias: f32,
    #[serde(default = "default_blend")]
    pub blend_time: f32,
    /// Keep the last N queries and their cost breakdown for the debugger.
    #[serde(default = "default_history")]
    pub query_history: u32,
}

fn default_candidates() -> u32 {
    1
}
fn default_blend() -> f32 {
    0.2
}
fn default_history() -> u32 {
    64
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self { candidate_count: 1, continuing_pose_bias: 0.0, blend_time: 0.2, query_history: 64 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PoseDbAsset {
    pub version: u32,
    pub id: AssetRef<PoseDatabaseAssetKind>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeleton: Option<AssetRef<SkeletonAssetKind>>,
    #[serde(default)]
    pub sources: Vec<PoseDbSource>,
    #[serde(default)]
    pub channels: Vec<FeatureChannel>,
    #[serde(default)]
    pub sampling: Sampling,
    #[serde(default)]
    pub search: SearchSettings,
    #[serde(flatten)]
    pub extra: Extra,
}

impl PoseDbAsset {
    pub fn new(name: impl Into<String>, skeleton: impl Into<Option<AssetRef<SkeletonAssetKind>>>) -> Self {
        Self {
            version: POSE_DB_VERSION,
            id: AssetRef::new(),
            name: name.into(),
            skeleton: skeleton.into(),
            sources: Vec::new(),
            channels: Vec::new(),
            sampling: Sampling::default(),
            search: SearchSettings::default(),
            extra: Extra::new(),
        }
    }

    pub fn parse(text: &str) -> Result<Self, CodecError> {
        let (value, _) = read_envelope(text, POLICY)?;
        if value.get("id").is_none() {
            return Err(CodecError::MissingId);
        }
        let asset: Self = from_value(value)?;
        asset.validate_structure().map_err(CodecError::Invalid)?;
        Ok(asset)
    }

    pub fn to_text(&self) -> anyhow::Result<String> {
        self.validate_structure().map_err(|e| anyhow::anyhow!(e))?;
        let mut asset = self.clone();
        asset.version = POSE_DB_VERSION;
        Ok(serde_json::to_string_pretty(&asset)?)
    }

    pub fn peek_id(text: &str) -> Option<String> {
        codec::peek_uuid(text)
    }

    pub fn validate_structure(&self) -> Result<(), String> {
        ensure_unique("source", self.sources.iter().map(|s| s.id.as_str()))?;
        ensure_unique("channel", self.channels.iter().map(|c| c.id.as_str()))?;
        ensure_unique("channel name", self.channels.iter().map(|c| c.name.as_str()))?;
        if !self.sampling.interval.is_finite() || !(1.0 / 240.0..=1.0).contains(&self.sampling.interval) {
            return Err("Sampling interval must be between 1/240 s and 1 s".into());
        }
        if !self.sampling.end_margin.is_finite() || self.sampling.end_margin < 0.0 {
            return Err("End margin must be >= 0".into());
        }
        for s in &self.sources {
            ensure_finite("cost bias", s.cost_bias)?;
            for r in &s.exclusions {
                if !r.start.is_finite() || !r.end.is_finite() || r.start < 0.0 || r.start >= r.end {
                    return Err(format!("Source `{}` has an invalid exclusion range", s.id));
                }
            }
        }
        for c in &self.channels {
            if c.name.trim().is_empty() {
                return Err("Channel needs a name".into());
            }
            if !c.weight.is_finite() || c.weight < 0.0 {
                return Err(format!("Channel `{}` weight must be >= 0", c.name));
            }
            if let ChannelKind::Trajectory { offsets, position, facing, velocity } = &c.kind {
                if offsets.is_empty() {
                    return Err(format!("Trajectory channel `{}` needs sample offsets", c.name));
                }
                if offsets.iter().any(|o| !o.is_finite() || *o == 0.0) || offsets.windows(2).any(|w| w[0] >= w[1]) {
                    return Err(format!("Trajectory channel `{}` offsets must be finite, nonzero and strictly increasing", c.name));
                }
                if !(*position || *facing || *velocity) {
                    return Err(format!("Trajectory channel `{}` selects no feature", c.name));
                }
            }
        }
        let s = &self.search;
        if s.candidate_count == 0 || !s.blend_time.is_finite() || s.blend_time < 0.0 || !s.continuing_pose_bias.is_finite() {
            return Err("Search settings are out of range".into());
        }
        Ok(())
    }

    /// Problems that need no skeleton content.
    pub fn reference_diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if self.skeleton.is_none() {
            out.push(Diagnostic::error("posedb.no_skeleton", "The database has no skeleton").repair("Pick a skeleton for the database"));
        }
        out
    }

    /// Authoring problems that do not stop the file loading.
    pub fn diagnostics(&self, skeleton: &Skeleton) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if self.channels.is_empty() {
            out.push(Diagnostic::error("posedb.no_channels", "The schema has no feature channels, so there is nothing to match").repair("Add a pose or trajectory channel"));
        } else if self.channels.iter().all(|c| c.weight == 0.0) {
            out.push(Diagnostic::error("posedb.zero_weights", "Every channel has weight 0, so every pose costs the same"));
        }
        let enabled = self.sources.iter().filter(|s| s.enabled).count();
        if enabled == 0 {
            out.push(Diagnostic::error("posedb.no_sources", "No enabled source clips").repair("Add a sequence or enable one"));
        }
        for c in &self.channels {
            let bone = match &c.kind {
                ChannelKind::BonePosition { bone } | ChannelKind::BoneRotation { bone } | ChannelKind::BoneVelocity { bone } => Some(bone),
                _ => None,
            };
            if let Some(bone) = bone {
                if skeleton.bone(bone).is_none() {
                    out.push(Diagnostic::error("posedb.unknown_bone", format!("Channel `{}` names bone `{bone}` which the skeleton does not have", c.name)).at(&c.id).repair("Pick a bone of the database's skeleton"));
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        for s in &self.sources {
            let key = serde_json::to_string(&s.source).unwrap_or_default();
            if !seen.insert((key, s.mirrored)) {
                out.push(Diagnostic::warning("posedb.duplicate_source", "The same clip is listed twice with the same mirroring; its poses would be indexed twice").at(&s.id));
            }
        }
        out
    }

    /// Hash of every setting that changes the built index. Display name, search
    /// settings and unknown fields are excluded: editing them never forces a
    /// rebuild. Disabled sources are excluded too.
    pub fn settings_hash(&self) -> String {
        #[derive(Serialize)]
        struct Canonical<'a> {
            skeleton: Option<&'a str>,
            sources: Vec<&'a PoseDbSource>,
            channels: &'a [FeatureChannel],
            sampling: &'a Sampling,
        }
        let mut sources: Vec<&PoseDbSource> = self.sources.iter().filter(|s| s.enabled).collect();
        sources.sort_by(|a, b| a.id.cmp(&b.id));
        let canonical = Canonical { skeleton: self.skeleton.as_ref().map(|s| s.as_str()), sources, channels: &self.channels, sampling: &self.sampling };
        sha256_hex(&serde_json::to_vec(&canonical).unwrap_or_default())
    }

    /// The key of the derived index for the current settings and the content
    /// hash of each enabled source's asset (`(source id, content hash)`).
    pub fn index_key(&self, source_hashes: &[(String, String)]) -> IndexKey {
        let mut sorted = source_hashes.to_vec();
        sorted.sort();
        let mut material = self.settings_hash();
        for (id, hash) in &sorted {
            material.push('\n');
            material.push_str(id);
            material.push(':');
            material.push_str(hash);
        }
        IndexKey { builder_version: INDEX_BUILDER_VERSION, hash: sha256_hex(material.as_bytes()) }
    }
}

/// Bump when the index build changes in a way old payloads cannot satisfy.
pub const INDEX_BUILDER_VERSION: u32 = 1;
const INDEX_MAGIC: &[u8; 4] = b"PDIX";

/// Identifies one derived index: builder version plus input hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexKey {
    pub builder_version: u32,
    pub hash: String,
}

/// Where the derived index for `asset` with `key` lives, under the project root.
pub fn derived_index_path(project_root: &Path, asset: &AssetRef<PoseDatabaseAssetKind>, key: &IndexKey) -> PathBuf {
    project_root
        .join(".pulsar")
        .join("cache")
        .join("posedb")
        .join(format!("{}-{}.bin", asset.as_str(), &key.hash[..16]))
}

/// Wrap a built payload with its key: `PDIX`, builder version, hash, payload length.
pub fn encode_index(key: &IndexKey, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 80);
    out.extend_from_slice(INDEX_MAGIC);
    out.extend_from_slice(&key.builder_version.to_le_bytes());
    out.extend_from_slice(key.hash.as_bytes());
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexState {
    /// The payload matches `key`.
    Current(Vec<u8>),
    /// Built for other inputs; rebuild.
    Stale { found: IndexKey },
    /// Not an index, or truncated; rebuild.
    Corrupt,
}

/// Classify cached bytes against the key the asset needs now.
pub fn check_index(bytes: &[u8], key: &IndexKey) -> IndexState {
    let hash_len = key.hash.len();
    let header = 4 + 4 + hash_len + 8;
    if bytes.len() < header || &bytes[..4] != INDEX_MAGIC {
        return IndexState::Corrupt;
    }
    let builder_version = u32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes"));
    let Ok(hash) = std::str::from_utf8(&bytes[8..8 + hash_len]) else { return IndexState::Corrupt };
    let len = u64::from_le_bytes(bytes[8 + hash_len..header].try_into().expect("8 bytes")) as usize;
    if bytes.len() != header + len {
        return IndexState::Corrupt;
    }
    let found = IndexKey { builder_version, hash: hash.to_string() };
    if &found == key {
        IndexState::Current(bytes[header..].to_vec())
    } else {
        IndexState::Stale { found }
    }
}

crate::asset_file_io!(PoseDbAsset, load_pose_db, save_pose_db);

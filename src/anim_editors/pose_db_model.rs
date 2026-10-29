//! Pose Search Database editor model: source clips, feature channels, sampling
//! and search settings, the derived-index build state and the query debugger.
//!
//! The asset stores only authored settings. The index is derived data keyed by
//! the settings and the source content hashes (`PoseDbAsset::index_key`); the
//! model reports whether a cached index is current and runs the real builder
//! and search from `skeletal_assets::runtime::posesearch`.

use std::path::Path;
use std::sync::Arc;

use pulsar_reflection::{AnimationSequenceAssetKind, AssetRef, BlendSpaceAssetKind};
use skeletal_assets::diagnostics::Diagnostic;
use skeletal_assets::pose_db::{
    check_index, ChannelKind, FeatureChannel, IndexKey, IndexState, Normalization, PoseDbAsset, PoseDbSource, PoseSource, SearchSettings, TimeRange,
};
use skeletal_assets::runtime::clip::CompiledClip;
use skeletal_assets::runtime::posesearch::{layout, ChannelSpan, DesiredTrajectory, FrameFeatures, MirrorTable, PoseIndex, SearchClip, SearchResult};
use skeletal_assets::runtime::pose::RuntimeSkeleton;
use skeletal_assets::skeleton::Skeleton;

use super::document::AnimDocument;
use super::selection::AnimSelection;
use super::store::DocStore;

/// Status of the cached derived index against the current authored data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexStatus {
    /// No cache file.
    Missing,
    /// A cache for other settings or sources; rebuild.
    Stale { cached_hash: String },
    Corrupt,
    Current,
}

/// What the last build produced. Editor state, never saved in the asset.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildInfo {
    pub builder_version: u32,
    pub hash: String,
    pub entries: usize,
    pub dims: usize,
    pub clips: usize,
}

/// One query for the debugger timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryRecord {
    pub desired_speed: f32,
    pub result: SearchResult,
    /// Source id of the matched clip.
    pub source: String,
}

pub struct PoseDbModel {
    pub doc: AnimDocument<PoseDbAsset>,
    pub selection: AnimSelection,
    pub skeleton: Option<Skeleton>,
    pub last_build: Option<BuildInfo>,
    pub build_diagnostics: Vec<Diagnostic>,
    pub queries: Vec<QueryRecord>,
    index: Option<(PoseIndex, Vec<SearchClip>)>,
}

fn identifier_ok(name: &str) -> bool {
    !name.trim().is_empty()
}

impl PoseDbModel {
    pub fn open(store: Arc<dyn DocStore>, path: &Path, default_text: &str) -> Result<Self, String> {
        let doc = AnimDocument::<PoseDbAsset>::open(store, path, default_text)?;
        Ok(Self { doc, selection: AnimSelection::None, skeleton: None, last_build: None, build_diagnostics: Vec::new(), queries: Vec::new(), index: None })
    }

    pub fn asset(&self) -> &PoseDbAsset {
        self.doc.value()
    }

    // ---- sources ---------------------------------------------------------

    fn source_index(&self, id: &str) -> Option<usize> {
        self.asset().sources.iter().position(|s| s.id == id)
    }

    fn push_source(&mut self, source: PoseSource) -> Result<String, String> {
        if self.asset().sources.iter().any(|s| s.source == source) {
            return Err("that asset is already a source".into());
        }
        let entry = PoseDbSource { id: uuid::Uuid::new_v4().to_string(), source, enabled: true, mirrored: false, looping: false, exclusions: Vec::new(), cost_bias: 0.0 };
        let id = entry.id.clone();
        self.doc.apply("Add source", |a| a.sources.push(entry));
        self.selection = AnimSelection::PoseSource { id: id.clone() };
        Ok(id)
    }

    pub fn add_sequence_source(&mut self, sequence: AssetRef<AnimationSequenceAssetKind>) -> Result<String, String> {
        self.push_source(PoseSource::Sequence { sequence })
    }

    pub fn add_blend_space_source(&mut self, blend_space: AssetRef<BlendSpaceAssetKind>) -> Result<String, String> {
        self.push_source(PoseSource::BlendSpace { blend_space })
    }

    pub fn select_source(&mut self, id: &str) -> bool {
        if self.source_index(id).is_some() {
            self.selection = AnimSelection::PoseSource { id: id.into() };
            true
        } else {
            false
        }
    }

    /// Point a source at another asset of the same or the other kind.
    pub fn replace_source_asset(&mut self, id: &str, source: PoseSource) -> Result<(), String> {
        let i = self.source_index(id).ok_or("no such source")?;
        if self.asset().sources.iter().enumerate().any(|(j, s)| j != i && s.source == source) {
            return Err("that asset is already a source".into());
        }
        self.doc.edit("Replace source", None, |a| {
            let changed = a.sources[i].source != source;
            a.sources[i].source = source;
            (changed, ())
        });
        Ok(())
    }

    pub fn set_source_flags(&mut self, id: &str, enabled: bool, mirrored: bool, looping: bool) -> bool {
        let Some(i) = self.source_index(id) else { return false };
        self.doc.edit("Set source options", None, |a| {
            let s = &mut a.sources[i];
            let changed = (s.enabled, s.mirrored, s.looping) != (enabled, mirrored, looping);
            s.enabled = enabled;
            s.mirrored = mirrored;
            s.looping = looping;
            (changed, changed)
        })
    }

    pub fn set_source_bias(&mut self, id: &str, bias: f32) -> Result<(), String> {
        let i = self.source_index(id).ok_or("no such source")?;
        if !bias.is_finite() {
            return Err("the cost bias must be a number".into());
        }
        self.doc.edit("Set cost bias", Some(&format!("bias:{id}")), |a| {
            a.sources[i].cost_bias = bias;
            (true, ())
        });
        Ok(())
    }

    pub fn add_exclusion(&mut self, id: &str, start: f32, end: f32) -> Result<(), String> {
        let i = self.source_index(id).ok_or("no such source")?;
        if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
            return Err("an exclusion needs 0 <= start < end".into());
        }
        self.doc.apply("Add exclusion", |a| a.sources[i].exclusions.push(TimeRange { start, end }));
        Ok(())
    }

    pub fn remove_exclusion(&mut self, id: &str, index: usize) -> bool {
        let Some(i) = self.source_index(id) else { return false };
        if index >= self.asset().sources[i].exclusions.len() {
            return false;
        }
        self.doc.apply("Remove exclusion", |a| {
            a.sources[i].exclusions.remove(index);
        });
        true
    }

    pub fn delete_source(&mut self, id: &str) -> bool {
        let Some(i) = self.source_index(id) else { return false };
        self.doc.apply("Delete source", |a| {
            a.sources.remove(i);
        });
        if matches!(&self.selection, AnimSelection::PoseSource { id: s } if s == id) {
            self.selection = AnimSelection::None;
        }
        true
    }

    // ---- channels ----------------------------------------------------------

    fn push_channel(&mut self, name: &str, weight: f32, kind: ChannelKind) -> Result<usize, String> {
        let name = name.trim();
        if !identifier_ok(name) {
            return Err("a channel needs a name".into());
        }
        if self.asset().channels.iter().any(|c| c.name == name) {
            return Err(format!("a channel named `{name}` exists"));
        }
        if !weight.is_finite() || weight < 0.0 {
            return Err("the weight must be 0 or more".into());
        }
        if let Some(bone) = match &kind {
            ChannelKind::BonePosition { bone } | ChannelKind::BoneRotation { bone } | ChannelKind::BoneVelocity { bone } => Some(bone),
            _ => None,
        } {
            if self.skeleton.as_ref().is_some_and(|s| s.bone(bone).is_none()) {
                return Err("pick a bone of the database's skeleton".into());
            }
        }
        if let ChannelKind::Trajectory { offsets, position, facing, velocity } = &kind {
            if offsets.is_empty() || offsets.iter().any(|o| !o.is_finite()) {
                return Err("a trajectory needs at least one sample time".into());
            }
            if !(*position || *facing || *velocity) {
                return Err("a trajectory samples position, facing or velocity".into());
            }
        }
        let channel = FeatureChannel { id: uuid::Uuid::new_v4().to_string(), name: name.to_string(), weight, normalization: Normalization::default(), kind };
        let index = self.asset().channels.len();
        self.doc.apply("Add feature channel", |a| a.channels.push(channel));
        self.selection = AnimSelection::PoseChannel { index };
        Ok(index)
    }

    pub fn add_bone_channel(&mut self, name: &str, bone: &str, position: bool, weight: f32) -> Result<usize, String> {
        let bone = bone.to_string();
        self.push_channel(name, weight, if position { ChannelKind::BonePosition { bone } } else { ChannelKind::BoneVelocity { bone } })
    }

    pub fn add_rotation_channel(&mut self, name: &str, bone: &str, weight: f32) -> Result<usize, String> {
        self.push_channel(name, weight, ChannelKind::BoneRotation { bone: bone.to_string() })
    }

    pub fn add_trajectory_channel(&mut self, name: &str, mut offsets: Vec<f32>, weight: f32) -> Result<usize, String> {
        offsets.sort_by(f32::total_cmp);
        offsets.dedup();
        self.push_channel(name, weight, ChannelKind::Trajectory { offsets, position: true, facing: true, velocity: false })
    }

    pub fn add_curve_channel(&mut self, name: &str, curve: &str, weight: f32) -> Result<usize, String> {
        if curve.trim().is_empty() {
            return Err("name a curve".into());
        }
        self.push_channel(name, weight, ChannelKind::Curve { curve: curve.trim().to_string() })
    }

    pub fn add_phase_channel(&mut self, weight: f32) -> Result<usize, String> {
        self.push_channel("Phase", weight, ChannelKind::Phase)
    }

    pub fn select_channel(&mut self, index: usize) -> bool {
        if index < self.asset().channels.len() {
            self.selection = AnimSelection::PoseChannel { index };
            true
        } else {
            false
        }
    }

    pub fn set_channel_weight(&mut self, index: usize, weight: f32) -> Result<(), String> {
        if index >= self.asset().channels.len() {
            return Err("no such channel".into());
        }
        if !weight.is_finite() || weight < 0.0 {
            return Err("the weight must be 0 or more".into());
        }
        self.doc.edit("Set channel weight", Some(&format!("cw:{index}")), |a| {
            a.channels[index].weight = weight;
            (true, ())
        });
        Ok(())
    }

    pub fn set_channel_normalization(&mut self, index: usize, normalization: Normalization) -> bool {
        if index >= self.asset().channels.len() {
            return false;
        }
        self.doc.edit("Set normalisation", None, |a| {
            let changed = a.channels[index].normalization != normalization;
            a.channels[index].normalization = normalization;
            (changed, changed)
        })
    }

    pub fn set_trajectory(&mut self, index: usize, mut offsets: Vec<f32>, position: bool, facing: bool, velocity: bool) -> Result<(), String> {
        if !matches!(self.asset().channels.get(index).map(|c| &c.kind), Some(ChannelKind::Trajectory { .. })) {
            return Err("that channel is not a trajectory".into());
        }
        if offsets.is_empty() || offsets.iter().any(|o| !o.is_finite()) {
            return Err("a trajectory needs at least one sample time".into());
        }
        if !(position || facing || velocity) {
            return Err("a trajectory samples position, facing or velocity".into());
        }
        offsets.sort_by(f32::total_cmp);
        offsets.dedup();
        self.doc.edit("Edit trajectory", Some(&format!("traj:{index}")), |a| {
            a.channels[index].kind = ChannelKind::Trajectory { offsets, position, facing, velocity };
            (true, ())
        });
        Ok(())
    }

    pub fn delete_channel(&mut self, index: usize) -> bool {
        if index >= self.asset().channels.len() {
            return false;
        }
        self.doc.apply("Delete feature channel", |a| {
            a.channels.remove(index);
        });
        self.selection = AnimSelection::None;
        true
    }

    /// The feature layout: each channel's slice of the feature vector.
    pub fn schema(&self) -> Vec<ChannelSpan> {
        layout(self.asset())
    }

    pub fn feature_dimensions(&self) -> usize {
        self.schema().iter().map(|s| s.len).sum()
    }

    // ---- sampling & search ---------------------------------------------------

    pub fn set_sampling(&mut self, interval: f32, end_margin: f32) -> Result<(), String> {
        if !interval.is_finite() || interval <= 0.0 {
            return Err("the sample interval must be positive".into());
        }
        if !end_margin.is_finite() || end_margin < 0.0 {
            return Err("the end margin must be 0 or more".into());
        }
        self.doc.edit("Set sampling", Some("sampling"), |a| {
            a.sampling.interval = interval;
            a.sampling.end_margin = end_margin;
            (true, ())
        });
        Ok(())
    }

    pub fn set_search(&mut self, settings: SearchSettings) -> Result<(), String> {
        if settings.candidate_count == 0 {
            return Err("keep at least one candidate".into());
        }
        if !settings.continuing_pose_bias.is_finite() || settings.continuing_pose_bias < 0.0 || !settings.blend_time.is_finite() || settings.blend_time < 0.0 {
            return Err("bias and blend time must be 0 or more".into());
        }
        self.doc.edit("Set search settings", Some("search"), |a| {
            let changed = a.search != settings;
            a.search = settings;
            (changed, ())
        });
        Ok(())
    }

    // ---- derived index -----------------------------------------------------------

    /// The key the index must have now. `source_hashes` is `(source id, content
    /// hash of its asset)` for the enabled sources.
    pub fn index_key(&self, source_hashes: &[(String, String)]) -> IndexKey {
        self.asset().index_key(source_hashes)
    }

    pub fn index_status(&self, cached: Option<&[u8]>, key: &IndexKey) -> IndexStatus {
        match cached.map(|b| check_index(b, key)) {
            None => IndexStatus::Missing,
            Some(IndexState::Current(_)) => IndexStatus::Current,
            Some(IndexState::Stale { found }) => IndexStatus::Stale { cached_hash: found.hash },
            Some(IndexState::Corrupt) => IndexStatus::Corrupt,
        }
    }

    /// Run the real index builder over compiled source clips.
    pub fn build_index(&mut self, skeleton: &Skeleton, clips: Vec<SearchClip>, key: &IndexKey) -> Result<BuildInfo, Vec<Diagnostic>> {
        let runtime = RuntimeSkeleton::new("posedb", skeleton).map_err(|e| vec![Diagnostic::error("posedb.skeleton", e.to_string())])?;
        let mirror = clips.iter().any(|c| c.mirrored).then(|| MirrorTable::from_names(&runtime));
        match PoseIndex::build(self.asset(), &runtime, &clips, mirror.as_ref()) {
            Ok(index) => {
                let info = BuildInfo { builder_version: key.builder_version, hash: key.hash.clone(), entries: index.entries.len(), dims: index.dims, clips: clips.len() };
                self.last_build = Some(info.clone());
                self.build_diagnostics.clear();
                self.index = Some((index, clips));
                Ok(info)
            }
            Err(diagnostics) => {
                self.last_build = None;
                self.build_diagnostics = diagnostics.clone();
                self.index = None;
                Err(diagnostics)
            }
        }
    }

    /// The payload to cache next to the project for the last build.
    pub fn index_payload(&self) -> Option<Vec<u8>> {
        self.index.as_ref().map(|(i, _)| i.to_payload())
    }

    /// Whether editing has invalidated the last build: any authored change
    /// since it ran. A build must be rerun to be trusted.
    pub fn build_is_current(&self, key: &IndexKey) -> bool {
        self.last_build.as_ref().is_some_and(|b| b.hash == key.hash && b.builder_version == key.builder_version)
    }

    /// Run a query through the real search: the frame features are the pose of
    /// the clip at the time (velocities from a step back), the trajectory is a constant
    /// desired velocity. The record is kept for the timeline.
    pub fn run_query(
        &mut self,
        skeleton: &Skeleton,
        clip: &CompiledClip,
        time: f32,
        desired_velocity: [f32; 3],
        current: Option<(u32, f32)>,
    ) -> Result<QueryRecord, String> {
        let (index, clips) = self.index.as_ref().ok_or("build the index first")?;
        let runtime = RuntimeSkeleton::new("posedb", skeleton).map_err(|e| e.to_string())?;
        let dt = 1.0 / 30.0;
        let pose = clip.sample(&runtime, time);
        let previous = clip.sample(&runtime, (time - dt).max(0.0));
        let frame = FrameFeatures::from_pose(&runtime, &pose, Some(&previous), dt, 0.0);
        let desired = DesiredTrajectory::from_array(desired_velocity);
        let query = index.query_vector(self.doc.value(), &runtime, &frame, &desired);
        let result = index.search(clips, &query, current, 0.0, &self.doc.value().search).ok_or("the index is empty")?;
        let record = QueryRecord {
            desired_speed: desired_velocity.iter().map(|v| v * v).sum::<f32>().sqrt(),
            source: clips[result.clip as usize].source_id.clone(),
            result,
        };
        let keep = self.asset().search.query_history.max(1) as usize;
        self.queries.push(record.clone());
        if self.queries.len() > keep {
            self.queries.remove(0);
        }
        Ok(record)
    }

    pub fn select_result(&mut self, index: usize) -> bool {
        if index < self.queries.len() {
            self.selection = AnimSelection::PoseResult { index };
            true
        } else {
            false
        }
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

    pub fn validation(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if let Err(m) = self.asset().validate_structure() {
            out.push(Diagnostic::error("posedb.invalid", m));
        }
        out.extend(self.asset().reference_diagnostics());
        if let Some(sk) = &self.skeleton {
            out.extend(self.asset().diagnostics(sk));
        }
        out.extend(self.build_diagnostics.iter().cloned());
        out
    }

    pub fn save(&mut self) -> anyhow::Result<super::document::SaveOutcome> {
        self.doc.save()
    }
}


#[cfg(test)]
mod tests {
    use super::super::ik_rig_model::tests::skeleton;
    use super::super::store::MemStore;
    use super::*;
    use std::sync::Arc as StdArc;

    fn model() -> (PoseDbModel, MemStore) {
        let store = MemStore::default();
        let mut m = PoseDbModel::open(StdArc::new(store.clone()), Path::new("d.posedb"), &PoseDbAsset::new("DB", None).to_text().unwrap()).unwrap();
        m.skeleton = Some(skeleton());
        (m, store)
    }

    fn seq() -> AssetRef<AnimationSequenceAssetKind> {
        AssetRef::new()
    }

    #[test]
    fn sources_are_unique_flagged_excluded_and_deleted() {
        let (mut m, _) = model();
        let s = seq();
        let id = m.add_sequence_source(s.clone()).unwrap();
        assert!(m.add_sequence_source(s).is_err(), "the same asset twice is refused");
        let bs = m.add_blend_space_source(AssetRef::new()).unwrap();
        assert!(m.set_source_flags(&id, true, true, true) && !m.set_source_flags(&id, true, true, true));
        assert!(m.set_source_bias(&id, f32::NAN).is_err());
        m.set_source_bias(&id, 0.25).unwrap();
        assert!(m.add_exclusion(&id, 1.0, 0.5).is_err());
        m.add_exclusion(&id, 0.2, 0.4).unwrap();
        assert!(m.remove_exclusion(&id, 0) && !m.remove_exclusion(&id, 0));
        assert!(m.replace_source_asset(&bs, PoseSource::Sequence { sequence: seq() }).is_ok());
        assert!(m.delete_source(&id) && m.asset().sources.len() == 1);
        assert!(m.asset().validate_structure().is_ok());
    }

    #[test]
    fn channels_are_validated_and_define_the_feature_schema() {
        let (mut m, _) = model();
        assert!(m.add_bone_channel("Foot", "ghost", true, 1.0).is_err());
        assert!(m.add_bone_channel("Foot", "foot", true, -1.0).is_err());
        let foot = m.add_bone_channel("Foot", "foot", true, 1.0).unwrap();
        assert!(m.add_bone_channel("Foot", "foot", false, 1.0).is_err(), "names are unique");
        m.add_rotation_channel("ShinRot", "shin", 0.5).unwrap();
        assert!(m.add_trajectory_channel("Traj", vec![], 1.0).is_err());
        let traj = m.add_trajectory_channel("Traj", vec![0.4, 0.2, 0.4, -0.2], 2.0).unwrap();
        match &m.asset().channels[traj].kind {
            ChannelKind::Trajectory { offsets, .. } => assert_eq!(offsets, &[-0.2, 0.2, 0.4], "sorted and unique"),
            other => panic!("{other:?}"),
        }
        assert!(m.set_trajectory(traj, vec![0.5], false, false, false).is_err());
        m.set_trajectory(traj, vec![0.5, 1.0], true, false, true).unwrap();
        assert!(m.set_trajectory(foot, vec![0.5], true, true, true).is_err(), "not a trajectory");
        m.add_curve_channel("Speed", "speed", 1.0).unwrap();
        m.add_phase_channel(1.0).unwrap();
        let dims = m.feature_dimensions();
        assert!(dims > 0 && m.schema().len() == 5);
        m.set_channel_weight(foot, 3.0).unwrap();
        assert!(m.set_channel_weight(foot, -1.0).is_err() && m.set_channel_weight(99, 1.0).is_err());
        assert!(m.set_channel_normalization(foot, Normalization::None));
        assert!(m.delete_channel(foot));
        assert_eq!(m.asset().channels.len(), 4);
    }

    #[test]
    fn sampling_and_search_settings_are_validated() {
        let (mut m, _) = model();
        assert!(m.set_sampling(0.0, 0.1).is_err() && m.set_sampling(0.05, -1.0).is_err());
        m.set_sampling(0.05, 0.2).unwrap();
        assert!(m.set_search(SearchSettings { candidate_count: 0, ..SearchSettings::default() }).is_err());
        m.set_search(SearchSettings { candidate_count: 3, continuing_pose_bias: 0.1, blend_time: 0.3, query_history: 8 }).unwrap();
        assert_eq!((m.asset().sampling.interval, m.asset().search.candidate_count), (0.05, 3));
    }

    #[test]
    fn index_status_distinguishes_missing_stale_corrupt_and_current() {
        let (mut m, _) = model();
        m.add_sequence_source(seq()).unwrap();
        let hashes = vec![("a".to_string(), "h1".to_string())];
        let key = m.index_key(&hashes);
        assert_eq!(m.index_status(None, &key), IndexStatus::Missing);
        let bytes = skeletal_assets::pose_db::encode_index(&key, b"payload");
        assert_eq!(m.index_status(Some(&bytes), &key), IndexStatus::Current);
        let changed = m.index_key(&[("a".to_string(), "h2".to_string())]);
        assert!(matches!(m.index_status(Some(&bytes), &changed), IndexStatus::Stale { .. }), "a changed source makes the cache stale");
        assert_eq!(m.index_status(Some(b"nope"), &key), IndexStatus::Corrupt);
        let before = m.index_key(&hashes);
        m.set_search(SearchSettings { blend_time: 0.9, ..SearchSettings::default() }).unwrap();
        assert_eq!(m.index_key(&hashes), before, "runtime settings do not invalidate the index");
        m.add_phase_channel(1.0).unwrap();
        assert_ne!(m.index_key(&hashes), before, "the feature schema does");
    }

    #[test]
    fn build_reports_diagnostics_and_a_successful_build_answers_queries() {
        let (mut m, _) = model();
        let sk = skeleton();
        let key = m.index_key(&[]);
        let diagnostics = m.build_index(&sk, vec![], &key).unwrap_err();
        assert!(diagnostics.iter().any(|d| d.code == "posedb.no_channels") && diagnostics.iter().any(|d| d.code == "posedb.no_sources"));
        assert!(m.validation().iter().any(|d| d.code == "posedb.no_channels"), "build diagnostics show with the rest");

        m.add_bone_channel("Foot", "foot", true, 1.0).unwrap();
        m.add_trajectory_channel("Traj", vec![0.2, 0.4], 1.0).unwrap();
        let clip = |name: &str| SearchClip {
            source_id: name.into(),
            clip: StdArc::new(CompiledClip::placeholder(name, 2.0)),
            mirrored: false,
            looping: false,
            exclusions: vec![],
            cost_bias: 0.0,
        };
        let key = m.index_key(&[]);
        let info = m.build_index(&sk, vec![clip("walk"), clip("idle")], &key).unwrap();
        assert!(info.entries > 0 && info.dims == m.feature_dimensions());
        assert!(m.build_diagnostics.is_empty() && m.build_is_current(&key));
        assert!(!m.build_is_current(&m.index_key(&[("x".into(), "y".into())])));
        assert!(m.index_payload().is_some());
        let reference = CompiledClip::placeholder("ref", 2.0);
        let record = m.run_query(&sk, &reference, 0.5, [0.0, 0.0, 2.0], None).unwrap();
        assert!(record.result.cost.is_finite() && !record.result.breakdown.is_empty());
        assert!(["walk", "idle"].contains(&record.source.as_str()));
        assert_eq!(m.queries.len(), 1);
        assert!(m.select_result(0) && !m.select_result(5));
    }

    #[test]
    fn query_history_is_capped_and_needs_a_build() {
        let (mut m, _) = model();
        let sk = skeleton();
        let reference = CompiledClip::placeholder("ref", 1.0);
        assert!(m.run_query(&sk, &reference, 0.0, [0.0; 3], None).is_err());
        m.add_bone_channel("Foot", "foot", true, 1.0).unwrap();
        m.set_search(SearchSettings { query_history: 2, ..SearchSettings::default() }).unwrap();
        let key = m.index_key(&[]);
        m.build_index(&sk, vec![SearchClip { source_id: "a".into(), clip: StdArc::new(CompiledClip::placeholder("a", 1.0)), mirrored: false, looping: false, exclusions: vec![], cost_bias: 0.0 }], &key).unwrap();
        for _ in 0..4 {
            m.run_query(&sk, &reference, 0.0, [0.0; 3], None).unwrap();
        }
        assert_eq!(m.queries.len(), 2);
    }

    #[test]
    fn authored_data_round_trips_and_build_state_does_not() {
        let (mut m, store) = model();
        let s = seq();
        let id = m.add_sequence_source(s.clone()).unwrap();
        m.set_source_flags(&id, true, false, true);
        m.add_exclusion(&id, 0.1, 0.2).unwrap();
        m.add_trajectory_channel("Traj", vec![0.2, 0.6], 1.5).unwrap();
        m.set_sampling(0.1, 0.3).unwrap();
        m.last_build = Some(BuildInfo { builder_version: 1, hash: "abc".into(), entries: 3, dims: 4, clips: 1 });
        m.save().unwrap();
        let text = store.text("d.posedb").unwrap();
        assert!(!text.contains("abc"), "the build hash is derived, not stored");
        let r = PoseDbModel::open(StdArc::new(store), Path::new("d.posedb"), "").unwrap();
        let src = &r.asset().sources[0];
        assert_eq!((src.source.clone(), src.looping, src.exclusions.len()), (PoseSource::Sequence { sequence: s }, true, 1));
        assert_eq!(r.asset().channels[0].weight, 1.5);
        assert_eq!(r.asset().sampling.interval, 0.1);
        assert!(r.last_build.is_none());
    }
}

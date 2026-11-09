//! Building a Pose Search Database index off the UI thread.
//!
//! [`BuildJob`] owns everything a build reads (the authored database, the
//! skeleton and a loader for source assets), so it can run on a background
//! thread while the editor stays responsive. [`BuildHandle`] is the editor's
//! side of it: progress to display and a cancel flag the builder polls between
//! poses, so cancelling stops the work rather than hiding it.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use pulsar_reflection::{AnimationSequenceAssetKind, AssetKind, BlendSpaceAssetKind};
use skeletal_assets::assets::AnimSeqAsset;
use skeletal_assets::blend_space::BlendSpaceAsset;
use skeletal_assets::diagnostics::Diagnostic;
use skeletal_assets::pose_db::{PoseDbAsset, PoseSource};
use skeletal_assets::runtime::clip::CompiledClip;
use skeletal_assets::runtime::pose::RuntimeSkeleton;
use skeletal_assets::runtime::posesearch::{MirrorTable, PoseIndex, SearchClip};
use skeletal_assets::skeleton::Skeleton;

use super::refs::ProjectRefIndex;

/// Where source assets come from.
pub trait SourceLoader: Send + Sync {
    fn sequence(&self, id: &str) -> Option<AnimSeqAsset>;
    fn blend_space(&self, id: &str) -> Option<BlendSpaceAsset>;
}

/// The project on disk.
pub struct ProjectSources {
    pub root: std::path::PathBuf,
}

impl ProjectSources {
    fn read(&self, kind: &str, id: &str) -> Option<String> {
        let path = ProjectRefIndex::new(&self.root).locate_path(kind, id)?;
        String::from_utf8(engine_fs::virtual_fs::read_file(&path).ok()?).ok()
    }
}

impl SourceLoader for ProjectSources {
    fn sequence(&self, id: &str) -> Option<AnimSeqAsset> {
        AnimSeqAsset::parse(&self.read(AnimationSequenceAssetKind::KIND, id)?).ok()
    }
    fn blend_space(&self, id: &str) -> Option<BlendSpaceAsset> {
        BlendSpaceAsset::parse(&self.read(BlendSpaceAssetKind::KIND, id)?).ok()
    }
}

pub struct BuildJob {
    pub asset: PoseDbAsset,
    pub skeleton: Skeleton,
    pub sources: Arc<dyn SourceLoader>,
}

pub struct BuildOutput {
    pub index: PoseIndex,
    pub clips: Vec<SearchClip>,
    /// Sources that could not be read and were left out.
    pub missing: Vec<String>,
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

impl BuildJob {
    /// Compile every enabled source into search clips (a blend space expands to
    /// its samples) and run the real index builder. `progress` receives
    /// `(done, expected)` poses and returns `false` to cancel.
    pub fn run(&self, progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<BuildOutput, Vec<Diagnostic>> {
        let runtime = RuntimeSkeleton::new("posedb", &self.skeleton).map_err(|e| vec![Diagnostic::error("posedb.skeleton", e)])?;
        let mut clips = Vec::new();
        let mut missing = Vec::new();
        for s in self.asset.sources.iter().filter(|s| s.enabled) {
            let mut add = |source_id: String, asset: AnimSeqAsset, clips: &mut Vec<SearchClip>| {
                clips.push(SearchClip { source_id, clip: Arc::new(CompiledClip::compile(&asset, &runtime)), mirrored: s.mirrored, looping: s.looping, exclusions: s.exclusions.clone(), cost_bias: s.cost_bias });
            };
            match &s.source {
                PoseSource::Sequence { sequence } => match self.sources.sequence(sequence.as_str()) {
                    Some(a) => add(s.id.clone(), a, &mut clips),
                    None => missing.push(format!("Sequence {}", short(sequence.as_str()))),
                },
                PoseSource::BlendSpace { blend_space } => match self.sources.blend_space(blend_space.as_str()) {
                    Some(space) => {
                        for sample in &space.samples {
                            match sample.sequence.as_ref().and_then(|q| self.sources.sequence(q.as_str())) {
                                Some(a) => add(format!("{}:{}", s.id, sample.id), a, &mut clips),
                                None => missing.push(format!("a sample of Blend Space {}", short(blend_space.as_str()))),
                            }
                        }
                    }
                    None => missing.push(format!("Blend Space {}", short(blend_space.as_str()))),
                },
            }
        }
        let mirror = clips.iter().any(|c| c.mirrored).then(|| MirrorTable::from_names(&runtime));
        let index = PoseIndex::build_with(&self.asset, &runtime, &clips, mirror.as_ref(), progress)?;
        Ok(BuildOutput { index, clips, missing })
    }
}

/// The editor's side of a running build.
#[derive(Clone, Default)]
pub struct BuildHandle {
    done: Arc<AtomicUsize>,
    expected: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
}

impl BuildHandle {
    /// The callback to give [`BuildJob::run`].
    pub fn reporter(&self) -> impl FnMut(usize, usize) -> bool + Send + 'static {
        let (done, expected, cancel) = (self.done.clone(), self.expected.clone(), self.cancel.clone());
        move |d, e| {
            done.store(d, Ordering::Relaxed);
            expected.store(e, Ordering::Relaxed);
            !cancel.load(Ordering::Relaxed)
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Fraction done in `[0, 1]`, or `None` before the first pose.
    pub fn fraction(&self) -> Option<f32> {
        let expected = self.expected.load(Ordering::Relaxed);
        (expected > 0).then(|| (self.done.load(Ordering::Relaxed) as f32 / expected as f32).clamp(0.0, 1.0))
    }
}

pub fn is_cancelled(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.code == "posedb.cancelled")
}

#[cfg(test)]
mod tests;

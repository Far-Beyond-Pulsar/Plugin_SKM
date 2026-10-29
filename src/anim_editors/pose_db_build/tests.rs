use super::*;
use crate::core::sample;
use skeletal_assets::assets::SkeletonAsset;
use skeletal_assets::pose_db::{ChannelKind, FeatureChannel, Normalization, PoseDbSource};

struct Mem(Vec<Arc<AnimSeqAsset>>);

impl SourceLoader for Mem {
    fn sequence(&self, id: &str) -> Option<AnimSeqAsset> {
        self.0.iter().find(|s| s.id.as_str() == id).map(|s| (**s).clone())
    }
    fn blend_space(&self, _: &str) -> Option<BlendSpaceAsset> {
        None
    }
}

fn job(sequences: usize) -> BuildJob {
    let skeleton_asset = SkeletonAsset::new(sample::humanoid_skeleton());
    let skeleton = skeleton_asset.skeleton();
    let hand = skeleton.bones.iter().find(|b| b.name.to_lowercase().contains("hand")).unwrap().id.clone();
    let seqs: Vec<Arc<AnimSeqAsset>> = (0..sequences).map(|_| Arc::new(AnimSeqAsset::new(Some(skeleton_asset.id.clone()), sample::run_animation(&skeleton)))).collect();
    let mut asset = PoseDbAsset::new("Locomotion", skeleton_asset.id.to_ref());
    for (i, s) in seqs.iter().enumerate() {
        asset.sources.push(PoseDbSource { id: format!("s{i}"), source: PoseSource::Sequence { sequence: s.id.to_ref() }, enabled: true, mirrored: false, looping: true, exclusions: vec![], cost_bias: 0.0 });
    }
    asset.channels.push(FeatureChannel { id: "c0".into(), name: "Hand".into(), weight: 1.0, normalization: Normalization::Standardize, kind: ChannelKind::BonePosition { bone: hand } });
    BuildJob { asset, skeleton, sources: Arc::new(Mem(seqs)) }
}

#[test]
fn a_build_reports_progress_up_to_the_expected_count() {
    let job = job(2);
    let handle = BuildHandle::default();
    let mut calls = 0usize;
    let mut last = (0, 0);
    let mut reporter = handle.reporter();
    let out = job
        .run(&mut |d, e| {
            calls += 1;
            last = (d, e);
            reporter(d, e)
        })
        .expect("builds");
    assert!(calls > 10 && last.0 > 0 && last.0 <= last.1, "progress ran: {calls} {last:?}");
    assert_eq!(handle.fraction().map(|f| f > 0.5), Some(true));
    assert_eq!(out.clips.len(), 2);
    assert!(out.missing.is_empty() && out.index.entries.len() > 10);
}

#[test]
fn cancelling_stops_the_build_instead_of_finishing_it() {
    let job = job(3);
    let handle = BuildHandle::default();
    let mut reporter = handle.reporter();
    let watcher = handle.clone();
    let mut seen = 0usize;
    let result = job.run(&mut |d, e| {
        seen = d;
        if d == 5 {
            watcher.cancel();
        }
        reporter(d, e)
    });
    let diagnostics = result.err().expect("a cancelled build produces no index");
    assert!(is_cancelled(&diagnostics));
    assert_eq!(seen, 5, "no pose was visited after the cancel");
    assert!(handle.is_cancelled());
}

#[test]
fn a_build_runs_on_another_thread_while_the_caller_polls_progress() {
    let job = job(2);
    let handle = BuildHandle::default();
    let mut reporter = handle.reporter();
    let thread = std::thread::spawn(move || job.run(&mut reporter).map(|o| o.index.entries.len()));
    let mut polled = false;
    while !thread.is_finished() {
        polled |= handle.fraction().is_some();
        std::thread::yield_now();
    }
    assert!(thread.join().unwrap().unwrap() > 10);
    assert!(handle.fraction().is_some() || polled);
}

#[test]
fn unreadable_sources_are_named_and_left_out() {
    let mut job = job(1);
    job.asset.sources.push(PoseDbSource {
        id: "ghost".into(),
        source: PoseSource::Sequence { sequence: pulsar_reflection::AssetRef::new() },
        enabled: true,
        mirrored: false,
        looping: false,
        exclusions: vec![],
        cost_bias: 0.0,
    });
    let out = job.run(&mut |_, _| true).unwrap();
    assert_eq!(out.clips.len(), 1);
    assert_eq!(out.missing.len(), 1);
    job.asset.sources.clear();
    assert!(job.run(&mut |_, _| true).err().unwrap().iter().any(|d| d.code == "posedb.no_sources"));
}

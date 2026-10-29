use super::*;
use crate::core::sample;
use skeletal_assets::blend_space::BlendSample;
use skeletal_assets::montage::{MontageSegment, SlotTrack};

struct Mem {
    skeleton: Arc<SkeletonAsset>,
    sequences: Vec<Arc<AnimSeqAsset>>,
}

impl AssetSource for Mem {
    fn skeleton(&self, id: &str) -> Option<Arc<SkeletonAsset>> {
        (self.skeleton.id.as_str() == id).then(|| self.skeleton.clone())
    }
    fn sequence(&self, id: &str) -> Option<Arc<AnimSeqAsset>> {
        self.sequences.iter().find(|s| s.id.as_str() == id).cloned()
    }
    fn blend_space(&self, _: &str) -> Option<Arc<BlendSpaceAsset>> {
        None
    }
    fn montage(&self, _: &str) -> Option<Arc<AnimationMontageAsset>> {
        None
    }
    fn graph(&self, _: &str) -> Option<Arc<AnimationGraphAsset>> {
        None
    }
    fn ik_rig(&self, _: &str) -> Option<Arc<IkRigAsset>> {
        None
    }
    fn retarget_profile(&self, _: &str) -> Option<Arc<RetargetProfileAsset>> {
        None
    }
    fn ik_retargeter(&self, _: &str) -> Option<Arc<IkRetargetAsset>> {
        None
    }
    fn pose_db(&self, _: &str) -> Option<Arc<PoseDbAsset>> {
        None
    }
}

struct World {
    mem: Mem,
    rig: Rig,
    run: Arc<AnimSeqAsset>,
    idle: Arc<AnimSeqAsset>,
}

fn world() -> World {
    let skeleton = Arc::new(SkeletonAsset::new(sample::humanoid_skeleton()));
    let run = Arc::new(AnimSeqAsset::new(Some(skeleton.id.clone()), sample::run_animation(&skeleton.skeleton())));
    let idle = Arc::new(AnimSeqAsset::new(Some(skeleton.id.clone()), crate::core::AnimationClip::bind_pose()));
    let mesh = sample::sample_mesh(&skeleton);
    let rig = Rig::new(&skeleton, Some(mesh)).unwrap();
    World { mem: Mem { skeleton, sequences: vec![run.clone(), idle.clone()] }, rig, run, idle }
}

fn montage(w: &World) -> AnimationMontageAsset {
    let mut asset = AnimationMontageAsset::default();
    asset.skeleton = Some(w.rig.skeleton_ref.clone());
    asset.blend_in.duration = 0.0;
    asset.blend_out.duration = 0.0;
    let mut track = SlotTrack::new("UpperBody", "DefaultGroup");
    track.segments.push(MontageSegment::new(Some(w.run.id.to_ref()), 0.0));
    asset.tracks.push(track);
    asset
}

fn moved(a: &HashMap<String, Mat4>, b: &HashMap<String, Mat4>) -> f32 {
    a.iter().map(|(id, m)| {
        let p = m.transform_point(Vec3::ZERO);
        let q = b[id].transform_point(Vec3::ZERO);
        (p.x - q.x).abs() + (p.y - q.y).abs() + (p.z - q.z).abs()
    }).sum()
}

#[test]
fn a_montage_plays_on_the_runtime_and_scrubbing_reports_nothing() {
    let w = world();
    let asset = montage(&w);
    let mut preview = MontagePreview::build(&asset, &w.rig, &w.mem).expect("compiles");
    assert!(preview.length > 0.2);
    let start = world_of(&w.rig, &preview.pose_at(0.0));
    let later = world_of(&w.rig, &preview.pose_at(preview.length * 0.4));
    assert!(moved(&start, &later) > 0.05, "the posed skeleton follows the Montage");
    // scrubbing back replays and lands on the same pose
    let again = world_of(&w.rig, &preview.pose_at(0.0));
    assert!(moved(&start, &again) < 1e-4);
    let forward = world_of(&w.rig, &preview.pose_at(preview.length * 0.4));
    assert!(moved(&later, &forward) < 1e-3, "the same time gives the same pose");
    let report = &preview.instance().report;
    assert!(report.montage_events.is_empty() && report.spans.is_empty() && report.montage_timelines.is_empty(), "scrub dispatches no gameplay events");
}

#[test]
fn a_montage_without_a_playable_track_is_refused_with_a_reason() {
    let w = world();
    let mut asset = montage(&w);
    asset.tracks.clear();
    assert_eq!(MontagePreview::build(&asset, &w.rig, &w.mem).err().unwrap()[0].code, "preview.no_tracks");
    let mut asset = montage(&w);
    asset.tracks[0].segments[0].sequence = None;
    assert!(MontagePreview::build(&asset, &w.rig, &w.mem).is_err(), "an empty segment cannot play");
}

#[test]
fn a_blend_space_plays_at_its_input_through_the_player_node() {
    let w = world();
    let mut space = BlendSpaceAsset::default();
    space.skeleton = Some(w.rig.skeleton_ref.clone());
    space.x_axis.min = 0.0;
    space.x_axis.max = 1.0;
    space.samples.push(BlendSample::new(Some(w.idle.id.to_ref()), 0.0, None));
    space.samples.push(BlendSample::new(Some(w.run.id.to_ref()), 1.0, None));
    let space = Arc::new(space);
    let overlay = OverlaySource { base: &w.mem, blend_space: Some(space.clone()), montage: None };
    let mut preview = BlendSpacePreview::build(&space, &w.rig, &overlay).expect("compiles with the unsaved space");
    preview.set_input(0.0, 0.0);
    let idle = world_of(&w.rig, &preview.advance(0.2));
    preview.set_input(1.0, 0.0);
    let run = world_of(&w.rig, &preview.advance(0.2));
    assert!(moved(&idle, &run) > 0.02, "moving the input changes the pose");
    assert!(preview.instance().report.spans.is_empty(), "scrub reports no playback spans");
}

#[test]
fn the_posed_mesh_is_drawn_skinned_when_a_gpu_exists() {
    let Some(mut renderer) = SkinRenderer::new() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let w = world();
    let asset = montage(&w);
    let mut preview = MontagePreview::build(&asset, &w.rig, &w.mem).unwrap();
    let palette = crate::style::Palette::test_dark();
    let a = renderer.render(&w.rig, &preview.pose_at(0.0), &palette, 320, 260);
    let b = renderer.render(&w.rig, &preview.pose_at(preview.length * 0.5), &palette, 320, 260);
    assert!(crate::rendering::preview::coverage(&a.rgba) > 0.02, "something is drawn");
    assert_ne!(a.rgba, b.rgba, "a different time draws a different pose");
    crate::rendering::preview::save("montage_preview_start", 320, 260, &a.rgba);
    crate::rendering::preview::save("montage_preview_mid", 320, 260, &b.rgba);
}

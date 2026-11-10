//! A project on disk for the headless wiring tests: a skeleton, a sequence, an
//! Animation Graph playing it and a Montage, all registered in the file index
//! by stable id, plus the process-global engine context and project path the
//! plugin's runtime reads.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Mutex;

use pulsar_reflection::{
    AnimationGraphAssetKind, AnimationMontageAssetKind, AnimationSequenceAssetKind,
    SkeletonAssetKind,
};
use serde_json::json;
use skeletal_assets::anim_graph::builder::GraphBuilder;
use skeletal_assets::anim_graph::AnimationGraphAsset;
use skeletal_assets::animation::{AnimationClip, BoneTrack, Keyframe};
use skeletal_assets::assets::{AnimSeqAsset, SkeletonAsset};
use skeletal_assets::math::{Transform as AssetTransform, Vec3 as AssetVec3};
use skeletal_assets::montage::{AnimationMontageAsset, MontageSegment, SlotTrack};

/// The engine context and the project path are process globals: tests that
/// open a project take turns.
pub static PROJECT: Mutex<()> = Mutex::new(());

pub struct Project {
    pub _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub skeleton_id: String,
    pub sequence_id: String,
    pub sequence_path: PathBuf,
    pub graph_id: String,
    pub graph_path: PathBuf,
    pub montage_id: String,
    pub montage_path: PathBuf,
    pub index: std::sync::Arc<engine_fs::ProjectFileIndex>,
}

fn slide(bone: &str, to: [f32; 3], duration: f32) -> BoneTrack {
    let at = |p: [f32; 3]| AssetTransform {
        translation: AssetVec3::new(p[0], p[1], p[2]),
        ..AssetTransform::IDENTITY
    };
    BoneTrack {
        bone_id: bone.into(),
        keyframes: vec![
            Keyframe::linear(0.0, at([0.0; 3])),
            Keyframe::linear(duration, at(to)),
        ],
    }
}

/// The 2 s sequence the project starts with: b1 slides `x` metres along x.
pub fn sequence_text(skeleton: &SkeletonAsset, id: Option<&str>, x: f32) -> (String, String) {
    let mut walk = AnimSeqAsset::new(
        None,
        AnimationClip {
            name: "Walk".into(),
            duration: 2.0,
            fps: 30.0,
            tracks: vec![slide("b0", [0.0; 3], 2.0), slide("b1", [x, 0.0, 0.0], 2.0)],
        },
    );
    walk.skeleton = Some(skeletal_assets::ids::SkeletonId::parse(skeleton.id.as_str()).unwrap());
    if let Some(id) = id {
        walk.id = skeletal_assets::ids::AnimSequenceId::parse(id).unwrap();
    }
    (walk.id.as_str().to_string(), walk.to_text())
}

/// Open a project, point the engine at it and write its assets.
pub fn open_project() -> Project {
    use pulsar_reflection::AssetKind as _;
    static CONTEXT: std::sync::Once = std::sync::Once::new();
    CONTEXT.call_once(|| engine_state::EngineContext::new().set_global());
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    engine_state::set_project_path(root.to_string_lossy().into_owned());
    let index = engine_fs::open_project_file_index(&root)
        .unwrap()
        .expect("local index");
    let write = |name: &str, text: String| {
        let path = root.join(name);
        std::fs::write(&path, text).unwrap();
        index.refresh_path(&path).unwrap();
        path
    };
    let bones: Vec<_> = (0..3)
        .map(|i| skeletal_assets::skeleton::Bone {
            id: format!("b{i}"),
            name: format!("b{i}"),
            parent: (i > 0).then(|| format!("b{}", i - 1)),
            bind_transform: skeletal_assets::math::Transform::IDENTITY,
        })
        .collect();
    let skeleton = SkeletonAsset::new(skeletal_assets::skeleton::Skeleton { bones });
    let skeleton_id = skeleton.id.as_str().to_string();
    let skeleton_path = write("body.skeleton", skeleton.to_text());
    index
        .record_typed_asset_id(SkeletonAssetKind::KIND, &skeleton_id, &skeleton_path)
        .unwrap();

    let (sequence_id, text) = sequence_text(&skeleton, None, 4.0);
    let sequence_path = write("walk.animseq", text);
    index
        .record_typed_asset_id(
            AnimationSequenceAssetKind::KIND,
            &sequence_id,
            &sequence_path,
        )
        .unwrap();

    let mut graph = AnimationGraphAsset::default();
    graph.skeleton = Some(
        skeletal_assets::ids::SkeletonId::parse(&skeleton_id)
            .unwrap()
            .to_ref(),
    );
    let mut b = GraphBuilder::new("main", &[]);
    b.node("out", "anim.output_pose", &[]);
    b.node(
        "p",
        "anim.sequence_player",
        &[("sequence", json!(sequence_id))],
    );
    b.node("slot", "anim.slot", &[("slot", json!("Body"))]);
    b.wire("p", "slot", "source").wire("slot", "out", "pose");
    graph.main = b.into_graph();
    let graph_id = graph.id.as_str().to_string();
    let graph_path = write("locomotion.animgraph", graph.to_text().unwrap());
    index
        .record_typed_asset_id(AnimationGraphAssetKind::KIND, &graph_id, &graph_path)
        .unwrap();

    let mut montage = AnimationMontageAsset::default();
    let mut track = SlotTrack::new("Body", "DefaultGroup");
    track.segments.push(MontageSegment::new(
        pulsar_reflection::AssetRef::parse(&sequence_id),
        0.0,
    ));
    montage.tracks.push(track);
    let montage_id = montage.id.as_str().to_string();
    let montage_path = write("swing.montage", montage.to_text().unwrap());
    index
        .record_typed_asset_id(AnimationMontageAssetKind::KIND, &montage_id, &montage_path)
        .unwrap();

    Project {
        _dir: dir,
        root,
        skeleton_id,
        sequence_id,
        sequence_path,
        graph_id,
        graph_path,
        montage_id,
        montage_path,
        index,
    }
}

/// The engine's own frame loop over a world: each tick runs the animation
/// phase, the schedule, the script/event flushes and the asset pump, as a game
/// does. Fixed 0.1 s steps.
pub struct Game {
    pub tick_loop: pulsar_game::prelude::TickLoop,
}

pub const STEP: f32 = 0.1;

impl Game {
    pub fn new() -> Self {
        use pulsar_game::prelude::{TickLoop, TickMode};
        Self {
            tick_loop: TickLoop::new(
                TickMode::Fixed {
                    dt: std::time::Duration::from_secs_f32(STEP),
                },
                0,
            ),
        }
    }

    pub fn tick(&mut self, frames: usize) {
        for _ in 0..frames {
            self.tick_loop.tick_once();
        }
    }

    /// Run `f` on the world (the shared scene's write lock).
    pub fn with<R>(&self, f: impl FnOnce(&mut pulsar_scenedb::World) -> R) -> R {
        f(&mut self.tick_loop.scene_store.write().world)
    }

    /// Tick until `ready` holds (assets load across frames), at most 100 frames.
    pub fn tick_until(&mut self, what: &str, ready: impl Fn(&mut pulsar_scenedb::World) -> bool) {
        for _ in 0..100 {
            if self.with(&ready) {
                return;
            }
            self.tick(1);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("never became ready: {what}");
    }
}

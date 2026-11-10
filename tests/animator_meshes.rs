//! One animator drives every skinned mesh of its owner that is bound to its
//! skeleton (Plugin_SKM#13), through the real wiring: the real meshes plugin's
//! `SkinnedMeshAccess` (`SkeletalMeshComponent`), the engine's own frame loop
//! (`TickLoop`: the animation phase, the flushes, the asset pump) and an
//! animator that loads its graph and skeleton from a project on disk.
//! Headless: no window and no GPU.
//!
//! What a mesh "shows" is read where the renderer reads it: the joint palette
//! the meshes plugin derives from the pose (`bone_palette`).

mod common;

// The plugins as a game links them: their roots, with every registration.
use meshes_plugin as _;
use skeletal_animation_plugin as _;

use std::sync::Arc;

use animation_component::AnimationComponent;
use common::{open_project, Game, Project, PROJECT};
use meshes_component::components::SkeletalMeshComponent;
use pulsar_reflection::{AssetRef, SkeletonAssetKind};
use pulsar_scenedb::{Entity, World};

type Palette = Vec<[[f32; 4]; 4]>;

/// A skinned mesh bound to the project's skeleton (or to none: a prop), with
/// one joint following bone `b1`, so its palette is that bone's pose.
fn attach_mesh(world: &mut World, owner: Entity, skeleton: Option<&str>) -> Entity {
    let mesh = pulsar_world_registry::attach_value(world, owner, SkeletalMeshComponent::default())
        .expect("the meshes plugin registers the component");
    let mut asset = skeletal_assets::skmesh::SkeletalMeshAsset::new(None);
    asset.joints = vec![skeletal_assets::skmesh::Joint {
        bone: "b1".into(),
        inverse_bind: skeletal_assets::math::Mat4::IDENTITY.0,
    }];
    let mut component = world.get_mut::<SkeletalMeshComponent>(mesh).unwrap();
    component.loaded_mesh = Some(Arc::new(asset));
    component.mesh_skeleton = skeleton.and_then(AssetRef::<SkeletonAssetKind>::parse);
    mesh
}

fn attach_animator(world: &mut World, owner: Entity, project: &Project, rate: f32) -> Entity {
    let mut animator = AnimationComponent::default();
    animator.animation_graph = AssetRef::parse(&project.graph_id);
    animator.playing = true;
    animator.playback_rate = rate;
    pulsar_world_registry::attach_value(world, owner, animator).unwrap()
}

fn palette(game: &Game, mesh: Entity) -> Palette {
    game.with(|w| {
        w.get::<SkeletalMeshComponent>(mesh)
            .unwrap()
            .bone_palette
            .clone()
    })
}

/// The pose the animator drew, as its one joint sees it (b1's matrix).
fn animator_pose(game: &Game, animator: Entity) -> Palette {
    game.with(|w| {
        let a = w.get::<AnimationComponent>(animator).unwrap();
        a.bone_world_pose
            .get(1)
            .map(|m| vec![*m])
            .unwrap_or_default()
    })
}

fn x(palette: &Palette) -> f32 {
    palette[0][3][0]
}

/// Frames until every animator has followed the skeleton of the mesh it drives,
/// loaded it and drawn a pose.
fn settle(game: &mut Game, animators: &[Entity]) {
    let animators = animators.to_vec();
    game.tick_until("the animators draw a pose", move |w| {
        animators.iter().all(|a| {
            w.get::<AnimationComponent>(*a)
                .is_some_and(|a| a.bone_world_pose.len() == 3)
        })
    });
}

#[test]
fn one_animator_drives_every_mesh_on_its_skeleton_and_a_late_mesh_gets_the_pose() {
    let _guard = PROJECT.lock().unwrap_or_else(|e| e.into_inner());
    let project = open_project();
    let mut game = Game::new();
    // a modular character: body, head and gear on one skeleton, and a prop
    // (a skinned mesh with no skeleton) that nothing animates
    let (owner, animator, hero, prop) = game.with(|w| {
        let owner = w.spawn();
        let animator = attach_animator(w, owner, &project, 1.0);
        let hero: Vec<Entity> = (0..3)
            .map(|_| attach_mesh(w, owner, Some(&project.skeleton_id)))
            .collect();
        let prop = attach_mesh(w, owner, None);
        (owner, animator, hero, prop)
    });
    settle(&mut game, &[animator]);
    game.tick(3);

    let pose = animator_pose(&game, animator);
    assert!(x(&pose) > 0.05, "the animation moved b1: {}", x(&pose));
    for mesh in &hero {
        assert_eq!(
            palette(&game, *mesh),
            pose,
            "every mesh on the skeleton shows the animator's pose"
        );
    }
    assert!(
        palette(&game, prop).is_empty(),
        "a mesh on no skeleton is left alone"
    );

    // the pose keeps following the clock on every mesh
    game.tick(2);
    let later = animator_pose(&game, animator);
    assert!(x(&later) > x(&pose));
    for mesh in &hero {
        assert_eq!(palette(&game, *mesh), later);
    }

    // a mesh added at runtime picks up the current pose
    let late = game.with(|w| attach_mesh(w, owner, Some(&project.skeleton_id)));
    assert!(palette(&game, late).is_empty(), "nothing shown yet");
    game.tick(1);
    let now = animator_pose(&game, animator);
    assert!(x(&now) > x(&later));
    assert_eq!(
        palette(&game, late),
        now,
        "the new mesh shows the current pose"
    );
    for mesh in &hero {
        assert_eq!(palette(&game, *mesh), now);
    }
}

#[test]
fn two_animators_never_share_a_mesh() {
    let _guard = PROJECT.lock().unwrap_or_else(|e| e.into_inner());
    let project = open_project();
    let mut game = Game::new();
    let (slow, fast, meshes, other, other_mesh) = game.with(|w| {
        // one owner, two animators at different speeds, three meshes on the
        // one skeleton: each animator keeps the mesh at its own ordinal, the
        // spare mesh goes to the first animator only
        let owner = w.spawn();
        let slow = attach_animator(w, owner, &project, 0.5);
        let fast = attach_animator(w, owner, &project, 2.0);
        let meshes: Vec<Entity> = (0..3)
            .map(|_| attach_mesh(w, owner, Some(&project.skeleton_id)))
            .collect();
        // another character in the same world, with its own animator
        let other_owner = w.spawn();
        let other = attach_animator(w, other_owner, &project, 1.0);
        let other_mesh = attach_mesh(w, other_owner, Some(&project.skeleton_id));
        (slow, fast, meshes, other, other_mesh)
    });
    settle(&mut game, &[slow, fast, other]);
    game.tick(3);

    let (slow_pose, fast_pose, other_pose) = (
        animator_pose(&game, slow),
        animator_pose(&game, fast),
        animator_pose(&game, other),
    );
    assert_ne!(
        slow_pose, fast_pose,
        "the animators run at different speeds"
    );
    assert_ne!(slow_pose, other_pose);
    assert_eq!(palette(&game, meshes[0]), slow_pose);
    assert_eq!(palette(&game, meshes[1]), fast_pose);
    assert_eq!(
        palette(&game, meshes[2]),
        slow_pose,
        "the spare mesh belongs to the lowest animator, never to both"
    );
    assert_eq!(
        palette(&game, other_mesh),
        other_pose,
        "an animator never reaches a mesh of another owner"
    );
}

//! The asset-changed hook (Plugin_SKM#13), through the engine's real entry:
//! `engine_backend::scene::reload_updated_asset` runs the animation plugin's
//! registered `AssetChangedRegistration` (`animation_asset_changed` ->
//! `reload_animation_dependencies`) on a world with a real animator that loaded
//! its graph from a project on disk, driven by the engine's own frame loop.
//! Headless: no window and no GPU.
//!
//! What is observed:
//! - the graph recompiles in place: the pose after the reload follows the new
//!   sequence data while the clock carries on;
//! - the Montage cache is cleared by an animation file and left alone by a
//!   texture edit. The cache is private, so it is read through its behaviour:
//!   a cached Montage plays again with its file gone from disk, a cleared one
//!   has to be read (and compiled) again.
//!
//! `the_file_watcher_publishes_animation_updates` covers the watcher's side: an
//! animation file edited on disk publishes the update.

mod common;

// The plugins as a game links them: their roots, with every registration.
use meshes_plugin as _;
use skeletal_animation_plugin as _;

use animation_component::AnimationComponent;
use common::{open_project, sequence_text, Game, Project, PROJECT};
use pulsar_events::{AssetKind, AssetUpdated};
use pulsar_reflection::AssetRef;
use pulsar_scenedb::Entity;

fn running_animator(project: &Project) -> (Game, Entity) {
    let mut game = Game::new();
    let animator = game.with(|world| {
        let owner = world.spawn();
        let mut animator = AnimationComponent::default();
        animator.animation_graph = AssetRef::parse(&project.graph_id);
        animator.playing = true;
        animator.playback_rate = 1.0;
        let animator = pulsar_world_registry::attach_value(world, owner, animator).unwrap();
        // the skinned mesh it drives: the animator plays on the skeleton of its mesh
        let mesh = pulsar_world_registry::attach_value(
            world,
            owner,
            meshes_component::components::SkeletalMeshComponent::default(),
        )
        .unwrap();
        world
            .get_mut::<meshes_component::components::SkeletalMeshComponent>(mesh)
            .unwrap()
            .mesh_skeleton = AssetRef::parse(&project.skeleton_id);
        animator
    });
    game.tick_until(
        "the graph and its skeleton load and a pose is drawn",
        move |w| {
            w.get::<AnimationComponent>(animator)
                .is_some_and(|a| a.bone_world_pose.len() == 3)
        },
    );
    (game, animator)
}

fn play_montage(game: &Game, animator: Entity, project: &Project) -> Result<(), String> {
    game.with(|w| {
        w.get_mut::<AnimationComponent>(animator)
            .unwrap()
            .play_montage(&AssetRef::parse(&project.montage_id).unwrap(), 1.0, None)
            .map(|_| ())
    })
}

fn update(kind: AssetKind, path: &std::path::Path) -> AssetUpdated {
    let mut event = AssetUpdated::new(kind);
    event.path = Some(path.to_path_buf());
    event
}

fn animation_update(path: &std::path::Path) -> AssetUpdated {
    update(AssetKind::Custom("animation".into()), path)
}

/// The engine's entry for an updated asset, on the game's world.
fn reload(game: &Game, event: &AssetUpdated) -> usize {
    game.with(|w| engine_backend::scene::reload_updated_asset(w, event))
}

fn b1_x(game: &Game, animator: Entity) -> f32 {
    game.with(|w| {
        w.get::<AnimationComponent>(animator)
            .unwrap()
            .bone_world_pose[1][3][0]
    })
}

#[test]
fn an_edited_sequence_recompiles_the_running_graph_in_place() {
    let _guard = PROJECT.lock().unwrap_or_else(|e| e.into_inner());
    let project = open_project();
    let (mut game, animator) = running_animator(&project);
    // The first pose is the bind pose, drawn before the sequence has loaded; how many
    // frames pass before the player runs depends on the asset pump's timing. Start
    // from a clock that is certainly running, and derive every expectation from it.
    game.tick_until("the sequence player is running", |w| {
        w.get::<AnimationComponent>(animator)
            .is_some_and(|a| a.bone_world_pose.len() == 3 && a.bone_world_pose[1][3][0] > 0.0)
    });
    game.tick(2);
    let before = b1_x(&game, animator);
    assert!(before > 0.0 && before < 4.0, "2 m/s slide: {before}");
    // 2 m/s: the clock is at before / 2 seconds
    let clock = before / 2.0;

    // the artist moves the slide to 8 m in the sequence file and saves
    let skeleton =
        skeletal_assets::assets::load_skeleton_asset(&project.root.join("body.skeleton")).unwrap();
    let (_, text) = sequence_text(&skeleton, Some(&project.sequence_id), 8.0);
    std::fs::write(&project.sequence_path, text).unwrap();
    project.index.refresh_path(&project.sequence_path).unwrap();

    let reloaded = reload(&game, &animation_update(&project.sequence_path));
    assert_eq!(
        reloaded, 1,
        "the one running graph that names the file recompiled"
    );

    // the pose is evaluated from the new data (the slide per second doubled)
    // and the clock carried on instead of restarting
    game.tick(1);
    let after = b1_x(&game, animator);
    game.tick(1);
    let later = b1_x(&game, animator);
    assert!(
        (later - after - 4.0 * 0.1).abs() < 1e-2,
        "the new 4 m/s slide: {after} -> {later}"
    );
    // the clock carried on from `clock` (a restart would give 4 * STEP = 0.4)
    assert!(
        (after - 4.0 * (clock + common::STEP)).abs() < 1e-2,
        "the clock was not restarted: {after}, expected {}",
        4.0 * (clock + common::STEP)
    );

    // a file the graph does not name recompiles nothing
    let other = project.root.join("other.animseq");
    std::fs::write(&other, "{}").unwrap();
    assert_eq!(reload(&game, &animation_update(&other)), 0);
}

#[test]
fn a_texture_edit_leaves_the_montage_caches_and_an_animation_edit_clears_them() {
    let _guard = PROJECT.lock().unwrap_or_else(|e| e.into_inner());
    let project = open_project();
    let (game, animator) = running_animator(&project);

    // the first play reads and compiles the Montage; the file is then gone,
    // so a Montage that plays again came from the cache
    play_montage(&game, animator, &project).expect("the first play reads the file");
    let saved = std::fs::read(&project.montage_path).unwrap();
    std::fs::remove_file(&project.montage_path).unwrap();
    play_montage(&game, animator, &project).expect("a cached Montage needs no file");

    // a texture changed on disk: the reload path runs the hook, which ignores it
    let texture = project.root.join("rock.png");
    std::fs::write(&texture, b"not really a png").unwrap();
    reload(&game, &update(AssetKind::Texture, &texture));
    play_montage(&game, animator, &project)
        .expect("a texture edit must not clear the Montage caches");

    // an animation file changed: Montages compile afresh the next time, which
    // needs the file again
    reload(&game, &animation_update(&project.sequence_path));
    assert!(
        play_montage(&game, animator, &project).is_err(),
        "an animation edit clears the Montage caches"
    );
    std::fs::write(&project.montage_path, saved).unwrap();
    // the file is back and the project index knows it again
    project.index.refresh_path(&project.montage_path).unwrap();
    {
        use pulsar_reflection::AssetKind as _;
        project
            .index
            .record_typed_asset_id(
                pulsar_reflection::AnimationMontageAssetKind::KIND,
                &project.montage_id,
                &project.montage_path,
            )
            .unwrap();
    }
    play_montage(&game, animator, &project).expect("and the file compiles again");
}

/// The file watcher publishes `AssetUpdated(Custom("animation"))` for an animation
/// file edited on disk (`engine_fs::watchers::publish_asset_update`).
#[test]
fn the_file_watcher_publishes_animation_updates() {
    let _guard = PROJECT.lock().unwrap_or_else(|e| e.into_inner());
    let project = open_project();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<AssetUpdated>::new()));
    let sink = seen.clone();
    let _subscription = pulsar_events::subscribe_asset_updates(None, move |event| {
        sink.lock().unwrap().push(event.clone());
    });
    engine_fs::watchers::start_watcher(
        project.root.clone(),
        std::sync::Arc::new(engine_fs::AssetIndex::new()),
        std::sync::Arc::new(engine_fs::UserTypeRegistry::new()),
        Some(project.index.clone()),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));

    let skeleton =
        skeletal_assets::assets::load_skeleton_asset(&project.root.join("body.skeleton")).unwrap();
    let (_, text) = sequence_text(&skeleton, Some(&project.sequence_id), 8.0);
    std::fs::write(&project.sequence_path, text).unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if seen
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.path.as_deref() == Some(project.sequence_path.as_path()))
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!(
        "no AssetUpdated for the edited .animseq; saw {:?}",
        seen.lock().unwrap()
    );
}

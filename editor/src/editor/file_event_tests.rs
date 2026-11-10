//! The sequence editor follows the engine's file events (Plugin_SKM#13),
//! through the real wiring and without a window server: a real panel opened on
//! a project on disk in a headless GPUI window, real `engine_fs` events
//! (`engine_fs::emit`, the bus the file watcher and every file operation
//! publish on) reaching the panel's own subscription task, and the panel's own
//! project scan resolving the links again from the files.
//!
//! `scan_generation` counts the panel's rescans.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use engine_fs::FsChangeKind;
use gpui::{AppContext, Entity, TestAppContext, VisualTestContext};

use super::panel::SkeletalAnimEditorPanel;
use crate::core::skeleton::{Bone, Skeleton};
use crate::core::skmesh::{self, Joint, SkeletalMeshAsset};
use crate::core::{assets, AnimationClip, Mat4, MeshLink, SkeletonLink, Transform};

/// The file event bus is one per process, and a test below overruns it on
/// purpose: take turns, so no other test's panel sees that.
static BUS: Mutex<()> = Mutex::new(());

struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
    skeleton: assets::SkeletonAsset,
    skeleton_path: PathBuf,
    mesh: SkeletalMeshAsset,
    mesh_path: PathBuf,
    sequence_path: PathBuf,
}

fn bone(id: &str, parent: Option<&str>) -> Bone {
    Bone {
        id: id.into(),
        name: id.into(),
        parent: parent.map(str::to_owned),
        bind_transform: Transform::IDENTITY,
    }
}

fn bones(list: &[(&str, Option<&str>)]) -> Vec<Bone> {
    list.iter().map(|(id, parent)| bone(id, *parent)).collect()
}

/// A sequence that links a skeleton and a preview mesh, with the skeleton and
/// the mesh files written or left out (so that link cannot resolve).
fn project(write_skeleton: bool, write_mesh: bool) -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("Pulsar.toml"), "").unwrap();
    let skeleton = assets::SkeletonAsset::new(Skeleton {
        bones: bones(&[("root", None), ("arm", Some("root"))]),
    });
    let skeleton_path = root.join("hero.skeleton");
    if write_skeleton {
        assets::save_skeleton_asset(&skeleton_path, &skeleton).unwrap();
    }
    let mesh = SkeletalMeshAsset::new(Some(skeleton.id.clone()));
    let mesh_path = root.join("hero.skmesh");
    if write_mesh {
        skmesh::save_skmesh_asset(&mesh_path, &mesh).unwrap();
    }
    let sequence = assets::AnimSeqAsset::new(Some(skeleton.id.clone()), AnimationClip::bind_pose())
        .with_preview_mesh(Some(mesh.id.clone()));
    let sequence_path = root.join("walk.animseq");
    assets::save_animseq_asset(&sequence_path, &sequence).unwrap();
    Project {
        _dir: dir,
        root,
        skeleton,
        skeleton_path,
        mesh,
        mesh_path,
        sequence_path,
    }
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    path: &Path,
) -> (Entity<SkeletalAnimEditorPanel>, &'a mut VisualTestContext) {
    cx.update(ui::init);
    let path = path.to_path_buf();
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view =
            cx.new(|cx| SkeletalAnimEditorPanel::new_with_path(path.clone(), window, cx).unwrap());
        panel = Some(view.clone());
        ui::Root::new(view.into(), window, cx)
    });
    // the first scan and the file subscription are running tasks
    visual.run_until_parked();
    (panel.unwrap(), visual)
}

fn rescans(panel: &Entity<SkeletalAnimEditorPanel>, cx: &VisualTestContext) -> u64 {
    panel.read_with(cx, |p, _| p.scan_generation)
}

/// Files below are written with `std::fs` (silent): `save_*_asset` goes through
/// the virtual file system, which publishes its own event, and each test sends
/// exactly the events it means to.
///
/// Send `path` through the engine's file event bus and let the panel run.
fn event(path: &Path, kind: FsChangeKind, cx: &mut VisualTestContext) {
    engine_fs::emit(path, kind);
    cx.run_until_parked();
}

#[gpui::test]
fn the_sequence_editor_rescans_for_its_linked_skeleton_and_preview_mesh(cx: &mut TestAppContext) {
    let _bus = BUS.lock().unwrap_or_else(|e| e.into_inner());
    let project = project(true, true);
    let (panel, cx) = open(cx, &project.sequence_path);
    panel.read_with(cx, |p, _| {
        assert!(matches!(p.link, SkeletonLink::Resolved(_)), "{:?}", p.link);
        assert!(matches!(p.preview_link, MeshLink::Resolved(_)));
        assert_eq!(p.skeleton.bones.len(), 2);
        assert_eq!(p.mesh.as_ref().map(|m| m.joints.len()), Some(0));
    });
    let start = rescans(&panel, cx);

    // files that are none of the panel's business change nothing
    for other in [
        project.root.join("rock.png"),
        project.root.join("other.skeleton"),
        project.root.join("other.skmesh"),
        // the sequence's own save is not a reason to reload
        project.sequence_path.clone(),
        // outside the project
        std::env::temp_dir().join("elsewhere").join("hero.skeleton"),
    ] {
        event(&other, FsChangeKind::Modified, cx);
    }
    assert_eq!(rescans(&panel, cx), start, "no rescan for unrelated files");

    // the linked skeleton file gains a bone on disk
    let mut edited = project.skeleton.clone();
    edited.bones = bones(&[("root", None), ("arm", Some("root")), ("hand", Some("arm"))]);
    std::fs::write(&project.skeleton_path, edited.to_text()).unwrap();
    event(&project.skeleton_path, FsChangeKind::Modified, cx);
    assert_eq!(
        rescans(&panel, cx),
        start + 1,
        "the linked skeleton rescans"
    );
    panel.read_with(cx, |p, _| {
        assert!(matches!(p.link, SkeletonLink::Resolved(_)));
        assert_eq!(p.skeleton.bones.len(), 3, "the pose follows the new bones");
    });

    // the preview mesh file changes on disk (same file, same id): it is
    // loaded again
    let mut edited = project.mesh.clone();
    edited.joints.push(Joint {
        bone: "arm".into(),
        inverse_bind: Mat4::IDENTITY.0,
    });
    std::fs::write(&project.mesh_path, edited.encode()).unwrap();
    let revision = panel.read_with(cx, |p, _| p.mesh_revision);
    event(&project.mesh_path, FsChangeKind::Modified, cx);
    assert_eq!(rescans(&panel, cx), start + 2, "the preview mesh rescans");
    panel.read_with(cx, |p, _| {
        assert!(matches!(p.preview_link, MeshLink::Resolved(_)));
        assert_eq!(p.mesh.as_ref().map(|m| m.joints.len()), Some(1));
        assert!(p.mesh_revision > revision, "the drawn mesh was replaced");
    });
}

#[gpui::test]
fn an_unresolved_link_rescans_for_any_file_of_its_kind(cx: &mut TestAppContext) {
    let _bus = BUS.lock().unwrap_or_else(|e| e.into_inner());
    // the sequence names a skeleton and a preview mesh that are not there yet
    let project = project(false, false);
    let (panel, cx) = open(cx, &project.sequence_path);
    panel.read_with(cx, |p, _| {
        assert!(matches!(p.link, SkeletonLink::Missing(_)), "{:?}", p.link);
        assert!(matches!(p.preview_link, MeshLink::Missing(_)));
    });
    let start = rescans(&panel, cx);
    event(&project.root.join("rock.png"), FsChangeKind::Created, cx);
    assert_eq!(rescans(&panel, cx), start, "an image cannot fix a link");

    // the skeleton file turns up (any file with the extension might be it)
    std::fs::write(&project.skeleton_path, project.skeleton.to_text()).unwrap();
    event(&project.skeleton_path, FsChangeKind::Created, cx);
    assert_eq!(rescans(&panel, cx), start + 1);
    panel.read_with(cx, |p, _| {
        assert!(
            matches!(p.link, SkeletonLink::Resolved(_)),
            "the banner clears"
        );
        assert_eq!(p.skeleton.bones.len(), 2);
        assert!(matches!(p.preview_link, MeshLink::Missing(_)));
    });

    // and so does the mesh
    std::fs::write(&project.mesh_path, project.mesh.encode()).unwrap();
    event(&project.mesh_path, FsChangeKind::Created, cx);
    assert_eq!(rescans(&panel, cx), start + 2);
    panel.read_with(cx, |p, _| {
        assert!(matches!(p.preview_link, MeshLink::Resolved(_)));
        assert!(p.mesh.is_some(), "the preview mesh was loaded");
    });
}

#[gpui::test]
fn a_lagged_event_channel_rescans(cx: &mut TestAppContext) {
    let _bus = BUS.lock().unwrap_or_else(|e| e.into_inner());
    let project = project(true, true);
    let (panel, cx) = open(cx, &project.sequence_path);
    let start = rescans(&panel, cx);
    // the files changed while a burst overran the channel (it holds 256): the
    // panel lost events, so it cannot know which, and scans again
    let mut edited = project.skeleton.clone();
    edited.bones = bones(&[("root", None)]);
    std::fs::write(&project.skeleton_path, edited.to_text()).unwrap();
    for n in 0..400 {
        engine_fs::emit(
            project.root.join(format!("noise{n}.txt")),
            FsChangeKind::Modified,
        );
    }
    cx.run_until_parked();
    assert!(rescans(&panel, cx) > start, "the lag triggered a rescan");
    panel.read_with(cx, |p, _| {
        assert_eq!(p.skeleton.bones.len(), 1, "and it saw the new skeleton");
    });
}

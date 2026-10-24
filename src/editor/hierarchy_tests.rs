//! drive the real editor commands and render the hierarchy and properties headlessly.

use super::hierarchy_edit::HierarchyCommand;
use crate::core::{assets, sample, Skeleton, Transform, Vec3};
use crate::editor::{EditorMode, SkeletalAnimEditorPanel};
use crate::ui_components::{BoneHierarchyPanel, BonePropertiesPanel};
use gpui::{AppContext, Entity, TestAppContext, VisualTestContext};

fn editor_window<F>(
    cx: &mut TestAppContext,
    build: F,
) -> (Entity<SkeletalAnimEditorPanel>, &mut VisualTestContext)
where
    F: FnOnce(
        &mut gpui::Window,
        &mut gpui::Context<SkeletalAnimEditorPanel>,
    ) -> SkeletalAnimEditorPanel,
{
    let mut editor = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| build(window, cx));
        editor = Some(view.clone());
        ui::Root::new(view.into(), window, cx)
    });
    (editor.unwrap(), visual)
}

fn command(
    editor: &Entity<SkeletalAnimEditorPanel>,
    command: HierarchyCommand,
    cx: &mut VisualTestContext,
) {
    editor.update_in(cx, |e, window, cx| {
        e.request_hierarchy_command(command, window, cx)
    });
    cx.run_until_parked();
}

fn undo(editor: &Entity<SkeletalAnimEditorPanel>, redo: bool, cx: &mut VisualTestContext) {
    editor.update_in(cx, |e, window, cx| e.undo_hierarchy(redo, window, cx));
    cx.run_until_parked();
}

#[gpui::test]
fn hierarchy_and_socket_commands_restore_selection_dirty_state_and_disk_data(
    cx: &mut TestAppContext,
) {
    cx.update(ui::init);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
    let path = dir.path().join("empty.skeleton");
    let asset = assets::SkeletonAsset::new(Skeleton::default());
    assets::save_skeleton_asset(&path, &asset).unwrap();
    let (editor, cx) = editor_window(cx, |window, cx| {
        SkeletalAnimEditorPanel::new_with_path(path.clone(), window, cx).unwrap()
    });
    // the viewport's gpu renderer isn't needed to exercise the dock panels.
    let hierarchy = cx.new(|cx| BoneHierarchyPanel::new(editor.downgrade(), cx));
    let properties = cx.new(|cx| BonePropertiesPanel::new(editor.downgrade(), cx));
    editor.update(cx, |e, cx| {
        hierarchy.update(cx, |p, cx| p.attach(e, &editor, cx));
        properties.update(cx, |p, cx| p.attach(e, &editor, cx));
    });
    command(&editor, HierarchyCommand::AddBone { parent: None }, cx);
    let root = editor.read_with(cx, |e, _| e.selected_bone.clone().unwrap());
    command(
        &editor,
        HierarchyCommand::AddBone {
            parent: Some(root.clone()),
        },
        cx,
    );
    let child = editor.read_with(cx, |e, _| e.selected_bone.clone().unwrap());
    command(
        &editor,
        HierarchyCommand::RenameBone {
            id: child.clone(),
            name: "Hand".into(),
        },
        cx,
    );
    command(
        &editor,
        HierarchyCommand::AddSocket {
            bone: child.clone(),
        },
        cx,
    );
    let socket = editor.read_with(cx, |e, _| e.selected_socket.clone().unwrap());
    command(
        &editor,
        HierarchyCommand::Transform(Transform {
            translation: Vec3::new(0.0, 1.0, 0.0),
            ..Transform::IDENTITY
        }),
        cx,
    );
    editor.read_with(cx, |e, _| {
        assert_eq!(e.skeleton.bones.len(), 2);
        assert_eq!(e.skeleton.bone(&child).unwrap().name, "Hand");
        assert_eq!(
            e.sockets
                .get(&socket)
                .unwrap()
                .local_transform
                .translation
                .y,
            1.0
        );
        assert_eq!(
            e.skeleton.bone(&child).unwrap().bind_transform,
            Transform::IDENTITY
        );
        assert!(e.is_dirty);
    });
    undo(&editor, false, cx);
    editor.read_with(cx, |e, _| {
        assert_eq!(
            e.sockets.get(&socket).unwrap().local_transform,
            Transform::IDENTITY
        )
    });
    undo(&editor, false, cx);
    editor.read_with(cx, |e, _| {
        assert!(e.sockets.is_empty());
        assert_eq!(e.selected_bone.as_deref(), Some(child.as_str()));
    });
    undo(&editor, true, cx);
    undo(&editor, true, cx);
    editor.update(cx, |e, _| e.save().unwrap());
    assert_eq!(assets::load_skeleton_asset(&path).unwrap().id, asset.id);
    assert_eq!(
        assets::load_skeleton_asset(&path)
            .unwrap()
            .sockets
            .get(&socket)
            .unwrap()
            .local_transform
            .translation
            .y,
        1.0
    );
    command(&editor, HierarchyCommand::DeleteSocket(socket.clone()), cx);
    undo(&editor, false, cx);
    editor.read_with(cx, |e, _| {
        assert!(!e.is_dirty);
        assert_eq!(e.selected_socket.as_deref(), Some(socket.as_str()));
    });
    command(
        &editor,
        HierarchyCommand::Reparent {
            id: child.clone(),
            parent: None,
        },
        cx,
    );
    editor.read_with(cx, |e, _| {
        assert_eq!(e.skeleton.bone(&child).unwrap().parent, None)
    });
    undo(&editor, false, cx);
    editor.read_with(cx, |e, _| {
        assert_eq!(
            e.skeleton.bone(&child).unwrap().parent.as_deref(),
            Some(root.as_str())
        );
        assert!(!e.is_dirty);
    });
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear()
        });
    }
}

#[gpui::test]
fn referenced_skeletons_cannot_be_mutated_from_sequence_or_mesh_modes(cx: &mut TestAppContext) {
    cx.update(ui::init);
    let (editor, cx) = editor_window(cx, SkeletalAnimEditorPanel::new);
    let original = editor.read_with(cx, |e, _| e.skeleton.clone());
    for mode in [EditorMode::Sequence, EditorMode::Mesh] {
        editor.update(cx, |e, _| e.mode = mode);
        command(&editor, HierarchyCommand::AddBone { parent: None }, cx);
        command(
            &editor,
            HierarchyCommand::RenameBone {
                id: "root".into(),
                name: "Changed".into(),
            },
            cx,
        );
        command(
            &editor,
            HierarchyCommand::AddSocket {
                bone: "root".into(),
            },
            cx,
        );
        undo(&editor, false, cx);
        editor.read_with(cx, |e, _| {
            assert_eq!(e.skeleton, original);
            assert!(e.sockets.is_empty());
            assert!(!e.is_dirty);
        });
    }
}

#[gpui::test]
fn validation_failures_do_not_dirty_the_document_or_record_history(cx: &mut TestAppContext) {
    cx.update(ui::init);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
    let path = dir.path().join("rig.skeleton");
    assets::save_skeleton_asset(
        &path,
        &assets::SkeletonAsset::new(sample::humanoid_skeleton()),
    )
    .unwrap();
    let (editor, cx) = editor_window(cx, |window, cx| {
        SkeletalAnimEditorPanel::new_with_path(path.clone(), window, cx).unwrap()
    });
    let original = editor.read_with(cx, |e, _| e.skeleton.clone());
    command(
        &editor,
        HierarchyCommand::AddBone {
            parent: Some("absent".into()),
        },
        cx,
    );
    command(
        &editor,
        HierarchyCommand::Reparent {
            id: "root".into(),
            parent: Some("head".into()),
        },
        cx,
    );
    editor.read_with(cx, |e, _| {
        assert_eq!(e.skeleton, original);
        assert!(!e.is_dirty);
        assert!(!plugin_editor_api::undo::lock(&e.hierarchy_history).can_undo());
        assert!(e.notice.is_some());
    });
}

#[gpui::test]
fn confirmed_subtree_deletion_and_cancel_restore_sockets_and_primary_selection(
    cx: &mut TestAppContext,
) {
    cx.update(ui::init);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
    let path = dir.path().join("rig.skeleton");
    assets::save_skeleton_asset(
        &path,
        &assets::SkeletonAsset::new(sample::humanoid_skeleton()),
    )
    .unwrap();
    let (editor, cx) = editor_window(cx, |window, cx| {
        SkeletalAnimEditorPanel::new_with_path(path.clone(), window, cx).unwrap()
    });
    command(
        &editor,
        HierarchyCommand::AddSocket {
            bone: "head".into(),
        },
        cx,
    );
    let socket = editor.read_with(cx, |e, _| e.selected_socket.clone().unwrap());
    command(
        &editor,
        HierarchyCommand::DeleteBone {
            id: "neck".into(),
            subtree: true,
        },
        cx,
    );
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    editor.read_with(cx, |e, _| {
        assert!(e.skeleton.bone("head").is_some());
        assert!(e.sockets.get(&socket).is_some());
    });
    command(
        &editor,
        HierarchyCommand::DeleteBone {
            id: "neck".into(),
            subtree: true,
        },
        cx,
    );
    cx.simulate_prompt_answer("Delete");
    cx.run_until_parked();
    editor.read_with(cx, |e, _| {
        assert!(e.skeleton.bone("head").is_none());
        assert!(e.skeleton.bone("neck").is_none());
        assert!(e.sockets.is_empty());
        assert_eq!(e.selected_bone.as_deref(), Some("chest"));
        assert!(e.selected_socket.is_none());
    });
    undo(&editor, false, cx);
    editor.read_with(cx, |e, _| {
        assert!(e.skeleton.bone("head").is_some());
        assert_eq!(e.selected_socket.as_deref(), Some(socket.as_str()));
        e.sockets.validate(&e.skeleton).unwrap();
    });
    undo(&editor, true, cx);
    editor.read_with(cx, |e, _| {
        assert!(e.skeleton.bone("neck").is_none());
        assert!(e.sockets.is_empty());
    });
}

#[gpui::test]
fn displaying_bind_transforms_does_not_round_or_record_an_edit(cx: &mut TestAppContext) {
    cx.update(ui::init);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
    let path = dir.path().join("rig.skeleton");
    let mut skeleton = sample::humanoid_skeleton();
    skeleton
        .bone_mut("head")
        .unwrap()
        .bind_transform
        .translation
        .x = 0.1234567;
    assets::save_skeleton_asset(&path, &assets::SkeletonAsset::new(skeleton.clone())).unwrap();
    let (editor, cx) = editor_window(cx, |window, cx| {
        SkeletalAnimEditorPanel::new_with_path(path.clone(), window, cx).unwrap()
    });
    editor.update_in(cx, |e, window, cx| {
        e.select_bone(Some("head".into()), window, cx)
    });
    cx.run_until_parked();
    editor.read_with(cx, |e, _| {
        assert_eq!(e.skeleton, skeleton);
        assert!(!e.is_dirty);
        assert!(!plugin_editor_api::undo::lock(&e.hierarchy_history).can_undo());
    });
    editor.update_in(cx, |e, window, cx| {
        e.transform_inputs.translation[0]
            .update(cx, |input, cx| input.set_value("0.25", window, cx));
    });
    cx.run_until_parked();
    editor.read_with(cx, |e, cx| {
        assert_eq!(
            e.skeleton
                .bone("head")
                .unwrap()
                .bind_transform
                .translation
                .x,
            0.25
        );
        assert_eq!(
            e.transform_inputs.translation[0].read(cx).value().as_ref(),
            "0.25"
        );
    });
    undo(&editor, false, cx);
    editor.read_with(cx, |e, _| {
        assert_eq!(e.skeleton, skeleton);
        assert!(!e.is_dirty);
    });
}

#[gpui::test]
fn save_rechecks_dependencies_added_after_reparenting(cx: &mut TestAppContext) {
    cx.update(ui::init);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
    let path = dir.path().join("rig.skeleton");
    let (skeleton, sequence) = sample::sample_assets();
    assets::save_skeleton_asset(&path, &skeleton).unwrap();
    let before = std::fs::read(&path).unwrap();
    let (editor, cx) = editor_window(cx, |window, cx| {
        SkeletalAnimEditorPanel::new_with_path(path.clone(), window, cx).unwrap()
    });
    command(
        &editor,
        HierarchyCommand::Reparent {
            id: "neck".into(),
            parent: Some("root".into()),
        },
        cx,
    );
    editor.read_with(cx, |e, _| {
        assert_eq!(
            e.skeleton.bone("neck").unwrap().parent.as_deref(),
            Some("root")
        )
    });
    let sequence_path = dir.path().join(".new.animseq");
    assets::save_animseq_asset(&sequence_path, &sequence).unwrap();
    let sequence_before = std::fs::read(&sequence_path).unwrap();
    editor.update(cx, |e, cx| e.save_and_report(cx));
    cx.run_until_parked();
    editor.read_with(cx, |e, _| {
        assert!(e.notice.as_ref().unwrap().contains("migrating"));
        assert!(e.is_dirty);
        assert!(!e.saving_hierarchy);
    });
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read(&sequence_path).unwrap(), sequence_before);
    editor.update_in(cx, |e, w, cx| {
        e.save_and_report(cx);
        e.request_hierarchy_command(
            HierarchyCommand::RenameBone {
                id: "head".into(),
                name: "New Head".into(),
            },
            w,
            cx,
        );
    });
    cx.run_until_parked();
    editor.read_with(cx, |e, _| {
        assert!(e.notice.as_ref().unwrap().contains("changed during Save"));
        assert!(e.is_dirty);
    });
    assert_eq!(std::fs::read(&path).unwrap(), before);
    undo(&editor, false, cx);
    undo(&editor, false, cx);
    editor.update(cx, |e, _| e.save().unwrap());
    assert_eq!(assets::load_skeleton_asset(&path).unwrap().id, skeleton.id);
}

#[gpui::test]
fn editing_one_transform_axis_keeps_the_precision_of_other_axes(cx: &mut TestAppContext) {
    cx.update(ui::init);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
    let path = dir.path().join("rig.skeleton");
    let mut skeleton = sample::humanoid_skeleton();
    skeleton
        .bone_mut("head")
        .unwrap()
        .bind_transform
        .translation
        .x = 0.1234567;
    assets::save_skeleton_asset(&path, &assets::SkeletonAsset::new(skeleton.clone())).unwrap();
    let (editor, cx) = editor_window(cx, |window, cx| {
        SkeletalAnimEditorPanel::new_with_path(path.clone(), window, cx).unwrap()
    });
    editor.update_in(cx, |e, window, cx| {
        e.select_bone(Some("head".into()), window, cx)
    });
    cx.run_until_parked();
    editor.update_in(cx, |e, window, cx| {
        e.transform_inputs.translation[1]
            .update(cx, |input, cx| input.set_value("0.25", window, cx));
    });
    cx.run_until_parked();
    editor.read_with(cx, |e, _| {
        let transform = &e.skeleton.bone("head").unwrap().bind_transform;
        assert_eq!(transform.translation.x, 0.1234567);
        assert_eq!(transform.translation.y, 0.25);
    });
    undo(&editor, false, cx);
    editor.read_with(cx, |e, _| {
        assert_eq!(e.skeleton, skeleton);
        assert!(!e.is_dirty);
    });
}

#[gpui::test]
fn socket_name_and_parent_edits_preserve_the_current_local_transform(cx: &mut TestAppContext) {
    cx.update(ui::init);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
    let path = dir.path().join("rig.skeleton");
    assets::save_skeleton_asset(
        &path,
        &assets::SkeletonAsset::new(sample::humanoid_skeleton()),
    )
    .unwrap();
    let (editor, cx) = editor_window(cx, |window, cx| {
        SkeletalAnimEditorPanel::new_with_path(path.clone(), window, cx).unwrap()
    });
    command(
        &editor,
        HierarchyCommand::AddSocket {
            bone: "head".into(),
        },
        cx,
    );
    let id = editor.read_with(cx, |e, _| e.selected_socket.clone().unwrap());
    let transform = Transform {
        translation: Vec3::new(0.1234567, 0.0, 1.0),
        ..Transform::IDENTITY
    };
    command(&editor, HierarchyCommand::Transform(transform.clone()), cx);
    command(
        &editor,
        HierarchyCommand::RenameSocket {
            id: id.clone(),
            name: "Weapon".into(),
        },
        cx,
    );
    command(
        &editor,
        HierarchyCommand::ReattachSocket {
            id: id.clone(),
            bone: "root".into(),
        },
        cx,
    );
    editor.read_with(cx, |e, _| {
        let socket = e.sockets.get(&id).unwrap();
        assert_eq!(socket.name, "Weapon");
        assert_eq!(socket.bone_id, "root");
        assert_eq!(socket.local_transform, transform);
        assert_eq!(e.selected_bone.as_deref(), Some("root"));
    });
    undo(&editor, false, cx);
    undo(&editor, false, cx);
    editor.read_with(cx, |e, _| {
        let socket = e.sockets.get(&id).unwrap();
        assert_eq!(socket.bone_id, "head");
        assert_ne!(socket.name, "Weapon");
        assert_eq!(socket.local_transform, transform);
    });
}

#[gpui::test]
fn exact_resets_are_independent_transactions_for_bones_and_sockets(cx: &mut TestAppContext) {
    use super::transform_inputs::TransformGroup;
    cx.update(ui::init);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
    let path = dir.path().join("rig.skeleton");
    assets::save_skeleton_asset(
        &path,
        &assets::SkeletonAsset::new(sample::humanoid_skeleton()),
    )
    .unwrap();
    let (editor, cx) = editor_window(cx, |window, cx| {
        SkeletalAnimEditorPanel::new_with_path(path, window, cx).unwrap()
    });
    let precise = Transform {
        translation: Vec3::new(0.0004, 2.0, -0.0004),
        rotation: Vec3::new(0.0004, 2.0, -0.0004),
        scale: Vec3::new(1.0004, 2.0, 0.9996),
    };
    for socket in [false, true] {
        editor.update_in(cx, |e, w, cx| e.select_bone(Some("root".into()), w, cx));
        if socket {
            command(
                &editor,
                HierarchyCommand::AddSocket {
                    bone: "root".into(),
                },
                cx,
            );
        }
        for group in [
            TransformGroup::Translation,
            TransformGroup::Rotation,
            TransformGroup::Scale,
        ] {
            command(&editor, HierarchyCommand::Transform(precise.clone()), cx);
            let length = editor.read_with(cx, |e, _| {
                plugin_editor_api::undo::lock(&e.hierarchy_history).len()
            });
            editor.update_in(cx, |e, w, cx| e.reset_transform_group(group, w, cx));
            cx.run_until_parked();
            let mut expected = precise.clone();
            match group {
                TransformGroup::Translation => expected.translation = Vec3::ZERO,
                TransformGroup::Rotation => expected.rotation = Vec3::ZERO,
                TransformGroup::Scale => expected.scale = Vec3::ONE,
            }
            editor.read_with(cx, |e, _| {
                assert_eq!(e.current_transform(), expected);
                assert_eq!(
                    plugin_editor_api::undo::lock(&e.hierarchy_history).len(),
                    length + 1
                );
            });
            editor.update_in(cx, |e, w, cx| e.reset_transform_group(group, w, cx));
            cx.run_until_parked();
            editor.read_with(cx, |e, _| {
                assert_eq!(
                    plugin_editor_api::undo::lock(&e.hierarchy_history).len(),
                    length + 1
                )
            });
            undo(&editor, false, cx);
            editor.read_with(cx, |e, _| assert_eq!(e.current_transform(), precise));
            undo(&editor, true, cx);
            editor.read_with(cx, |e, _| assert_eq!(e.current_transform(), expected));
            command(&editor, HierarchyCommand::Transform(precise.clone()), cx);
            undo(&editor, false, cx);
            editor.read_with(cx, |e, _| assert_eq!(e.current_transform(), expected));
        }
    }
}

#[gpui::test]
fn asynchronous_save_discards_stale_results_and_preserves_history_boundaries(
    cx: &mut TestAppContext,
) {
    cx.update(ui::init);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
    let path = dir.path().join("rig.skeleton");
    assets::save_skeleton_asset(
        &path,
        &assets::SkeletonAsset::new(sample::humanoid_skeleton()),
    )
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    let (editor, cx) = editor_window(cx, |w, cx| {
        SkeletalAnimEditorPanel::new_with_path(path.clone(), w, cx).unwrap()
    });
    editor.update_in(cx, |e, w, cx| {
        e.select_bone(Some("head".into()), w, cx);
        e.save_and_report(cx);
        assert!(e.saving_hierarchy);
        e.apply_hierarchy_command(
            HierarchyCommand::Transform(Transform {
                translation: Vec3::new(3.0, 0.0, 0.0),
                ..Transform::IDENTITY
            }),
            true,
            w,
            cx,
        );
    });
    cx.run_until_parked();
    editor.read_with(cx, |e, _| {
        assert!(e.is_dirty);
        assert!(e.notice.as_ref().unwrap().contains("changed during Save"));
    });
    assert_eq!(std::fs::read(&path).unwrap(), before);
    editor.update(cx, |e, cx| e.save_and_report(cx));
    cx.run_until_parked();
    editor.read_with(cx, |e, _| {
        assert!(!e.is_dirty);
        assert!(e.notice.is_none());
    });
    let saved = assets::load_skeleton_asset(&path).unwrap();
    command(
        &editor,
        HierarchyCommand::Transform(Transform::IDENTITY),
        cx,
    );
    undo(&editor, false, cx);
    editor.read_with(cx, |e, _| {
        assert!(!e.is_dirty);
        assert_eq!(e.skeleton.bones, saved.bones);
    });
    undo(&editor, false, cx);
    editor.read_with(cx, |e, _| assert!(e.is_dirty));
    undo(&editor, true, cx);
    editor.read_with(cx, |e, _| assert!(!e.is_dirty));
    command(
        &editor,
        HierarchyCommand::Transform(Transform::IDENTITY),
        cx,
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    editor.update(cx, |e, cx| e.save_and_report(cx));
    cx.run_until_parked();
    editor.read_with(cx, |e, _| {
        assert!(e.is_dirty);
        assert!(!e.saving_hierarchy);
        assert!(e.notice.as_ref().unwrap().contains("Could not save"));
        assert_eq!(e.saved_hierarchy.bones, saved.bones);
        assert!(plugin_editor_api::undo::lock(&e.hierarchy_history).is_dirty());
    });
    undo(&editor, false, cx);
    editor.read_with(cx, |e, _| assert!(!e.is_dirty));
}

#[gpui::test]
fn hidden_references_block_both_delete_commands_and_saves(cx: &mut TestAppContext) {
    cx.update(ui::init);
    for kind in ["animation", "mesh", "physics"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
        let path = dir.path().join("rig.skeleton");
        let (skeleton, sequence) = sample::sample_assets();
        assets::save_skeleton_asset(&path, &skeleton).unwrap();
        let before = std::fs::read(&path).unwrap();
        let hidden = dir.path().join(".authored");
        std::fs::create_dir(&hidden).unwrap();
        match kind {
            "animation" => {
                assets::save_animseq_asset(&hidden.join(".hidden.animseq"), &sequence).unwrap()
            }
            "mesh" => crate::core::skmesh::save_skmesh_asset(
                &hidden.join(".hidden.skmesh"),
                &sample::sample_mesh(&skeleton),
            )
            .unwrap(),
            _ => {
                let mut mesh = sample::sample_mesh(&skeleton);
                mesh.joints.clear();
                crate::core::skmesh::save_skmesh_asset(&hidden.join(".hidden.skmesh"), &mesh)
                    .unwrap();
                let physics = pulsar_physics::skeletal::SkeletalPhysicsAsset::from_skeleton(
                    mesh.id,
                    &skeleton.skeleton(),
                );
                std::fs::write(
                    hidden.join(".hidden.physasset"),
                    serde_json::to_string(&physics).unwrap(),
                )
                .unwrap();
            }
        }
        let (editor, cx) = editor_window(cx, |w, cx| {
            SkeletalAnimEditorPanel::new_with_path(path.clone(), w, cx).unwrap()
        });
        command(
            &editor,
            HierarchyCommand::DeleteBone {
                id: "root".into(),
                subtree: true,
            },
            cx,
        );
        cx.simulate_prompt_answer("Delete");
        cx.run_until_parked();
        editor.read_with(cx, |e, _| {
            assert_eq!(e.skeleton.bones, skeleton.bones);
            assert!(!e.is_dirty);
            assert!(e.notice.is_some());
        });
        // a later dependency must also block persistence of an otherwise valid pending edit.
        editor.update_in(cx, |e, w, cx| {
            e.apply_hierarchy_command(
                HierarchyCommand::DeleteBone {
                    id: "root".into(),
                    subtree: true,
                },
                true,
                w,
                cx,
            )
        });
        editor.update(cx, |e, cx| e.save_and_report(cx));
        cx.run_until_parked();
        editor.read_with(cx, |e, _| {
            assert!(e.is_dirty);
            assert!(e.notice.as_ref().unwrap().contains("Could not save"));
        });
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}

#[gpui::test]
fn stale_selection_and_reference_results_do_not_mutate_the_wrong_target(cx: &mut TestAppContext) {
    cx.update(ui::init);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
    let path = dir.path().join("rig.skeleton");
    let asset = assets::SkeletonAsset::new(sample::humanoid_skeleton());
    assets::save_skeleton_asset(&path, &asset).unwrap();
    let (_, mut sequence) = sample::sample_assets();
    sequence.skeleton = Some(asset.id.clone());
    assets::save_animseq_asset(&dir.path().join(".dependency.animseq"), &sequence).unwrap();
    let (editor, cx) = editor_window(cx, |w, cx| {
        SkeletalAnimEditorPanel::new_with_path(path, w, cx).unwrap()
    });
    editor.update_in(cx, |e, w, cx| {
        e.select_bone(Some("head".into()), w, cx);
        e.selected_socket = Some("stale".into());
        e.request_hierarchy_command(HierarchyCommand::Transform(Transform::IDENTITY), w, cx);
        assert_eq!(e.skeleton.bones, asset.bones);
        e.select_bone(Some("missing".into()), w, cx);
        assert!(e.selected_bone.is_none());
        e.request_hierarchy_command(
            HierarchyCommand::Reparent {
                id: "neck".into(),
                parent: Some("root".into()),
            },
            w,
            cx,
        );
        assert!(e.checking_hierarchy);
        e.request_hierarchy_command(
            HierarchyCommand::RenameBone {
                id: "head".into(),
                name: "New Head".into(),
            },
            w,
            cx,
        );
    });
    cx.run_until_parked();
    editor.read_with(cx, |e, _| {
        assert_eq!(
            e.skeleton.bone("neck").unwrap().parent.as_deref(),
            Some("chest")
        );
        assert_eq!(e.skeleton.bone("head").unwrap().name, "New Head");
        assert!(e
            .notice
            .as_ref()
            .unwrap()
            .contains("changed during the reference check"));
    });
}

#[gpui::test]
fn input_focus_boundaries_keep_separate_axis_edits_separately_undoable(cx: &mut TestAppContext) {
    use gpui::Focusable;
    cx.update(ui::init);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
    let path = dir.path().join("rig.skeleton");
    assets::save_skeleton_asset(
        &path,
        &assets::SkeletonAsset::new(sample::humanoid_skeleton()),
    )
    .unwrap();
    let (editor, cx) = editor_window(cx, |w, cx| {
        SkeletalAnimEditorPanel::new_with_path(path, w, cx).unwrap()
    });
    editor.update_in(cx, |e, w, cx| {
        e.select_bone(Some("root".into()), w, cx);
    });
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|w, cx| {
            w.refresh();
            w.draw(cx).clear()
        });
    }
    cx.update(|w, _| w.activate_window());
    cx.simulate_click(
        gpui::point(gpui::px(20.0), gpui::px(20.0)),
        gpui::Modifiers::default(),
    );
    cx.update(|w, cx| {
        w.refresh();
        w.draw(cx).clear()
    });
    assert!(cx.update(|w, _| w.is_window_active()));
    let original = editor.read_with(cx, |e, _| e.current_transform().translation);
    editor.update_in(cx, |e, w, cx| {
        let focus = e.transform_inputs.translation[0].focus_handle(cx);
        focus.focus(w, cx);
    });
    cx.update(|w, cx| {
        w.refresh();
        w.draw(cx).clear()
    });
    cx.run_until_parked();
    editor.update_in(cx, |e, w, cx| {
        e.transform_inputs.translation[0].update(cx, |input, cx| input.set_value("1.25", w, cx))
    });
    cx.run_until_parked();
    editor.update_in(cx, |e, w, cx| {
        let focus = e.transform_inputs.translation[1].focus_handle(cx);
        focus.focus(w, cx);
    });
    cx.update(|w, cx| {
        w.refresh();
        w.draw(cx).clear()
    });
    cx.run_until_parked();
    editor.update_in(cx, |e, w, cx| {
        e.transform_inputs.translation[1].update(cx, |input, cx| input.set_value("2.5", w, cx))
    });
    cx.run_until_parked();
    editor.read_with(cx, |e, _| {
        assert_eq!(e.current_transform().translation, Vec3::new(1.25, 2.5, 0.0))
    });
    undo(&editor, false, cx);
    editor.read_with(cx, |e, _| {
        assert_eq!(
            e.current_transform().translation,
            Vec3::new(1.25, original.y, original.z)
        )
    });
    undo(&editor, false, cx);
    editor.read_with(cx, |e, _| {
        assert_eq!(e.current_transform().translation, original)
    });
    undo(&editor, true, cx);
    undo(&editor, true, cx);
    editor.read_with(cx, |e, _| {
        assert_eq!(e.current_transform().translation, Vec3::new(1.25, 2.5, 0.0))
    });
}

#[gpui::test]
fn workspace_shortcuts_route_document_history_and_leave_text_undo_in_the_field(
    cx: &mut TestAppContext,
) {
    use gpui::Focusable;
    cx.update(ui::init);
    cx.update(plugin_editor_api::bind_undo_keys);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
    let path = dir.path().join("rig.skeleton");
    let asset = assets::SkeletonAsset::new(sample::humanoid_skeleton());
    assets::save_skeleton_asset(&path, &asset).unwrap();
    let (editor, cx) = editor_window(cx, |w, cx| {
        SkeletalAnimEditorPanel::new_with_path(path.clone(), w, cx).unwrap()
    });
    command(
        &editor,
        HierarchyCommand::AddBone {
            parent: Some("root".into()),
        },
        cx,
    );
    let added = editor.read_with(cx, |e, _| e.selected_bone.clone().unwrap());
    editor.update_in(cx, |e, w, cx| e.focus_handle.focus(w, cx));
    cx.update(|w, cx| {
        w.refresh();
        w.draw(cx).clear()
    });
    cx.simulate_keystrokes("secondary-z");
    editor.read_with(cx, |e, _| {
        assert_eq!(e.skeleton.bones, asset.bones);
        assert!(!e.is_dirty);
    });
    cx.simulate_keystrokes("secondary-shift-z");
    editor.read_with(cx, |e, _| assert!(e.skeleton.bone(&added).is_some()));
    editor.update_in(cx, |e, w, cx| e.select_bone(Some(added.clone()), w, cx));
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|w, cx| {
            w.refresh();
            w.draw(cx).clear()
        });
    }
    editor.update_in(cx, |e, w, cx| {
        let focus = e.transform_inputs.translation[0].focus_handle(cx);
        focus.focus(w, cx);
    });
    cx.update(|w, cx| {
        w.refresh();
        w.draw(cx).clear()
    });
    cx.simulate_keystrokes("secondary-a");
    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("0.25".into())));
    cx.simulate_keystrokes("secondary-v");
    let depth = editor.read_with(cx, |e, _| {
        plugin_editor_api::undo::lock(&e.hierarchy_history).len()
    });
    cx.simulate_keystrokes("secondary-z");
    editor.read_with(cx, |e, _| {
        assert!(e.skeleton.bone(&added).is_some());
        assert_eq!(
            plugin_editor_api::undo::lock(&e.hierarchy_history).len(),
            depth
        );
    });
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-shift-z"
    } else {
        "ctrl-y"
    });
    let edited = editor.read_with(cx, |e, _| e.current_transform());
    for (keys, expected) in [
        ("down enter", Transform::IDENTITY),
        ("down down enter", edited),
    ] {
        cx.update(|w, cx| {
            w.refresh();
            w.draw(cx).clear()
        });
        let bounds = cx.debug_bounds("editor-menu-Edit").unwrap();
        cx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::default());
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        cx.simulate_keystrokes(keys);
        editor.read_with(cx, |e, _| {
            assert!(e.skeleton.bone(&added).is_some());
            assert_eq!(e.current_transform(), expected);
        });
    }
    editor.update_in(cx, |e, w, cx| e.focus_handle.focus(w, cx));
    cx.update(|w, cx| {
        w.refresh();
        w.draw(cx).clear()
    });
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    editor.read_with(cx, |e, _| {
        assert!(!e.is_dirty);
        assert!(e.notice.is_none());
    });
    assert_eq!(
        assets::load_skeleton_asset(&path).unwrap().bones.len(),
        asset.bones.len() + 1
    );
}

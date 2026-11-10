//! Property editors for the animation assets a component names: the Skeletal
//! Mesh and Animation Graph slots of a skeletal mesh component
//! (`Option<AssetRef<SkeletalMeshAssetKind>>`,
//! `Option<AssetRef<AnimationGraphAssetKind>>`).
//!
//! Each is the editor's generic typed reference editor
//! (`ui_common::asset_ref_editor`). The mesh editor also shows the Skeleton
//! the mesh is bound to, read from the mesh file's header: the mesh is the
//! only authority for it, so the row is read-only and the component never
//! stores it.
//!
//! The editors are attached to their types (`UiPropertyEditorHint`, a
//! reflection type attachment): they are this crate's, linked only with the
//! plugin's `editor` feature.

use std::path::Path;

use gpui::{App, Window};
use pulsar_reflection::{
    AnimationGraphAssetKind, AssetKind, AssetRef, BoundPropertyEditor, PropertyEditorArgs,
    PropertyEditorFactory, SkeletalMeshAssetKind, SkeletonAssetKind,
};
use ui_common::asset_ref_editor::{asset_ref_editor, no_details};

/// The Skeleton a mesh file is bound to: its file name when the project index
/// resolves it, a note when the mesh has none or the skeleton is missing.
fn describe_mesh(mesh_path: &Path, root: &Path) -> Vec<(&'static str, String)> {
    let text = match skeletal_assets::skmesh::load_skmesh_asset(mesh_path) {
        Err(_) => "mesh unreadable".to_owned(),
        Ok(mesh) => match mesh.skeleton {
            None => "none: pick one in the mesh editor".to_owned(),
            Some(id) => engine_fs::open_project_file_index(root)
                .ok()
                .flatten()
                .and_then(|index| {
                    index
                        .path_for_asset_id_kind(SkeletonAssetKind::KIND, &id.to_string())
                        .ok()
                        .flatten()
                })
                .and_then(|path| path.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| format!("missing ({id})")),
        },
    };
    vec![("Skeleton (from mesh)", text)]
}

fn skeletal_mesh_editor(
    args: &PropertyEditorArgs<'_>,
    window: &mut Window,
    cx: &mut App,
) -> BoundPropertyEditor {
    asset_ref_editor::<SkeletalMeshAssetKind>(args, "No mesh", describe_mesh, window, cx)
}

fn animation_graph_editor(
    args: &PropertyEditorArgs<'_>,
    window: &mut Window,
    cx: &mut App,
) -> BoundPropertyEditor {
    asset_ref_editor::<AnimationGraphAssetKind>(args, "No animation graph", no_details, window, cx)
}

pulsar_reflection::inventory::submit! {
    pulsar_reflection::UiPropertyEditorHint {
        type_id: std::any::TypeId::of::<Option<AssetRef<SkeletalMeshAssetKind>>>(),
        fn_ptr: pulsar_reflection::erase_property_editor_fn_ptr(
            skeletal_mesh_editor as PropertyEditorFactory
        ),
    }
}

pulsar_reflection::inventory::submit! {
    pulsar_reflection::UiPropertyEditorHint {
        type_id: std::any::TypeId::of::<Option<AssetRef<AnimationGraphAssetKind>>>(),
        fn_ptr: pulsar_reflection::erase_property_editor_fn_ptr(
            animation_graph_editor as PropertyEditorFactory
        ),
    }
}

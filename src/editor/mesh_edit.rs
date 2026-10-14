//! Edits to the skeletal mesh being edited (`Mesh` mode): its material slots.
//!
//! The geometry, joints and weights are imported data and are not edited here;
//! the editor only inspects them. What a person sets on the mesh itself is
//! which material each slot uses, and which skeleton it is skinned to (see
//! [`super::skeleton_ref`]).

use std::path::Path;
use std::sync::Arc;

use gpui::*;
use pulsar_reflection::MaterialRef;
use ui_common::{AssetPickedEvent, AssetQuery, MeshAssetPicker};

use super::panel::{EditorMode, SkeletalAnimEditorPanel};
use crate::core::material_ids;

/// What a material slot can point at: a `.mat` file or a shader graph folder.
fn material_queries() -> Vec<AssetQuery> {
    vec![
        AssetQuery::extension("mat"),
        AssetQuery::folder_marker("shader_graph_save.json"),
    ]
}

impl SkeletalAnimEditorPanel {
    /// Build one material picker per slot of the mesh on screen. Called once
    /// when the workspace is built; the slot count does not change while the
    /// mesh is open.
    pub(crate) fn init_slot_pickers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode != EditorMode::Mesh || !self.slot_pickers.is_empty() {
            return;
        }
        let Some(mesh) = self.mesh.clone() else {
            return;
        };
        let root = (!self.project_root.as_os_str().is_empty()).then(|| self.project_root.clone());
        for (index, slot) in mesh.material_slots.iter().enumerate() {
            let mut selected = slot.material.clone().unwrap_or_default();
            let mut material_id = slot.material_id.clone();
            if let (Some(root), Some(id)) = (root.as_deref(), material_id.as_ref()) {
                if let Some(path) = material_ids::path_for_id(root, id) {
                    selected = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                }
            } else if let (Some(root), Some(path)) = (root.as_deref(), slot.material.as_deref()) {
                let path = Path::new(path);
                let absolute = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    root.join(path)
                };
                if let Ok(id) = material_ids::id_for_path(root, &absolute) {
                    material_id = Some(id);
                }
            }
            if material_id != slot.material_id
                || selected != slot.material.clone().unwrap_or_default()
            {
                self.set_slot_material(
                    index,
                    (!selected.is_empty()).then_some(selected.clone()),
                    material_id,
                    cx,
                );
            }
            let picker = cx.new(|cx| {
                MeshAssetPicker::new(
                    selected,
                    Vec::new(),
                    root.clone(),
                    material_queries(),
                    window,
                    cx,
                )
            });
            self.subscriptions.push(cx.subscribe_in(
                &picker,
                window,
                move |this, picker, _: &AssetPickedEvent, _window, cx| {
                    let chosen = picker.read(cx).selected_path().to_string();
                    let material = if chosen.is_empty() {
                        None
                    } else {
                        let root = this.project_root.clone();
                        let path = Path::new(&chosen);
                        let absolute = if path.is_absolute() {
                            path.to_path_buf()
                        } else {
                            root.join(path)
                        };
                        match material_ids::id_for_path(&root, &absolute) {
                            Ok(id) => Some(id),
                            Err(error) => {
                                log::warn!(
                                    "could not assign stable material id for {chosen}: {error}"
                                );
                                return;
                            }
                        }
                    };
                    this.set_slot_material(
                        index,
                        (!chosen.is_empty()).then_some(chosen),
                        material,
                        cx,
                    );
                },
            ));
            self.slot_pickers.push(picker);
        }
    }

    /// Assign a stable material reference and its current project-relative path
    /// to slot `index`, or clear it.
    pub fn set_slot_material(
        &mut self,
        index: usize,
        material: Option<String>,
        material_id: Option<MaterialRef>,
        cx: &mut Context<Self>,
    ) {
        if self.mode != EditorMode::Mesh {
            return;
        }
        let Some(mesh) = self.mesh.as_mut() else {
            return;
        };
        let Some(slot) = Arc::make_mut(mesh).material_slots.get_mut(index) else {
            return;
        };
        if slot.material == material && slot.material_id == material_id {
            return;
        }
        slot.material = material.clone();
        slot.material_id = material_id;
        self.mesh_revision += 1;
        self.is_dirty = true;
        if let Some(picker) = self.slot_pickers.get(index) {
            let shown = material.unwrap_or_default();
            picker.update(cx, |p, _| p.set_selected_path(shown));
        }
        cx.notify();
    }
}

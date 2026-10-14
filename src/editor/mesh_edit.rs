//! Edits to the skeletal mesh being edited (`Mesh` mode): its material slots.
//!
//! The geometry, joints and weights are imported data and are not edited here;
//! the editor only inspects them. What a person sets on the mesh itself is
//! which material each slot uses, and which skeleton it is skinned to (see
//! [`super::skeleton_ref`]).

use std::sync::Arc;

use gpui::*;
use ui_common::{AssetPickedEvent, AssetQuery, MeshAssetPicker};

use super::panel::{EditorMode, SkeletalAnimEditorPanel};

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
            let selected = slot.material.clone().unwrap_or_default();
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
                    this.set_slot_material(index, (!chosen.is_empty()).then_some(chosen), cx);
                },
            ));
            self.slot_pickers.push(picker);
        }
    }

    /// Assign `material` (a project-relative path) to slot `index`, or clear it.
    pub fn set_slot_material(
        &mut self,
        index: usize,
        material: Option<String>,
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
        if slot.material == material {
            return;
        }
        slot.material = material.clone();
        self.mesh_revision += 1;
        self.is_dirty = true;
        if let Some(picker) = self.slot_pickers.get(index) {
            let shown = material.unwrap_or_default();
            picker.update(cx, |p, _| p.set_selected_path(shown));
        }
        cx.notify();
    }
}

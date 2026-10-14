//! A skeleton's or sequence's optional preview mesh: resolving the stored
//! [`SkeletalMeshId`] through the project's mesh index, loading the mesh it
//! names, and the typed picker that changes it.
//!
//! This is [`super::skeleton_ref`] for the other direction of the link: the
//! skeleton and the sequence do not depend on the mesh, they only draw it. An
//! unresolved or ambiguous link therefore never blocks editing; it draws
//! nothing and says why.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use ui::button::{Button, ButtonVariants};
use ui::popover::Popover;
use ui::{h_flex, ActiveTheme, Disableable, Icon, IconName, Sizable};
use ui_common::{AssetPickedEvent, AssetQuery, MeshAssetPicker};

use crate::core::mesh_library;
use crate::core::skmesh::{self, SKMESH_EXTENSION};
use crate::core::{MeshLink, SkeletalMeshId};

use super::link_state;
use super::panel::{EditorMode, SkeletalAnimEditorPanel};
use super::skeleton_ref::status_tag;

impl SkeletalAnimEditorPanel {
    /// Name the preview mesh of a freshly opened asset, before any scan has
    /// resolved it.
    pub(crate) fn set_preview_ref_unscanned(&mut self, id: Option<SkeletalMeshId>) {
        self.preview_link = match &id {
            Some(id) => MeshLink::Resolving(id.clone()),
            None => MeshLink::Unset,
        };
        self.preview_mesh_ref = id;
    }

    /// Take what the preview reference resolved to, and load the mesh if it is
    /// a file that is not already the one on screen.
    pub(crate) fn apply_preview_link(&mut self, link: MeshLink, cx: &mut Context<Self>) {
        let wanted = match &link {
            MeshLink::Resolved(entry) => Some(entry.path.clone()),
            _ => None,
        };
        self.preview_link = link;
        match wanted {
            Some(path) if self.preview_loaded_from.as_ref() == Some(&path) => {}
            Some(path) => self.load_preview_mesh(path, cx),
            None => {
                // Nothing to draw: drop a mesh left from an earlier link.
                self.mesh_generation += 1;
                self.preview_loaded_from = None;
                if self.mesh.take().is_some() {
                    self.mesh_revision += 1;
                }
            }
        }
        self.refresh_issues();
    }

    /// Read the mesh file off the UI thread. A load started later supersedes
    /// an earlier one still running.
    fn load_preview_mesh(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.mesh_generation += 1;
        let generation = self.mesh_generation;
        self.preview_loaded_from = Some(path.clone());
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(
                    async move { skmesh::load_skmesh_asset(&path).map_err(|e| format!("{e:#}")) },
                )
                .await;
            this.update(cx, |this, cx| {
                if this.mesh_generation != generation {
                    return;
                }
                match loaded {
                    Ok(mesh) => {
                        this.mesh = Some(Arc::new(mesh));
                        this.mesh_revision += 1;
                    }
                    Err(error) => {
                        log::warn!("Cannot load the preview mesh: {error}");
                        this.preview_loaded_from = None;
                        this.notice =
                            Some(format!("The preview mesh could not be loaded: {error}"));
                        if this.mesh.take().is_some() {
                            this.mesh_revision += 1;
                        }
                    }
                }
                this.refresh_issues();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Point the skeleton or sequence at the preview mesh with `id` (or none).
    pub fn set_preview_mesh_ref(&mut self, id: Option<SkeletalMeshId>, cx: &mut Context<Self>) {
        if self.mode == EditorMode::Mesh || id == self.preview_mesh_ref {
            return;
        }
        self.preview_mesh_ref = id;
        // A new choice reads the file again, so edits made since show up.
        self.preview_loaded_from = None;
        let link = self.mesh_library.link(self.preview_mesh_ref.as_ref());
        self.apply_preview_link(link, cx);
        self.is_dirty = true;
        self.rescan_library(cx);
        cx.notify();
    }

    /// The picker returned a project-relative path; store the id of the mesh
    /// in that file.
    pub fn set_preview_mesh_from_path(&mut self, relative: &str, cx: &mut Context<Self>) {
        let path = self.project_root.join(relative);
        match mesh_library::read_id(&path) {
            Ok(id) => {
                self.mesh_library.add_path(&path);
                self.set_preview_mesh_ref(Some(id), cx);
            }
            Err(err) => log::warn!("Cannot use {path:?} as a skeletal mesh: {err}"),
        }
    }

    /// Give the mesh file at `path` a new id, separating it from the other
    /// files that shared one, then re-resolve the reference.
    pub fn regenerate_mesh_id(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        match mesh_library::regenerate_id(&path) {
            Ok(_) => {
                self.preview_loaded_from = None;
                self.rescan_library(cx);
            }
            Err(err) => log::warn!("Cannot give {path:?} a new id: {err}"),
        }
    }

    /// The `.skmesh` search popover's content, created on first use. `None`
    /// when there is no project to search (the unsaved sample).
    fn mesh_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<MeshAssetPicker>> {
        if self.project_root.as_os_str().is_empty() {
            return None;
        }
        if self.mesh_picker.is_none() {
            let (selected, root) = (
                link_state::mesh_picked_path(&self.preview_link, &self.project_root),
                self.project_root.clone(),
            );
            let picker = cx.new(|cx| {
                MeshAssetPicker::new(
                    selected,
                    Vec::new(),
                    Some(root),
                    vec![AssetQuery::extension(SKMESH_EXTENSION)],
                    window,
                    cx,
                )
            });
            self.subscriptions.push(cx.subscribe_in(
                &picker,
                window,
                |this, picker, _: &AssetPickedEvent, _window, cx| {
                    let chosen = picker.read(cx).selected_path().to_string();
                    this.set_preview_mesh_from_path(&chosen, cx);
                },
            ));
            self.mesh_picker = Some(picker);
        }
        let picker = self.mesh_picker.clone()?;
        let current = link_state::mesh_picked_path(&self.preview_link, &self.project_root);
        if picker.read(cx).selected_path() != current {
            picker.update(cx, |p, _| p.set_selected_path(current));
        }
        Some(picker)
    }

    /// The toolbar's preview-mesh field: a name button that opens the picker,
    /// a status tag, and a clear button.
    pub(crate) fn render_preview_mesh_field(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let picker = self.mesh_picker(window, cx);
        let state = link_state::mesh_field(&self.preview_link, &self.project_root);

        let trigger = Button::new("preview-mesh")
            .label(state.label.clone())
            .small()
            .outline()
            .dropdown_caret(true)
            .tooltip(state.tooltip.clone())
            .disabled(picker.is_none());
        let field: AnyElement = match picker {
            Some(picker) => Popover::<MeshAssetPicker>::new("preview-mesh-popover")
                .anchor(Corner::BottomLeft)
                .trigger(trigger)
                .content(move |_, _| picker.clone())
                .into_any_element(),
            None => trigger.into_any_element(),
        };

        let clear = cx.entity();
        h_flex()
            .gap_1p5()
            .items_center()
            .child(
                Icon::new(IconName::Cube)
                    .size(px(14.0))
                    .text_color(theme.muted_foreground),
            )
            .child(field)
            .child(status_tag(&state))
            .when(self.preview_mesh_ref.is_some(), |row| {
                row.child(
                    Button::new("preview-mesh-clear")
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .tooltip("Clear the preview mesh")
                        .on_click(move |_, _window, cx| {
                            clear.update(cx, |editor, cx| editor.set_preview_mesh_ref(None, cx));
                        }),
                )
            })
            .into_any_element()
    }
}

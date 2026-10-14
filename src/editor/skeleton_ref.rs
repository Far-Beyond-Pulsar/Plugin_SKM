//! An asset's reference to its skeleton: resolving the stored id through the
//! project library, the typed asset picker that changes it, and the banner
//! that explains an unresolved link (of the skeleton, or of a preview mesh).
//!
//! A sequence or a skeletal mesh stores only a [`SkeletonId`]. The name and
//! path shown here are looked up from that id on every render, so renaming or
//! moving the skeleton file never leaves a stale label behind.

use gpui::prelude::FluentBuilder;
use gpui::*;
use ui::button::{Button, ButtonVariants};
use ui::popover::Popover;
use ui::tag::Tag;
use ui::{h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable};
use ui_common::{AssetPickedEvent, AssetQuery, MeshAssetPicker};

use crate::core::assets::SKELETON_EXTENSION;
use crate::core::library::{self, Library};
use crate::core::validate::{
    validate_preview, validate_sequence, validate_skeletal_mesh, Severity,
};
use crate::core::{MeshLibrary, MeshLink, Skeleton, SkeletonId, SkeletonLink};

use super::link_state::{self, display_path, FieldState, Tone};
use super::panel::{EditorMode, SkeletalAnimEditorPanel, TracerState};

/// A status tag in the color of its tone.
pub(crate) fn status_tag(state: &FieldState) -> AnyElement {
    match state.tone {
        Tone::Warning => Tag::warning()
            .small()
            .child(state.status)
            .into_any_element(),
        Tone::Secondary => Tag::secondary()
            .small()
            .child(state.status)
            .into_any_element(),
        Tone::Success => Tag::success()
            .small()
            .child(state.status)
            .into_any_element(),
        Tone::Danger => Tag::danger().small().child(state.status).into_any_element(),
    }
}

impl SkeletalAnimEditorPanel {
    /// Recompute [`Self::issues`] for the current links and data.
    pub(crate) fn refresh_issues(&mut self) {
        let preview = |this: &Self, skeleton_id: Option<&SkeletonId>| {
            validate_preview(
                &this.preview_link,
                skeleton_id,
                &this.skeleton,
                this.mesh.as_deref(),
            )
        };
        self.issues = match self.mode {
            EditorMode::Skeleton => preview(self, self.asset_id.as_ref()),
            EditorMode::Sequence => {
                let mut issues = validate_sequence(&self.animation, &self.link);
                issues.extend(preview(self, self.skeleton_ref.as_ref()));
                issues
            }
            EditorMode::Mesh => self
                .mesh
                .as_deref()
                .map(|mesh| validate_skeletal_mesh(mesh, &self.link))
                .unwrap_or_default(),
        };
    }

    /// Scan the project for skeletons (and for meshes, if a preview mesh is
    /// named) off the UI thread, then re-resolve the references against the
    /// result. A scan started later supersedes an earlier one that is still
    /// running.
    pub(crate) fn rescan_library(&mut self, cx: &mut Context<Self>) {
        self.scan_generation += 1;
        let generation = self.scan_generation;
        let root = self.project_root.clone();
        // A skeleton being edited is its own source: nothing to resolve.
        let scan_skeletons = self.mode != EditorMode::Skeleton;
        let scan_meshes = self.preview_mesh_ref.is_some();
        cx.spawn(async move |this, cx| {
            let (skeletons, meshes) = cx
                .background_executor()
                .spawn(async move {
                    (
                        scan_skeletons.then(|| Library::scan(&root)),
                        scan_meshes.then(|| MeshLibrary::scan(&root)),
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.scan_generation == generation {
                    this.set_libraries(skeletons, meshes, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn set_libraries(
        &mut self,
        skeletons: Option<Library>,
        meshes: Option<MeshLibrary>,
        cx: &mut Context<Self>,
    ) {
        if let Some(library) = skeletons {
            self.library = library;
            let link = self.library.link(self.skeleton_ref.as_ref());
            self.apply_link(link);
        }
        if let Some(library) = meshes {
            self.mesh_library = library;
            let link = self.mesh_library.link(self.preview_mesh_ref.as_ref());
            self.apply_preview_link(link, cx);
        }
        self.refresh_issues();
        cx.notify();
    }

    /// Show what `link` resolved to: the referenced skeleton's bones, or none.
    fn apply_link(&mut self, link: SkeletonLink) {
        self.skeleton = match &link {
            SkeletonLink::Resolved(entry) => (*entry.skeleton).clone(),
            _ => Skeleton::default(),
        };
        self.link = link;
        self.skeleton_revision += 1;
        self.tracer_state = TracerState::new(&self.skeleton);
        if self
            .selected_bone
            .as_ref()
            .is_some_and(|id| self.skeleton.bone(id).is_none())
        {
            self.selected_bone = None;
        }
        let roots: Vec<String> = self
            .skeleton
            .root_bones()
            .iter()
            .map(|b| b.id.clone())
            .collect();
        self.expanded_bones.extend(roots);
        self.refresh_issues();
    }

    /// Point the sequence or mesh at the skeleton with `id` (or at none).
    pub fn set_skeleton_ref(&mut self, id: Option<SkeletonId>, cx: &mut Context<Self>) {
        if self.mode == EditorMode::Skeleton || id == self.skeleton_ref {
            return;
        }
        self.skeleton_ref = id;
        let link = self.library.link(self.skeleton_ref.as_ref());
        self.apply_link(link);
        self.is_dirty = true;
        self.rescan_library(cx);
        cx.notify();
    }

    /// The picker returned a project-relative path; store the id of the
    /// skeleton in that file. A file that has no id yet is given one.
    pub fn set_skeleton_from_path(&mut self, relative: &str, cx: &mut Context<Self>) {
        let path = self.project_root.join(relative);
        match library::stamp_file(&path) {
            Ok(id) => {
                self.library.add_path(&path);
                self.set_skeleton_ref(Some(id), cx);
            }
            Err(err) => log::warn!("Cannot use {path:?} as a skeleton: {err}"),
        }
    }

    /// Give the skeleton file at `path` a new id, separating it from the other
    /// files that shared one, then re-resolve the reference.
    pub fn regenerate_skeleton_id(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        match library::regenerate_id(&path) {
            Ok(_) => self.rescan_library(cx),
            Err(err) => log::warn!("Cannot give {path:?} a new id: {err}"),
        }
    }

    /// The `.skeleton` search popover's content, created on first use. `None`
    /// when there is no project to search (the unsaved sample).
    fn skeleton_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<MeshAssetPicker>> {
        if self.project_root.as_os_str().is_empty() {
            return None;
        }
        if self.skeleton_picker.is_none() {
            let (selected, root) = (
                link_state::skeleton_picked_path(&self.link, &self.project_root),
                self.project_root.clone(),
            );
            let picker = cx.new(|cx| {
                MeshAssetPicker::new(
                    selected,
                    Vec::new(),
                    Some(root),
                    vec![AssetQuery::extension(SKELETON_EXTENSION)],
                    window,
                    cx,
                )
            });
            self.subscriptions.push(cx.subscribe_in(
                &picker,
                window,
                |this, picker, _: &AssetPickedEvent, _window, cx| {
                    let chosen = picker.read(cx).selected_path().to_string();
                    this.set_skeleton_from_path(&chosen, cx);
                },
            ));
            self.skeleton_picker = Some(picker);
        }
        let picker = self.skeleton_picker.clone()?;
        let current = link_state::skeleton_picked_path(&self.link, &self.project_root);
        if picker.read(cx).selected_path() != current {
            picker.update(cx, |p, _| p.set_selected_path(current));
        }
        Some(picker)
    }

    /// The toolbar's skeleton field: a name button that opens the picker, a
    /// status tag, and a clear button.
    pub(crate) fn render_skeleton_field(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let picker = self.skeleton_picker(window, cx);
        let unset_hint = match self.mode {
            EditorMode::Mesh => "Pick the skeleton this mesh is skinned to",
            _ => "Pick the skeleton this sequence animates",
        };
        let state = link_state::skeleton_field(&self.link, &self.project_root, unset_hint);

        let trigger = Button::new("skeleton-ref")
            .label(state.label.clone())
            .small()
            .outline()
            .dropdown_caret(true)
            .tooltip(state.tooltip.clone())
            .disabled(picker.is_none());
        let field: AnyElement = match picker {
            Some(picker) => Popover::<MeshAssetPicker>::new("skeleton-ref-popover")
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
                Icon::new(IconName::Accessibility)
                    .size(px(14.0))
                    .text_color(theme.muted_foreground),
            )
            .child(field)
            .child(status_tag(&state))
            .when(self.skeleton_ref.is_some(), |row| {
                row.child(
                    Button::new("skeleton-ref-clear")
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .tooltip("Clear the skeleton")
                        .on_click(move |_, _window, cx| {
                            clear.update(cx, |editor, cx| editor.set_skeleton_ref(None, cx));
                        }),
                )
            })
            .into_any_element()
    }

    /// Rows under the toolbar for the one-off notice and any issues, and,
    /// when a reference is ambiguous, the files that share its id with a
    /// button to give each a new one.
    pub(crate) fn render_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let editor = cx.entity();
        let mut rows: Vec<(Severity, String)> = Vec::new();
        if let Some(notice) = &self.notice {
            rows.push((Severity::Info, notice.clone()));
        }
        rows.extend(self.issues.iter().map(|i| (i.severity, i.message.clone())));

        // Files sharing a referenced id, by project-relative name.
        let relative = |paths: &[std::path::PathBuf]| {
            paths
                .iter()
                .map(|p| (display_path(p, &self.project_root), p.clone()))
                .collect::<Vec<_>>()
        };
        let skeleton_copies = match &self.link {
            SkeletonLink::Ambiguous { paths, .. } => Some(relative(paths)),
            _ => None,
        };
        let mesh_copies = match &self.preview_link {
            MeshLink::Ambiguous { paths, .. } => Some(relative(paths)),
            _ => None,
        };
        if rows.is_empty() {
            return None;
        }
        let copies_row =
            |id: &'static str, copies: Vec<(String, std::path::PathBuf)>, is_mesh: bool| {
                let editor = editor.clone();
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Give a new id to:"),
                    )
                    .children(
                        copies
                            .into_iter()
                            .enumerate()
                            .map(move |(i, (shown, path))| {
                                let editor = editor.clone();
                                Button::new((id, i))
                        .label(shown)
                        .small()
                        .outline()
                        .tooltip("Make this file a distinct asset; the others keep the shared id")
                        .on_click(move |_, _window, cx| {
                            editor.update(cx, |editor, cx| {
                                if is_mesh {
                                    editor.regenerate_mesh_id(path.clone(), cx)
                                } else {
                                    editor.regenerate_skeleton_id(path.clone(), cx)
                                }
                            });
                        })
                            }),
                    )
            };
        Some(
            v_flex()
                .w_full()
                .flex_shrink_0()
                .children(rows.into_iter().map(|(severity, message)| {
                    let (color, icon) = match severity {
                        Severity::Info => (theme.info, IconName::Info),
                        Severity::Warning => (theme.warning, IconName::TriangleAlert),
                        Severity::Error => (theme.danger, IconName::TriangleAlert),
                    };
                    h_flex()
                        .w_full()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .bg(color.opacity(0.12))
                        .border_b_1()
                        .border_color(color.opacity(0.4))
                        .child(Icon::new(icon).size(px(13.0)).text_color(color))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(theme.foreground.opacity(0.9))
                                .child(message),
                        )
                }))
                .when_some(skeleton_copies, |column, copies| {
                    column.child(copies_row("skeleton-new-id", copies, false))
                })
                .when_some(mesh_copies, |column, copies| {
                    column.child(copies_row("mesh-new-id", copies, true))
                })
                .into_any_element(),
        )
    }
}

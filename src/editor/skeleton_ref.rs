//! A sequence's reference to its skeleton: resolving the stored id through the
//! project library, the typed asset picker that changes it, and the banner
//! that explains an unresolved link.
//!
//! The sequence stores only a [`SkeletonId`]. The name and path shown here are
//! looked up from that id on every render, so renaming or moving the skeleton
//! file never leaves a stale label behind.

use std::path::Path;

use gpui::prelude::FluentBuilder;
use gpui::*;
use ui::button::{Button, ButtonVariants};
use ui::popover::Popover;
use ui::tag::Tag;
use ui::{h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable};
use ui_common::{AssetPickedEvent, AssetQuery, MeshAssetPicker};

use crate::core::assets::SKELETON_EXTENSION;
use crate::core::library::{self, Library};
use crate::core::validate::{validate_sequence, Severity};
use crate::core::{Skeleton, SkeletonId, SkeletonLink};

use super::panel::{EditorMode, SkeletalAnimEditorPanel, TracerState};

impl SkeletalAnimEditorPanel {
    /// Recompute [`Self::issues`] for the current link.
    pub(crate) fn refresh_issues(&mut self) {
        self.issues = match self.mode {
            EditorMode::Skeleton => Vec::new(),
            EditorMode::Sequence => validate_sequence(&self.animation, &self.link),
        };
    }

    /// Scan the project for skeletons off the UI thread, then re-resolve the
    /// reference against the result. A scan started later supersedes an
    /// earlier one that is still running.
    pub(crate) fn rescan_library(&mut self, cx: &mut Context<Self>) {
        self.scan_generation += 1;
        let generation = self.scan_generation;
        let root = self.project_root.clone();
        cx.spawn(async move |this, cx| {
            let library = cx
                .background_executor()
                .spawn(async move { Library::scan(&root) })
                .await;
            this.update(cx, |this, cx| {
                if this.scan_generation == generation {
                    this.set_library(library, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn set_library(&mut self, library: Library, cx: &mut Context<Self>) {
        self.library = library;
        let link = self.library.link(self.skeleton_ref.as_ref());
        self.apply_link(link);
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
        let roots: Vec<String> = self.skeleton.root_bones().iter().map(|b| b.id.clone()).collect();
        self.expanded_bones.extend(roots);
        self.refresh_issues();
    }

    /// Point the sequence at the skeleton with `id` (or at none).
    pub fn set_skeleton_ref(&mut self, id: Option<SkeletonId>, cx: &mut Context<Self>) {
        if self.mode != EditorMode::Sequence || id == self.skeleton_ref {
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

    /// Where the referenced skeleton is, relative to the project, for the
    /// picker's highlight.
    fn picked_path(&self) -> String {
        match &self.link {
            SkeletonLink::Resolved(entry) => entry
                .path
                .strip_prefix(&self.project_root)
                .unwrap_or(&entry.path)
                .to_string_lossy()
                .replace('\\', "/"),
            _ => String::new(),
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
            let (selected, root) = (self.picked_path(), self.project_root.clone());
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
        let current = self.picked_path();
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

        let (label, tooltip, status): (String, String, AnyElement) = match &self.link {
            SkeletonLink::Unset => (
                "No skeleton".into(),
                "Pick the skeleton this sequence animates".into(),
                Tag::warning().small().child("Unset").into_any_element(),
            ),
            SkeletonLink::Resolving(id) => (
                format!("Skeleton {}", id.short()),
                "Looking for the skeleton in the project".into(),
                Tag::secondary().small().child("Resolving").into_any_element(),
            ),
            SkeletonLink::Resolved(entry) => (
                entry.name.clone(),
                if entry.path.as_os_str().is_empty() {
                    "Sample skeleton (not saved)".into()
                } else {
                    display_path(&entry.path, &self.project_root)
                },
                Tag::success().small().child("Linked").into_any_element(),
            ),
            SkeletonLink::Missing(id) => (
                format!("Missing {}", id.short()),
                format!("No skeleton with id {id} is in the project"),
                Tag::danger().small().child("Missing").into_any_element(),
            ),
            SkeletonLink::Ambiguous { id, paths } => (
                format!("Ambiguous {}", id.short()),
                format!(
                    "Claimed by: {}",
                    paths
                        .iter()
                        .map(|p| display_path(p, &self.project_root))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Tag::danger().small().child("Ambiguous").into_any_element(),
            ),
        };

        let trigger = Button::new("skeleton-ref")
            .label(label)
            .small()
            .outline()
            .dropdown_caret(true)
            .tooltip(tooltip)
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
            .child(status)
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

    /// Rows under the toolbar for the one-off notice and any issues.
    pub(crate) fn render_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let editor = cx.entity();
        let mut rows: Vec<(Severity, String)> = Vec::new();
        if let Some(notice) = &self.notice {
            rows.push((Severity::Info, notice.clone()));
        }
        rows.extend(self.issues.iter().map(|i| (i.severity, i.message.clone())));
        // Files sharing the referenced id, by project-relative name.
        let copies = match &self.link {
            SkeletonLink::Ambiguous { paths, .. } => Some(
                paths
                    .iter()
                    .map(|p| (display_path(p, &self.project_root), p.clone()))
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        };
        if rows.is_empty() {
            return None;
        }
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
                .when_some(copies, |column, paths| {
                    column.child(
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
                            .children(paths.into_iter().enumerate().map(|(i, (shown, path))| {
                                let editor = editor.clone();
                                Button::new(("skeleton-new-id", i))
                                    .label(shown)
                                    .small()
                                    .outline()
                                    .tooltip("Make this file a distinct skeleton; the others keep the shared id")
                                    .on_click(move |_, _window, cx| {
                                        editor.update(cx, |editor, cx| editor.regenerate_skeleton_id(path.clone(), cx));
                                    })
                            })),
                    )
                })
                .into_any_element(),
        )
    }
}

/// `path` relative to the project, with `/` separators.
fn display_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

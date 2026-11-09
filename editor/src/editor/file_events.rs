//! Keeping an open sequence (or skeleton) current with the files it shows.
//!
//! The panel only scanned the project when it opened, when a reference was
//! picked and after a new-id fix, so a skeleton or preview mesh that changed on
//! disk afterwards left the editor showing the old bones, banner state and
//! mesh. The panel now listens to the engine file events (the same ones that
//! keep the project file index current) and rescans when one concerns what it
//! references; the scan re-resolves both links, which refreshes the banner
//! state (`refresh_issues`), the pose (the skeleton revision) and reloads the
//! preview mesh.
//!
//! [`WatchedFiles::react`] is the pure decision, tested without a window.

use std::path::{Path, PathBuf};

use engine_fs::FsChangeKind;
use gpui::*;

use crate::core::assets::SKELETON_EXTENSION;
use crate::core::library::same_file;
use crate::core::skmesh::SKMESH_EXTENSION;
use crate::core::{MeshLink, SkeletonLink};

use super::panel::{EditorMode, SkeletalAnimEditorPanel};

/// What the panel references, as far as file events are concerned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WatchedFiles {
    /// The asset being edited: its own saves are not a reason to reload.
    pub own: Option<PathBuf>,
    /// The panel resolves a skeleton reference (sequence and mesh editors).
    pub watches_skeleton: bool,
    pub skeleton: Option<PathBuf>,
    pub skeleton_unresolved: bool,
    /// The panel previews a mesh (skeleton and sequence editors).
    pub watches_preview: bool,
    pub preview: Option<PathBuf>,
    pub preview_unresolved: bool,
}

/// What one file event asks of the panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Reaction {
    /// Scan the project again and re-resolve the links.
    pub rescan: bool,
    /// The preview mesh file itself changed: load it again even though it is
    /// the same file.
    pub reload_preview: bool,
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

impl WatchedFiles {
    pub(crate) fn of(panel: &SkeletalAnimEditorPanel) -> Self {
        Self {
            own: panel.current_asset_path.clone(),
            watches_skeleton: panel.mode != EditorMode::Skeleton && panel.skeleton_ref.is_some(),
            skeleton: match &panel.link {
                SkeletonLink::Resolved(entry) if !entry.path.as_os_str().is_empty() => {
                    Some(entry.path.clone())
                }
                _ => None,
            },
            skeleton_unresolved: !matches!(panel.link, SkeletonLink::Resolved(_)),
            watches_preview: panel.mode != EditorMode::Mesh && panel.preview_mesh_ref.is_some(),
            preview: match &panel.preview_link {
                MeshLink::Resolved(entry) => Some(entry.path.clone()),
                _ => panel.preview_loaded_from.clone(),
            },
            preview_unresolved: !matches!(panel.preview_link, MeshLink::Resolved(_)),
        }
    }

    /// How the panel reacts to `path` having something happen to it. A file that
    /// is the linked one always counts; while a link does not resolve (missing,
    /// ambiguous, still resolving) any file of that kind might fix it.
    pub(crate) fn react(&self, path: &Path) -> Reaction {
        if self.own.as_deref().is_some_and(|own| same_file(own, path)) {
            return Reaction::default();
        }
        let mut reaction = Reaction::default();
        if self.watches_skeleton {
            let linked = self.skeleton.as_deref().is_some_and(|p| same_file(p, path));
            let candidate = self.skeleton_unresolved && has_extension(path, SKELETON_EXTENSION);
            reaction.rescan |= linked || candidate;
        }
        if self.watches_preview {
            let linked = self.preview.as_deref().is_some_and(|p| same_file(p, path));
            let candidate = self.preview_unresolved && has_extension(path, SKMESH_EXTENSION);
            if linked || candidate {
                reaction.rescan = true;
                reaction.reload_preview |= linked;
            }
        }
        reaction
    }
}

impl SkeletalAnimEditorPanel {
    /// Follow the engine file events for as long as the panel lives.
    pub(crate) fn watch_files(&mut self, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            let mut events = engine_fs::subscribe();
            loop {
                match events.recv().await {
                    Ok(event) => {
                        let alive = this
                            .update(cx, |this, cx| this.on_file_event(&event.path, cx))
                            .is_ok();
                        if !alive {
                            break;
                        }
                    }
                    // a burst overran the channel: the files may have changed, rescan
                    Err(error) if !error.to_string().contains("closed") => {
                        if this
                            .update(cx, |this, cx| this.rescan_after_lag(cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        self.file_watch = Some(task);
    }

    pub(crate) fn on_file_event(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !path.starts_with(&self.project_root) {
            return;
        }
        let reaction = WatchedFiles::of(self).react(path);
        if reaction.reload_preview {
            self.preview_loaded_from = None;
        }
        if reaction.rescan {
            self.rescan_library(cx);
        }
    }

    fn rescan_after_lag(&mut self, cx: &mut Context<Self>) {
        let watched = WatchedFiles::of(self);
        if watched.watches_skeleton || watched.watches_preview {
            if watched.watches_preview {
                self.preview_loaded_from = None;
            }
            self.rescan_library(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Reaction, WatchedFiles};
    use std::path::Path;

    fn sequence(skeleton: &str, preview: &str) -> WatchedFiles {
        WatchedFiles {
            own: Some("/p/walk.animseq".into()),
            watches_skeleton: true,
            skeleton: Some(skeleton.into()),
            skeleton_unresolved: false,
            watches_preview: true,
            preview: Some(preview.into()),
            preview_unresolved: false,
        }
    }

    #[test]
    fn the_linked_skeleton_rescans() {
        let w = sequence("/p/hero.skeleton", "/p/hero.skmesh");
        let r = w.react(Path::new("/p/hero.skeleton"));
        assert_eq!(
            r,
            Reaction {
                rescan: true,
                reload_preview: false
            }
        );
    }

    #[test]
    fn the_linked_preview_mesh_reloads_even_though_it_is_the_same_file() {
        let w = sequence("/p/hero.skeleton", "/p/hero.skmesh");
        let r = w.react(Path::new("/p/hero.skmesh"));
        assert_eq!(
            r,
            Reaction {
                rescan: true,
                reload_preview: true
            }
        );
    }

    #[test]
    fn unrelated_and_own_files_do_nothing() {
        let w = sequence("/p/hero.skeleton", "/p/hero.skmesh");
        for path in [
            "/p/other.skeleton",
            "/p/other.skmesh",
            "/p/rock.png",
            "/p/walk.animseq",
        ] {
            assert_eq!(w.react(Path::new(path)), Reaction::default(), "{path}");
        }
    }

    #[test]
    fn an_unresolved_link_listens_for_any_file_of_its_kind() {
        let mut w = sequence("/p/hero.skeleton", "/p/hero.skmesh");
        w.skeleton = None;
        w.skeleton_unresolved = true;
        w.preview = None;
        w.preview_unresolved = true;
        assert_eq!(
            w.react(Path::new("/p/new.skeleton")),
            Reaction {
                rescan: true,
                reload_preview: false
            }
        );
        // a new mesh may be the missing preview, but nothing is loaded to refresh
        assert_eq!(
            w.react(Path::new("/p/new.skmesh")),
            Reaction {
                rescan: true,
                reload_preview: false
            }
        );
        assert_eq!(w.react(Path::new("/p/rock.png")), Reaction::default());
    }

    #[test]
    fn a_skeleton_editor_ignores_skeleton_files_and_a_mesh_editor_ignores_meshes() {
        let mut w = sequence("/p/hero.skeleton", "/p/hero.skmesh");
        w.watches_skeleton = false;
        assert_eq!(w.react(Path::new("/p/hero.skeleton")), Reaction::default());
        let mut w = sequence("/p/hero.skeleton", "/p/hero.skmesh");
        w.watches_preview = false;
        assert_eq!(w.react(Path::new("/p/hero.skmesh")), Reaction::default());
    }
}

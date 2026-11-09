//! skeleton-only commands, reference checks and document history wiring.

use gpui::*;
use plugin_editor_api::{undo, DocumentId, UndoService};
use skeletal_assets::skeleton::Bone;
use skeletal_assets::sockets::Socket;

use super::hierarchy_history::{HierarchyEdit, Selection};
use super::panel::{EditorMode, SkeletalAnimEditorPanel, TracerState};
use crate::core::{hierarchy_references, Transform};

#[derive(Clone)]
pub(crate) enum HierarchyCommand {
    AddBone { parent: Option<String> },
    RenameBone { id: String, name: String },
    DeleteBone { id: String, subtree: bool },
    Reparent { id: String, parent: Option<String> },
    AddSocket { bone: String },
    RenameSocket { id: String, name: String },
    ReattachSocket { id: String, bone: String },
    DeleteSocket(String),
    Transform(Transform),
}

impl HierarchyCommand {
    fn label(&self) -> &'static str {
        match self {
            Self::AddBone { .. } => "Add bone",
            Self::RenameBone { .. } => "Rename bone",
            Self::DeleteBone { subtree: true, .. } => "Delete subtree",
            Self::DeleteBone { .. } => "Delete bone",
            Self::Reparent { .. } => "Reparent bone",
            Self::AddSocket { .. } => "Add socket",
            Self::RenameSocket { .. } => "Rename socket",
            Self::ReattachSocket { .. } => "Reattach socket",
            Self::DeleteSocket(_) => "Delete socket",
            Self::Transform(_) => "Edit transform",
        }
    }
}

impl SkeletalAnimEditorPanel {
    pub(crate) fn seal_viewport_transform(&self) {
        undo::lock(&self.hierarchy_history).seal();
    }

    /// Apply an absolute viewport transform sample through the skeleton
    /// document's undoable transform path.
    pub(crate) fn apply_viewport_transform(
        &mut self,
        transform: crate::core::Transform,
        cx: &mut Context<Self>,
    ) {
        if self.mode != EditorMode::Skeleton || !self.transform_editable() {
            return;
        }
        let selection = self.hierarchy_selection();
        let before_bone = self
            .selected_bone
            .as_deref()
            .filter(|_| self.selected_socket.is_none())
            .and_then(|id| {
                self.skeleton
                    .bones
                    .iter()
                    .enumerate()
                    .find(|(_, b)| b.id == id)
            })
            .map(|(index, bone)| (index, bone.clone()));
        let before_socket = self
            .selected_socket
            .as_deref()
            .and_then(|id| self.sockets.get(id))
            .cloned();
        if let Err(error) = self.mutate_hierarchy(HierarchyCommand::Transform(transform)) {
            self.notice = Some(error);
            cx.notify();
            return;
        }
        let edit = if let Some(before) = before_socket {
            self.sockets
                .get(&before.id)
                .cloned()
                .and_then(|after| HierarchyEdit::socket_transform(before, after, selection))
        } else {
            before_bone.and_then(|(index, before)| {
                self.skeleton.bones.get(index).and_then(|after| {
                    HierarchyEdit::bone_transform(index, before, after.clone(), selection)
                })
            })
        };
        if let Some(edit) = edit {
            undo::lock(&self.hierarchy_history).push(edit);
            self.notice = None;
            self.skeleton_revision += 1;
            self.hovered_bone = None;
            self.is_dirty = self.asset_id != self.saved_asset_id
                || self.skeleton != self.saved_hierarchy
                || self.sockets != self.saved_sockets
                || self.preview_mesh_ref != self.saved_preview_ref;
            self.refresh_issues();
            cx.notify();
        }
    }

    pub fn set_undo_service(&mut self, service: &UndoService) {
        if self.mode != EditorMode::Skeleton {
            return;
        }
        if let Some(path) = &self.current_asset_path {
            service.adopt(&DocumentId::for_path(path), &self.hierarchy_history);
        }
    }

    pub(crate) fn hierarchy_selection(&self) -> Selection {
        Selection {
            bone: self.selected_bone.clone(),
            socket: self.selected_socket.clone(),
        }
    }

    pub(crate) fn select_socket(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(socket) = self.sockets.get(&id) else {
            return;
        };
        let bone = socket.bone_id.clone();
        self.select_bone(Some(bone.clone()), window, cx);
        self.expanded_bones.insert(bone);
        self.selected_socket = Some(id);
        self.sync_transform_inputs(window, cx);
        cx.notify();
    }

    pub(crate) fn request_hierarchy_command(
        &mut self,
        command: HierarchyCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode != EditorMode::Skeleton {
            return;
        }
        let destructive = matches!(
            command,
            HierarchyCommand::DeleteBone { .. } | HierarchyCommand::Reparent { .. }
        );
        if !destructive {
            self.apply_hierarchy_command(command, true, window, cx);
            return;
        }
        if self.checking_hierarchy {
            return;
        }
        let (id, reparent) = match &command {
            HierarchyCommand::DeleteBone { id, subtree } => {
                if !subtree && !self.skeleton.children_of(id).is_empty() {
                    self.notice = Some("This bone has children. Use Delete Subtree.".into());
                    cx.notify();
                    return;
                }
                (id, false)
            }
            HierarchyCommand::Reparent { id, parent } => {
                // reject invalid targets before scanning the project.
                let mut candidate = self.skeleton.clone();
                if let Err(error) = candidate.reparent_bone(id, parent.as_deref()) {
                    self.notice = Some(error.to_string());
                    cx.notify();
                    return;
                }
                if candidate == self.skeleton {
                    return;
                }
                (id, true)
            }
            _ => return,
        };
        let affected = match self.skeleton.subtree_ids(id) {
            Ok(ids) => ids,
            Err(error) => {
                self.notice = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let answer = if reparent {
            None
        } else {
            let count = self
                .sockets
                .iter()
                .filter(|s| affected.contains(&s.bone_id))
                .count();
            let detail = format!("Delete {} bone(s) and {count} attached socket(s)? This can be undone. Dependent animation, mesh and physics assets will be checked first.", affected.len());
            Some(window.prompt(
                PromptLevel::Warning,
                "Delete bones?",
                Some(&detail),
                &["Cancel", "Delete"],
                cx,
            ))
        };
        let root = self.project_root.clone();
        let skeleton_id = self.asset_id.clone();
        let revision = self.skeleton_revision;
        self.checking_hierarchy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let confirmed = match answer {
                Some(answer) => answer.await.ok() == Some(1),
                None => true,
            };
            let result = if confirmed {
                cx.background_executor().spawn(async move {
                    match skeleton_id {
                        Some(id) => hierarchy_references::check(&root, &id, &affected, reparent),
                        None => Err("Save the skeleton before changing existing hierarchy relationships.".into()),
                    }
                }).await
            } else {
                Ok(())
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.checking_hierarchy = false;
                if confirmed {
                    if revision != this.skeleton_revision {
                        this.notice = Some("The skeleton changed during the reference check. Try the command again.".into());
                    } else {
                        match result {
                            Err(error) => this.notice = Some(error),
                            Ok(()) => this.apply_hierarchy_command(command, true, window, cx),
                        }
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn apply_hierarchy_command(
        &mut self,
        command: HierarchyCommand,
        sync_inputs: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode != EditorMode::Skeleton {
            return;
        }
        if let HierarchyCommand::Transform(transform) = command {
            self.edit_transform(transform, sync_inputs, window, cx);
            return;
        }
        let label = command.label();
        let edits_bones = matches!(
            command,
            HierarchyCommand::AddBone { .. }
                | HierarchyCommand::RenameBone { .. }
                | HierarchyCommand::DeleteBone { .. }
                | HierarchyCommand::Reparent { .. }
        );
        let edits_sockets = !edits_bones || matches!(command, HierarchyCommand::DeleteBone { .. });
        let before = if edits_bones {
            self.skeleton.clone()
        } else {
            Default::default()
        };
        let old_sockets = edits_sockets.then(|| self.sockets.clone());
        let old_selection = self.hierarchy_selection();
        let result = self.mutate_hierarchy(command);
        if let Err(error) = result {
            if edits_bones {
                self.skeleton = before;
            }
            if let Some(sockets) = old_sockets {
                self.sockets = sockets;
            }
            self.selected_bone = old_selection.bone;
            self.selected_socket = old_selection.socket;
            self.notice = Some(error);
            cx.notify();
            return;
        }
        let empty = Default::default();
        if let Some(edit) = HierarchyEdit::between(
            label,
            &before,
            if edits_bones { &self.skeleton } else { &empty },
            old_sockets,
            &self.sockets,
            (old_selection, self.hierarchy_selection()),
        ) {
            undo::lock(&self.hierarchy_history).push(edit);
            self.notice = None;
            self.hierarchy_revision += 1;
            self.hierarchy_changed(sync_inputs, window, cx);
        }
    }

    fn edit_transform(
        &mut self,
        transform: Transform,
        sync_inputs: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selection = self.hierarchy_selection();
        let before_bone = self
            .selected_bone
            .as_deref()
            .filter(|_| self.selected_socket.is_none())
            .and_then(|id| {
                self.skeleton
                    .bones
                    .iter()
                    .enumerate()
                    .find(|(_, b)| b.id.as_str() == id)
            })
            .map(|(i, b)| (i, b.clone()));
        let before_socket = self
            .selected_socket
            .as_deref()
            .and_then(|id| self.sockets.get(id))
            .cloned();
        if let Err(error) = self.mutate_hierarchy(HierarchyCommand::Transform(transform)) {
            self.notice = Some(error);
            cx.notify();
            return;
        }
        let edit = if let Some(before) = before_socket {
            self.sockets
                .get(&before.id)
                .cloned()
                .and_then(|after| HierarchyEdit::socket_transform(before, after, selection))
        } else {
            before_bone.and_then(|(i, before)| {
                self.skeleton.bones.get(i).and_then(|after| {
                    HierarchyEdit::bone_transform(i, before, after.clone(), selection)
                })
            })
        };
        if let Some(edit) = edit {
            undo::lock(&self.hierarchy_history).push(edit);
            self.notice = None;
            self.hierarchy_changed(sync_inputs, window, cx);
        }
    }

    fn mutate_hierarchy(&mut self, command: HierarchyCommand) -> Result<(), String> {
        self.sockets
            .validate(&self.skeleton)
            .map_err(|e| e.to_string())?;
        let result = match command {
            HierarchyCommand::AddBone { parent } => {
                let id = uuid::Uuid::new_v4().to_string();
                self.skeleton
                    .add_bone(Bone {
                        id: id.clone(),
                        name: "New Bone".into(),
                        parent,
                        bind_transform: Transform::IDENTITY,
                    })
                    .map_err(|e| e.to_string())?;
                self.selected_bone = Some(id);
                self.selected_socket = None;
                Ok(())
            }
            HierarchyCommand::RenameBone { id, name } => self.skeleton.rename_bone(&id, name),
            HierarchyCommand::DeleteBone { id, subtree } => {
                let ids = self.skeleton.subtree_ids(&id).map_err(|e| e.to_string())?;
                let parent = self.skeleton.bone(&id).and_then(|b| b.parent.clone());
                if subtree {
                    self.skeleton.remove_subtree(&id)
                } else {
                    self.skeleton.remove_bone(&id)
                }
                .map_err(|e| e.to_string())?;
                self.sockets.remove_for_bones(&ids);
                if self
                    .selected_bone
                    .as_ref()
                    .is_some_and(|id| ids.contains(id))
                {
                    self.selected_bone = parent;
                    self.selected_socket = None;
                }
                Ok(())
            }
            HierarchyCommand::Reparent { id, parent } => {
                self.skeleton.reparent_bone(&id, parent.as_deref())
            }
            HierarchyCommand::AddSocket { bone } => {
                let id = uuid::Uuid::new_v4().to_string();
                let name = (1..)
                    .map(|i| format!("Socket {i}"))
                    .find(|name| !self.sockets.iter().any(|s| &s.name == name))
                    .ok_or("Cannot allocate a socket name.")?;
                self.sockets
                    .add(
                        &self.skeleton,
                        Socket {
                            id: id.clone(),
                            name,
                            bone_id: bone.clone(),
                            local_transform: Transform::IDENTITY,
                        },
                    )
                    .map_err(|e| e.to_string())?;
                self.selected_socket = Some(id);
                self.selected_bone = Some(bone);
                Ok(())
            }
            HierarchyCommand::RenameSocket { id, name } => {
                let mut socket = self
                    .sockets
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| format!("Socket '{id}' does not exist."))?;
                socket.name = name;
                self.sockets
                    .set(&self.skeleton, socket)
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            HierarchyCommand::ReattachSocket { id, bone } => {
                let mut socket = self
                    .sockets
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| format!("Socket '{id}' does not exist."))?;
                socket.bone_id = bone.clone();
                self.sockets
                    .set(&self.skeleton, socket)
                    .map_err(|e| e.to_string())?;
                if self.selected_socket.as_ref() == Some(&id) {
                    self.selected_bone = Some(bone);
                }
                Ok(())
            }
            HierarchyCommand::DeleteSocket(id) => {
                self.sockets.remove(&id).map_err(|e| e.to_string())?;
                if self.selected_socket.as_ref() == Some(&id) {
                    self.selected_socket = None;
                }
                Ok(())
            }
            HierarchyCommand::Transform(transform) => {
                if let Some(id) = self.selected_socket.as_deref() {
                    let socket = self
                        .sockets
                        .get(id)
                        .ok_or_else(|| format!("Socket '{id}' does not exist."))?;
                    let mut socket = socket.clone();
                    socket.local_transform = transform;
                    self.sockets
                        .set(&self.skeleton, socket)
                        .map_err(|e| e.to_string())?;
                    Ok(())
                } else if let Some(id) = &self.selected_bone {
                    self.skeleton.set_bind_transform(id, transform)
                } else {
                    return Ok(());
                }
            }
        };
        result.map_err(|e| e.to_string())
    }

    pub(crate) fn hierarchy_changed(
        &mut self,
        sync_inputs: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.skeleton_revision += 1;
        if sync_inputs {
            let ids: std::collections::HashSet<_> =
                self.skeleton.bones.iter().map(|b| b.id.as_str()).collect();
            self.expanded_bones.retain(|id| ids.contains(id.as_str()));
            if let Some(id) = self.selected_bone.clone() {
                self.expand_ancestors(&id);
                self.expanded_bones.insert(id);
            }
            self.tracer_state = TracerState::new(&self.skeleton);
        } else {
            self.tracer_state.trails.clear();
        }
        self.hovered_bone = None;
        self.is_dirty = self.asset_id != self.saved_asset_id
            || self.skeleton != self.saved_hierarchy
            || self.sockets != self.saved_sockets
            || self.preview_mesh_ref != self.saved_preview_ref;
        if sync_inputs {
            self.sync_transform_inputs(window, cx);
        }
        self.refresh_issues();
        cx.notify();
    }

    pub(crate) fn undo_hierarchy(
        &mut self,
        redo: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode != EditorMode::Skeleton || self.checking_hierarchy {
            return;
        }
        let history = self.hierarchy_history.clone();
        let mut history = undo::lock(&history);
        let mut selection = None;
        let mut apply = |edit: &mut HierarchyEdit| {
            selection = Some(edit.apply(&mut self.skeleton, &mut self.sockets, redo));
            true
        };
        let changed = if redo {
            history.redo(&mut apply)
        } else {
            history.undo(&mut apply)
        };
        drop(history);
        if changed.is_some() {
            if let Some(selected) = selection {
                self.selected_bone = selected.bone;
                self.selected_socket = selected.socket;
            }
            self.notice = None;
            self.hierarchy_revision += 1;
            self.hierarchy_changed(true, window, cx);
        }
    }

    pub(crate) fn check_saved_hierarchy_references(&self) -> Result<(), String> {
        let (removed, reparented) = self.saved_hierarchy_changes();
        let Some(id) = &self.asset_id else {
            return Ok(());
        };
        if !removed.is_empty() {
            hierarchy_references::check(&self.project_root, id, &removed, false)?;
        }
        if reparented {
            hierarchy_references::check(&self.project_root, id, &removed, true)?;
        }
        Ok(())
    }
    fn saved_hierarchy_changes(&self) -> (std::collections::HashSet<String>, bool) {
        let current: std::collections::HashMap<_, _> = self
            .skeleton
            .bones
            .iter()
            .map(|b| (b.id.as_str(), b))
            .collect();
        let mut removed = std::collections::HashSet::new();
        let mut reparented = false;
        for bone in &self.saved_hierarchy.bones {
            match current.get(bone.id.as_str()) {
                None => {
                    removed.insert(bone.id.clone());
                }
                Some(now) => reparented |= now.parent != bone.parent,
            }
        }
        (removed, reparented)
    }

    pub(crate) fn save_hierarchy_async(&mut self, cx: &mut Context<Self>) {
        if self.saving_hierarchy || self.current_asset_path.is_none() {
            return;
        }
        undo::lock(&self.hierarchy_history).seal();
        let (removed, reparented) = self.saved_hierarchy_changes();
        let root = self.project_root.clone();
        let id = self.asset_id.clone();
        let revision = self.skeleton_revision;
        let path = self.current_asset_path.clone();
        let preview = self.preview_mesh_ref.clone();
        self.saving_hierarchy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let scan_id = id.clone();
            let result = cx.background_executor().spawn(async move {
                if let Some(id) = scan_id {
                    if !removed.is_empty() {
                        hierarchy_references::check(&root, &id, &removed, false)?;
                    }
                    if reparented {
                        hierarchy_references::check(&root, &id, &removed, true)?;
                    }
                }
                Ok::<_, String>(())
            }).await;
            let _ = this.update(cx, |this, cx| {
                this.saving_hierarchy = false;
                let result = if revision != this.skeleton_revision || path != this.current_asset_path
                    || id != this.asset_id || preview != this.preview_mesh_ref {
                    Err("The skeleton changed during Save. Save again to check the current revision.".into())
                } else {
                    result.and_then(|()| this.save_hierarchy_validated().map_err(|e| e.to_string()))
                };
                this.notice = result.err().map(|e| format!("Could not save: {e}"));
                cx.notify();
            });
        }).detach();
    }
}

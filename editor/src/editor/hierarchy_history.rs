//! sparse bone changes use the host's bounded per-document history.

use crate::core::skeleton::Bone;
use crate::core::Skeleton;
use plugin_editor_api::UndoEntry;
use skeletal_assets::sockets::{Socket, Sockets};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    pub bone: Option<String>,
    pub socket: Option<String>,
}

type SocketEntries = Vec<(usize, Socket)>;

pub(crate) struct HierarchyEdit {
    label: &'static str,
    before: Vec<(usize, Bone)>,
    after: Vec<(usize, Bone)>,
    sockets: Option<(SocketEntries, SocketEntries)>,
    socket_transform: Option<(Socket, Socket)>,
    selection: (Selection, Selection),
}

impl HierarchyEdit {
    pub fn bone_transform(
        index: usize,
        before: Bone,
        after: Bone,
        selection: Selection,
    ) -> Option<Self> {
        if before == after {
            return None;
        }
        Some(Self {
            label: "Edit transform",
            before: vec![(index, before)],
            after: vec![(index, after)],
            sockets: None,
            socket_transform: None,
            selection: (selection.clone(), selection),
        })
    }

    pub fn socket_transform(before: Socket, after: Socket, selection: Selection) -> Option<Self> {
        (before != after).then(|| Self {
            label: "Edit transform",
            before: Vec::new(),
            after: Vec::new(),
            sockets: None,
            socket_transform: Some((before, after)),
            selection: (selection.clone(), selection),
        })
    }

    pub fn between(
        label: &'static str,
        before: &Skeleton,
        after: &Skeleton,
        old_sockets: Option<Sockets>,
        sockets: &Sockets,
        selection: (Selection, Selection),
    ) -> Option<Self> {
        let changed = |source: &Skeleton, other: &Skeleton| {
            let by_id: std::collections::HashMap<_, _> =
                other.bones.iter().map(|b| (b.id.as_str(), b)).collect();
            source
                .bones
                .iter()
                .enumerate()
                .filter(|(_, b)| by_id.get(b.id.as_str()).copied() != Some(*b))
                .map(|(i, b)| (i, b.clone()))
                .collect::<Vec<_>>()
        };
        let before_bones = changed(before, after);
        let after_bones = changed(after, before);
        let sockets = old_sockets.and_then(|old| {
            let changed = |source: &Sockets, other: &Sockets| {
                let by_id: std::collections::HashMap<_, _> =
                    other.iter().map(|s| (s.id.as_str(), s)).collect();
                source
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| by_id.get(s.id.as_str()).copied() != Some(*s))
                    .map(|(i, s)| (i, s.clone()))
                    .collect::<SocketEntries>()
            };
            let before = changed(&old, sockets);
            let after = changed(sockets, &old);
            (!before.is_empty() || !after.is_empty()).then_some((before, after))
        });
        if before_bones.is_empty() && after_bones.is_empty() && sockets.is_none() {
            return None;
        }
        Some(Self {
            label,
            before: before_bones,
            after: after_bones,
            sockets,
            socket_transform: None,
            selection,
        })
    }

    pub fn apply(&self, skeleton: &mut Skeleton, sockets: &mut Sockets, redo: bool) -> Selection {
        let (remove, insert) = if redo {
            (&self.before, &self.after)
        } else {
            (&self.after, &self.before)
        };
        if remove.len() == 1
            && insert.len() == 1
            && remove[0].0 == insert[0].0
            && remove[0].1.id == insert[0].1.id
        {
            skeleton.bones[insert[0].0] = insert[0].1.clone();
        } else if !remove.is_empty() || !insert.is_empty() {
            let removed_ids: std::collections::HashSet<_> =
                remove.iter().map(|(_, b)| b.id.as_str()).collect();
            let capacity = skeleton.bones.len() + insert.len();
            let existing = std::mem::take(&mut skeleton.bones);
            let mut remaining = existing
                .into_iter()
                .filter(|b| !removed_ids.contains(b.id.as_str()));
            let mut changes = insert.iter().peekable();
            let mut bones = Vec::with_capacity(capacity);
            loop {
                if let Some((index, bone)) = changes.peek() {
                    if *index <= bones.len() {
                        bones.push(bone.clone());
                        changes.next();
                        continue;
                    }
                }
                match remaining.next() {
                    Some(bone) => bones.push(bone),
                    None => {
                        bones.extend(changes.map(|(_, b)| b.clone()));
                        break;
                    }
                }
            }
            skeleton.bones = bones;
        }
        if let Some((before, after)) = &self.sockets {
            let (remove, insert) = if redo {
                (before, after)
            } else {
                (after, before)
            };
            for (_, socket) in remove {
                sockets.remove(&socket.id).expect("valid history socket");
            }
            for (index, socket) in insert {
                sockets
                    .insert_at(skeleton, *index, socket.clone())
                    .expect("valid history socket");
            }
        }
        if let Some((before, after)) = &self.socket_transform {
            sockets
                .set(skeleton, if redo { after } else { before }.clone())
                .expect("valid history socket");
        }
        if redo {
            &self.selection.1
        } else {
            &self.selection.0
        }
        .clone()
    }
}

impl UndoEntry for HierarchyEdit {
    fn label(&self) -> String {
        self.label.into()
    }
    fn merge(&mut self, next: &Self) -> bool {
        if self.label != "Edit transform"
            || next.label != self.label
            || self.selection.1 != next.selection.0
        {
            return false;
        }
        if self.after != next.before
            || self.sockets.as_ref().map(|s| &s.1) != next.sockets.as_ref().map(|s| &s.0)
            || self.socket_transform.as_ref().map(|s| &s.1)
                != next.socket_transform.as_ref().map(|s| &s.0)
        {
            return false;
        }
        self.after = next.after.clone();
        if let (Some(ours), Some(theirs)) = (&mut self.sockets, &next.sockets) {
            ours.1 = theirs.1.clone();
        }
        if let (Some(ours), Some(theirs)) = (&mut self.socket_transform, &next.socket_transform) {
            ours.1 = theirs.1.clone();
        }
        self.selection.1 = next.selection.1.clone();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Transform;
    use plugin_editor_api::History;
    #[test]
    fn sparse_history_restores_order_parent_transforms_and_selection() {
        let mut skeleton = Skeleton::default();
        for (id, parent) in [
            ("root", None),
            ("arm", Some("root")),
            ("hand", Some("arm")),
            ("leg", Some("root")),
        ] {
            skeleton
                .add_bone(Bone {
                    id: id.into(),
                    name: id.into(),
                    parent: parent.map(str::to_owned),
                    bind_transform: Transform::IDENTITY,
                })
                .unwrap();
        }
        let original = skeleton.clone();
        let selected = Selection {
            bone: Some("hand".into()),
            socket: None,
        };
        let mut history = History::with_capacity(3);
        let mut sockets = Sockets::default();
        for (label, operation) in [
            ("Rename bone", 0),
            ("Reparent bone", 1),
            ("Delete subtree", 2),
        ] {
            let before = skeleton.clone();
            match operation {
                0 => skeleton.rename_bone("hand", "Palm".into()).unwrap(),
                1 => skeleton.reparent_bone("arm", Some("leg")).unwrap(),
                _ => {
                    skeleton.remove_subtree("arm").unwrap();
                }
            }
            history.push(
                HierarchyEdit::between(
                    label,
                    &before,
                    &skeleton,
                    Some(sockets.clone()),
                    &sockets,
                    (selected.clone(), selected.clone()),
                )
                .unwrap(),
            );
        }
        let final_state = skeleton.clone();
        while history.can_undo() {
            history.undo(|edit| {
                assert_eq!(edit.apply(&mut skeleton, &mut sockets, false), selected);
                skeleton.validate().unwrap();
                true
            });
        }
        assert_eq!(skeleton, original);
        assert!(!history.is_dirty());
        while history.can_redo() {
            history.redo(|edit| {
                edit.apply(&mut skeleton, &mut sockets, true);
                skeleton.validate().unwrap();
                true
            });
        }
        assert_eq!(skeleton, final_state);
    }
}

#[cfg(test)]
mod socket_history_tests {
    use super::*;
    use crate::core::Transform;
    use plugin_editor_api::History;
    use skeletal_assets::sockets::Socket;
    #[test]
    fn socket_history_is_bounded_and_transform_changes_merge() {
        let skeleton = Skeleton {
            bones: vec![Bone {
                id: "root".into(),
                name: "Root".into(),
                parent: None,
                bind_transform: Transform::IDENTITY,
            }],
        };
        let selected = Selection {
            bone: Some("root".into()),
            socket: Some("attachment".into()),
        };
        let mut sockets = Sockets::default();
        sockets
            .add(
                &skeleton,
                Socket {
                    id: "attachment".into(),
                    name: "Weapon".into(),
                    bone_id: "root".into(),
                    local_transform: Transform::IDENTITY,
                },
            )
            .unwrap();
        let mut history = History::with_capacity(2);
        for value in [1.0, 2.0, 3.0] {
            let before = sockets.clone();
            let mut socket = sockets.get("attachment").unwrap().clone();
            socket.local_transform.translation.x = value;
            sockets.set(&skeleton, socket).unwrap();
            history.push(
                HierarchyEdit::between(
                    "Edit transform",
                    &skeleton,
                    &skeleton,
                    Some(before),
                    &sockets,
                    (selected.clone(), selected.clone()),
                )
                .unwrap(),
            );
        }
        assert_eq!(history.len(), 1);
        let mut skeleton = skeleton;
        history.undo(|edit| {
            edit.apply(&mut skeleton, &mut sockets, false);
            true
        });
        assert_eq!(
            sockets.get("attachment").unwrap().local_transform,
            Transform::IDENTITY
        );
        history.redo(|edit| {
            edit.apply(&mut skeleton, &mut sockets, true);
            true
        });
        assert_eq!(
            sockets
                .get("attachment")
                .unwrap()
                .local_transform
                .translation
                .x,
            3.0
        );
        history.mark_saved();
        for value in [4.0, 5.0, 6.0] {
            history.seal();
            let before = sockets.clone();
            let mut socket = sockets.get("attachment").unwrap().clone();
            socket.local_transform.translation.x = value;
            sockets.set(&skeleton, socket).unwrap();
            history.push(
                HierarchyEdit::between(
                    "Edit transform",
                    &skeleton,
                    &skeleton,
                    Some(before),
                    &sockets,
                    (selected.clone(), selected.clone()),
                )
                .unwrap(),
            );
        }
        assert_eq!(history.len(), 2);
        assert!(history.is_dirty());
    }
}

#[cfg(test)]
mod allocation_tests {
    use super::*;
    use crate::core::Transform;
    use plugin_editor_api::History;

    #[test]
    fn transform_history_captures_only_the_target_and_replays_in_place() {
        let mut skeleton = Skeleton {
            bones: (0..1000)
                .map(|i| Bone {
                    id: i.to_string(),
                    name: i.to_string(),
                    parent: None,
                    bind_transform: Transform::IDENTITY,
                })
                .collect(),
        };
        let mut sockets = Sockets::default();
        for i in 0..1000 {
            sockets
                .add(
                    &skeleton,
                    Socket {
                        id: i.to_string(),
                        name: i.to_string(),
                        bone_id: i.to_string(),
                        local_transform: Transform::IDENTITY,
                    },
                )
                .unwrap();
        }
        let selection = Selection {
            bone: Some("0".into()),
            socket: None,
        };
        let before = skeleton.bones[0].clone();
        skeleton.bones[0].bind_transform.translation.x = 3.0;
        let bone_edit =
            HierarchyEdit::bone_transform(0, before, skeleton.bones[0].clone(), selection.clone())
                .unwrap();
        assert_eq!(bone_edit.before.len(), 1);
        assert!(bone_edit.sockets.is_none());
        let bones_ptr = skeleton.bones.as_ptr();
        bone_edit.apply(&mut skeleton, &mut sockets, false);
        assert_eq!(skeleton.bones.as_ptr(), bones_ptr);
        let before = sockets.get("0").unwrap().clone();
        let mut after = before.clone();
        after.local_transform.translation.x = 3.0;
        sockets.set(&skeleton, after.clone()).unwrap();
        let socket_edit = HierarchyEdit::socket_transform(
            before,
            after,
            Selection {
                socket: Some("0".into()),
                ..selection
            },
        )
        .unwrap();
        assert!(
            socket_edit.before.is_empty()
                && socket_edit.after.is_empty()
                && socket_edit.sockets.is_none()
        );
        socket_edit.apply(&mut skeleton, &mut sockets, false);
        assert_eq!(skeleton.bones.as_ptr(), bones_ptr);
        assert_eq!(
            sockets.get("0").unwrap().local_transform,
            Transform::IDENTITY
        );
        assert_eq!(
            sockets.get("999").unwrap().local_transform,
            Transform::IDENTITY
        );
        let original_sockets = sockets.clone();
        let mut renamed = sockets.get("500").unwrap().clone();
        renamed.name = "Renamed".into();
        sockets.set(&skeleton, renamed).unwrap();
        let empty = Skeleton::default();
        let selection = Selection {
            bone: Some("500".into()),
            socket: Some("500".into()),
        };
        let rename = HierarchyEdit::between(
            "Rename socket",
            &empty,
            &empty,
            Some(original_sockets.clone()),
            &sockets,
            (selection.clone(), selection.clone()),
        )
        .unwrap();
        assert_eq!(rename.sockets.as_ref().unwrap().0.len(), 1);
        assert_eq!(rename.sockets.as_ref().unwrap().1.len(), 1);
        rename.apply(&mut skeleton, &mut sockets, false);
        assert_eq!(sockets, original_sockets);
        rename.apply(&mut skeleton, &mut sockets, true);
        let before_delete = sockets.clone();
        sockets.remove("1").unwrap();
        sockets.remove("4").unwrap();
        let deletion = HierarchyEdit::between(
            "Delete sockets",
            &empty,
            &empty,
            Some(before_delete.clone()),
            &sockets,
            (selection.clone(), selection),
        )
        .unwrap();
        assert_eq!(deletion.sockets.as_ref().unwrap().0.len(), 2);
        assert!(deletion.sockets.as_ref().unwrap().1.is_empty());
        deletion.apply(&mut skeleton, &mut sockets, false);
        assert_eq!(sockets, before_delete);
        deletion.apply(&mut skeleton, &mut sockets, true);
        assert!(sockets.get("1").is_none() && sockets.get("4").is_none());
        assert_eq!(skeleton.bones.as_ptr(), bones_ptr);
        let mut history = History::default();
        for i in 0..101 {
            history.seal();
            let before = skeleton.bones[0].clone();
            skeleton.bones[0].bind_transform.translation.x = (i + 1) as f32;
            history.push(
                HierarchyEdit::bone_transform(
                    0,
                    before,
                    skeleton.bones[0].clone(),
                    Selection {
                        bone: Some("0".into()),
                        socket: None,
                    },
                )
                .unwrap(),
            );
        }
        assert_eq!(history.len(), 100);
    }
}

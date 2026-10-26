//! named attachment points in bone-local space, independent of display names.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;

use crate::animation::{bone_world_transform, AnimationClip};
use crate::math::{Mat4, Transform};
use crate::skeleton::{validate_transform, HierarchyError, Skeleton};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Socket {
    pub id: String,
    pub name: String,
    pub bone_id: String,
    pub local_transform: Transform,
}

impl Socket {
    pub fn world_transform(
        &self,
        skeleton: &Skeleton,
        clip: &AnimationClip,
        time: f32,
    ) -> Option<Mat4> {
        Some(
            bone_world_transform(skeleton, clip, &self.bone_id, time)?
                .mul(&self.local_transform.to_matrix()),
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sockets(Vec<Socket>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SocketError {
    Hierarchy(HierarchyError),
    DuplicateId(String),
    DuplicateName(String),
    MissingSocket(String),
    EmptyId,
}

impl fmt::Display for SocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hierarchy(error) => error.fmt(f),
            Self::DuplicateId(id) => write!(f, "Socket ID '{id}' already exists."),
            Self::DuplicateName(name) => write!(f, "Socket name '{name}' already exists."),
            Self::MissingSocket(id) => write!(f, "Socket '{id}' does not exist."),
            Self::EmptyId => f.write_str("The socket ID cannot be empty."),
        }
    }
}
impl std::error::Error for SocketError {}
impl From<HierarchyError> for SocketError {
    fn from(error: HierarchyError) -> Self {
        Self::Hierarchy(error)
    }
}

impl Sockets {
    pub fn iter(&self) -> impl Iterator<Item = &Socket> {
        self.0.iter()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn get(&self, id: &str) -> Option<&Socket> {
        self.0.iter().find(|s| s.id == id)
    }

    pub fn validate(&self, skeleton: &Skeleton) -> Result<(), SocketError> {
        skeleton.validate()?;
        let bone_ids: HashSet<_> = skeleton.bones.iter().map(|b| b.id.as_str()).collect();
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for socket in &self.0 {
            if socket.id.trim().is_empty() {
                return Err(SocketError::EmptyId);
            }
            if socket.name.trim().is_empty() {
                return Err(HierarchyError::EmptyName.into());
            }
            if !ids.insert(&socket.id) {
                return Err(SocketError::DuplicateId(socket.id.clone()));
            }
            if !names.insert(&socket.name) {
                return Err(SocketError::DuplicateName(socket.name.clone()));
            }
            if !bone_ids.contains(socket.bone_id.as_str()) {
                return Err(HierarchyError::MissingBone(socket.bone_id.clone()).into());
            }
            validate_transform(&socket.local_transform)?;
        }
        Ok(())
    }

    /// insert or replace one socket atomically; callers cannot mutate the collection unchecked.
    pub fn set(&mut self, skeleton: &Skeleton, socket: Socket) -> Result<(), SocketError> {
        self.validate(skeleton)?;
        if socket.id.trim().is_empty() {
            return Err(SocketError::EmptyId);
        }
        if socket.name.trim().is_empty() {
            return Err(HierarchyError::EmptyName.into());
        }
        if self
            .0
            .iter()
            .any(|s| s.id != socket.id && s.name == socket.name)
        {
            return Err(SocketError::DuplicateName(socket.name));
        }
        if skeleton.bone(&socket.bone_id).is_none() {
            return Err(HierarchyError::MissingBone(socket.bone_id).into());
        }
        validate_transform(&socket.local_transform)?;
        if let Some(existing) = self.0.iter_mut().find(|s| s.id == socket.id) {
            *existing = socket;
        } else {
            self.0.push(socket);
        }
        Ok(())
    }

    pub fn add(&mut self, skeleton: &Skeleton, socket: Socket) -> Result<(), SocketError> {
        if self.get(&socket.id).is_some() {
            return Err(SocketError::DuplicateId(socket.id));
        }
        self.set(skeleton, socket)
    }

    /// insert a validated socket at its prior position when replaying an ordered edit.
    pub fn insert_at(
        &mut self,
        skeleton: &Skeleton,
        index: usize,
        socket: Socket,
    ) -> Result<(), SocketError> {
        self.add(skeleton, socket)?;
        let socket = self.0.pop().expect("socket just inserted");
        self.0.insert(index.min(self.0.len()), socket);
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Result<Socket, SocketError> {
        let index = self
            .0
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| SocketError::MissingSocket(id.into()))?;
        Ok(self.0.remove(index))
    }

    /// used only after a confirmed bone deletion; undo retains the removed attachments.
    pub fn remove_for_bones(&mut self, bones: &HashSet<String>) {
        self.0.retain(|socket| !bones.contains(&socket.bone_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skeleton::Bone;

    fn skeleton() -> Skeleton {
        Skeleton {
            bones: ["root", "hand"]
                .into_iter()
                .map(|id| Bone {
                    id: id.into(),
                    name: id.into(),
                    parent: None,
                    bind_transform: Transform::IDENTITY,
                })
                .collect(),
        }
    }
    fn socket() -> Socket {
        Socket {
            id: "socket".into(),
            name: "Weapon".into(),
            bone_id: "hand".into(),
            local_transform: Transform::IDENTITY,
        }
    }
    #[test]
    fn socket_edits_are_atomic_and_keep_identity() {
        let skeleton = skeleton();
        let mut sockets = Sockets::default();
        sockets.add(&skeleton, socket()).unwrap();
        let before = sockets.clone();
        for invalid in [
            Socket {
                name: "".into(),
                ..socket()
            },
            Socket {
                id: "".into(),
                ..socket()
            },
            Socket {
                bone_id: "missing".into(),
                ..socket()
            },
            Socket {
                local_transform: Transform {
                    translation: crate::math::Vec3::new(f32::NAN, 0.0, 0.0),
                    ..Transform::IDENTITY
                },
                ..socket()
            },
        ] {
            assert!(sockets.set(&skeleton, invalid).is_err());
            assert_eq!(sockets, before);
        }
        sockets
            .set(
                &skeleton,
                Socket {
                    name: "Shield".into(),
                    bone_id: "root".into(),
                    ..socket()
                },
            )
            .unwrap();
        assert_eq!(sockets.get("socket").unwrap().name, "Shield");
        assert_eq!(sockets.get("socket").unwrap().bone_id, "root");
        assert_eq!(sockets.remove("socket").unwrap().id, "socket");
        assert!(sockets.is_empty());
        assert!(sockets.remove("missing").is_err());
    }
    #[test]
    fn malformed_serialized_sockets_fail_validation() {
        let skeleton = skeleton();
        for entries in [
            vec![socket(), socket()],
            vec![
                socket(),
                Socket {
                    id: "other".into(),
                    ..socket()
                },
            ],
            vec![Socket {
                bone_id: "absent".into(),
                ..socket()
            }],
        ] {
            let decoded: Sockets =
                serde_json::from_str(&serde_json::to_string(&entries).unwrap()).unwrap();
            assert!(decoded.validate(&skeleton).is_err());
        }
    }
    #[test]
    fn deleting_a_subtree_removes_only_its_attachments() {
        let skeleton = skeleton();
        let mut sockets = Sockets::default();
        sockets.add(&skeleton, socket()).unwrap();
        sockets
            .add(
                &skeleton,
                Socket {
                    id: "other".into(),
                    name: "Root attachment".into(),
                    bone_id: "root".into(),
                    ..socket()
                },
            )
            .unwrap();
        sockets.remove_for_bones(&HashSet::from(["hand".into()]));
        assert!(sockets.get("socket").is_none());
        assert!(sockets.get("other").is_some());
    }
}

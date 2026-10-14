//! Skeleton data model: bones arranged in a parent/child hierarchy.
//!
//! TODO: Types will come from Helio eventually

use serde::{Deserialize, Serialize};

use super::math::Transform;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bone {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    /// Local bind-pose transform, relative to the parent bone.
    pub bind_transform: Transform,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Skeleton {
    pub bones: Vec<Bone>,
}

impl Skeleton {
    pub fn bone(&self, id: &str) -> Option<&Bone> {
        self.bones.iter().find(|b| b.id == id)
    }

    pub fn bone_mut(&mut self, id: &str) -> Option<&mut Bone> {
        self.bones.iter_mut().find(|b| b.id == id)
    }

    pub fn root_bones(&self) -> Vec<&Bone> {
        self.bones.iter().filter(|b| b.parent.is_none()).collect()
    }

    pub fn children_of<'a>(&'a self, id: &str) -> Vec<&'a Bone> {
        self.bones
            .iter()
            .filter(|b| b.parent.as_deref() == Some(id))
            .collect()
    }

    /// Depth-first iteration order starting from the roots, yielding `(bone, depth)`.
    pub fn depth_first(&self) -> Vec<(&Bone, usize)> {
        let mut out = Vec::with_capacity(self.bones.len());
        for root in self.root_bones() {
            self.push_subtree(root, 0, &mut out);
        }
        out
    }

    fn push_subtree<'a>(&'a self, bone: &'a Bone, depth: usize, out: &mut Vec<(&'a Bone, usize)>) {
        out.push((bone, depth));
        for child in self.children_of(&bone.id) {
            self.push_subtree(child, depth + 1, out);
        }
    }
}

//! Stable asset identity.
//!
//! A skeleton's [`SkeletonId`] is generated once, stored in the file, and never
//! changes. Everything that refers to the skeleton (an animation sequence or a
//! skeletal mesh today, blend spaces later) refers to that id, never to its
//! name or path, so renaming or moving the file breaks nothing. A skeletal
//! mesh has the same kind of id, [`SkeletalMeshId`].
//!
//! This mirrors `WidgetId` in the UI editor plugin. The engine has no shared
//! id service for file assets yet; when it gets one (Pulsar-Native#1322), this
//! module and the plugin's libraries are meant to be lifted into it unchanged.

use serde::{Deserialize, Serialize};

/// Defines an asset id type: a lower-case hyphenated UUID.
macro_rules! asset_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// A fresh random id.
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4().to_string())
            }

            /// The id in `text`, if it is a UUID (any case, braces or not).
            pub fn parse(text: &str) -> Option<Self> {
                uuid::Uuid::parse_str(text.trim().trim_matches(|c| c == '{' || c == '}'))
                    .ok()
                    .map(|u| Self(u.to_string()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// The first group, enough to tell ids apart on screen.
            pub fn short(&self) -> &str {
                self.0.split('-').next().unwrap_or(&self.0)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

asset_id! {
    /// A skeleton's stable id.
    SkeletonId
}

asset_id! {
    /// A skeletal mesh's stable id.
    SkeletalMeshId
}

asset_id! {
    /// An animation sequence's stable id.
    AnimSequenceId
}

use pulsar_reflection::{AnimationSequenceAssetKind, AssetRef, SkeletalMeshAssetKind, SkeletonAssetKind};

impl SkeletonId {
    /// The same id as a typed project asset reference.
    pub fn to_ref(&self) -> AssetRef<SkeletonAssetKind> {
        AssetRef::parse(&self.0).expect("ids are UUIDs")
    }
    pub fn from_ref(reference: &AssetRef<SkeletonAssetKind>) -> Self {
        Self(reference.as_str().to_string())
    }
}

impl SkeletalMeshId {
    pub fn to_ref(&self) -> AssetRef<SkeletalMeshAssetKind> {
        AssetRef::parse(&self.0).expect("ids are UUIDs")
    }
}

impl AnimSequenceId {
    pub fn to_ref(&self) -> AssetRef<AnimationSequenceAssetKind> {
        AssetRef::parse(&self.0).expect("ids are UUIDs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_parse_in_any_spelling_and_print_canonically() {
        let id = SkeletonId::new();
        assert_eq!(SkeletonId::parse(id.as_str()), Some(id.clone()));
        assert_eq!(
            SkeletonId::parse(&id.as_str().to_uppercase()),
            Some(id.clone())
        );
        assert_eq!(
            SkeletonId::parse(&format!("{{{}}}", id.as_str())),
            Some(id.clone())
        );
        assert_eq!(SkeletonId::parse("not an id"), None);
        assert_eq!(SkeletonId::parse(""), None);
        assert_eq!(id.short().len(), 8);
        assert_ne!(SkeletonId::new(), SkeletonId::new());

        let mesh = SkeletalMeshId::new();
        assert_eq!(SkeletalMeshId::parse(&mesh.to_string()), Some(mesh));
    }
}

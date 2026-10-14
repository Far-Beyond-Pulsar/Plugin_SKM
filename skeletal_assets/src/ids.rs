//! Stable asset identity.
//!
//! A skeleton's [`SkeletonId`] is generated once, stored in the file, and never
//! changes. Everything that refers to the skeleton (an animation sequence
//! today, meshes and blend spaces later) refers to that id, never to its name
//! or path, so renaming or moving the file breaks nothing.
//!
//! This mirrors `WidgetId` in the UI editor plugin. The engine has no shared
//! id service for file assets yet; when it gets one, this module and
//! [`super::library`] are meant to be lifted into it unchanged.

use serde::{Deserialize, Serialize};

/// A skeleton's stable id: a lower-case hyphenated UUID.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SkeletonId(String);

impl SkeletonId {
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

impl Default for SkeletonId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SkeletonId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_parse_in_any_spelling_and_print_canonically() {
        let id = SkeletonId::new();
        assert_eq!(SkeletonId::parse(id.as_str()), Some(id.clone()));
        assert_eq!(SkeletonId::parse(&id.as_str().to_uppercase()), Some(id.clone()));
        assert_eq!(SkeletonId::parse(&format!("{{{}}}", id.as_str())), Some(id.clone()));
        assert_eq!(SkeletonId::parse("not an id"), None);
        assert_eq!(SkeletonId::parse(""), None);
        assert_eq!(id.short().len(), 8);
        assert_ne!(SkeletonId::new(), SkeletonId::new());
    }
}

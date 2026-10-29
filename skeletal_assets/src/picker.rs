//! What an asset picker needs to know to offer only sensible assets.
//!
//! The picker UI (Phase 4) lists project assets of one kind. This module is
//! its data side: a [`PickerCandidate`] describes an asset on disk, and
//! [`filter`] narrows candidates to a kind and, optionally, the skeleton the
//! owner works on. Incompatible assets are either hidden or shown flagged, so
//! the user sees why a clip is unavailable rather than wondering.

use crate::dependencies::{extract, kind_of_extension};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerCandidate {
    pub id: String,
    pub kind: &'static str,
    /// The asset's own name, or its file name when it has none.
    pub name: String,
    /// Project-relative path, shown next to the id.
    pub path: String,
    /// The skeleton the asset is for (see `Extracted::primary_skeleton`).
    pub primary_skeleton: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Compat {
    Compatible,
    /// The asset is for another skeleton.
    Incompatible { asset_skeleton: String },
    /// One side has no skeleton, so nothing can be said.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerEntry {
    pub candidate: PickerCandidate,
    pub compat: Compat,
}

impl PickerEntry {
    /// First group of the id, enough to tell assets apart on screen.
    pub fn short_id(&self) -> &str {
        self.candidate.id.split('-').next().unwrap_or(&self.candidate.id)
    }
}

#[derive(Clone, Debug)]
pub struct PickerFilter {
    /// `AssetKind::KIND` to list.
    pub kind: &'static str,
    /// The skeleton of the asset being edited; `None` lists every skeleton.
    pub skeleton: Option<String>,
    /// Keep incompatible assets, flagged, instead of hiding them.
    pub show_incompatible: bool,
}

/// Describe the asset in `bytes` stored at `path`, or `None` if it is not an
/// animation asset or cannot be read.
pub fn describe(path: &str, bytes: &[u8]) -> Option<PickerCandidate> {
    let extension = path.rsplit('.').next()?;
    let kind = kind_of_extension(extension)?;
    let extracted = extract(kind, bytes).ok()?;
    let stem = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
    let name = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| v.get("name")?.as_str().map(str::to_string))
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| stem.to_string());
    Some(PickerCandidate {
        id: extracted.id,
        kind: extracted.kind,
        name,
        path: path.to_string(),
        primary_skeleton: extracted.primary_skeleton,
    })
}

/// Entries of `filter.kind`, sorted by name then id. A skeleton picker (the
/// kind `skeleton`) is never filtered by skeleton.
pub fn filter(candidates: impl IntoIterator<Item = PickerCandidate>, filter: &PickerFilter) -> Vec<PickerEntry> {
    let mut entries: Vec<PickerEntry> = candidates
        .into_iter()
        .filter(|c| c.kind == filter.kind)
        .map(|candidate| {
            let compat = match (&filter.skeleton, &candidate.primary_skeleton) {
                _ if filter.kind == "skeleton" => Compat::Compatible,
                (Some(want), Some(have)) if want == have => Compat::Compatible,
                (Some(_), Some(have)) => Compat::Incompatible { asset_skeleton: have.clone() },
                (None, _) => Compat::Compatible,
                (Some(_), None) => Compat::Unknown,
            };
            PickerEntry { candidate, compat }
        })
        .filter(|e| filter.show_incompatible || !matches!(e.compat, Compat::Incompatible { .. }))
        .collect();
    entries.sort_by(|a, b| {
        (a.candidate.name.to_lowercase(), &a.candidate.id).cmp(&(b.candidate.name.to_lowercase(), &b.candidate.id))
    });
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, kind: &'static str, name: &str, skeleton: Option<&str>) -> PickerCandidate {
        PickerCandidate { id: id.into(), kind, name: name.into(), path: format!("{name}.x"), primary_skeleton: skeleton.map(Into::into) }
    }

    #[test]
    fn filters_by_kind_and_skeleton_and_flags_what_it_keeps() {
        let all = vec![
            candidate("1", "animseq", "Walk", Some("A")),
            candidate("2", "animseq", "run", Some("A")),
            candidate("3", "animseq", "Jump", Some("B")),
            candidate("4", "animseq", "Draft", None),
            candidate("5", "blendspace", "Loco", Some("A")),
        ];
        let want = PickerFilter { kind: "animseq", skeleton: Some("A".into()), show_incompatible: false };
        let names = |e: &[PickerEntry]| e.iter().map(|e| e.candidate.name.clone()).collect::<Vec<_>>();
        let hidden = filter(all.clone(), &want);
        assert_eq!(names(&hidden), ["Draft", "run", "Walk"], "case-insensitive name order; B hidden; unknown kept");
        assert_eq!(hidden[0].compat, Compat::Unknown);

        let shown = filter(all.clone(), &PickerFilter { show_incompatible: true, ..want.clone() });
        let jump = shown.iter().find(|e| e.candidate.name == "Jump").unwrap();
        assert_eq!(jump.compat, Compat::Incompatible { asset_skeleton: "B".into() });

        let any = filter(all.clone(), &PickerFilter { skeleton: None, ..want });
        assert_eq!(any.len(), 4);
        let skeletons = filter([candidate("9", "skeleton", "Humanoid", Some("9"))], &PickerFilter { kind: "skeleton", skeleton: Some("A".into()), show_incompatible: false });
        assert_eq!(skeletons.len(), 1, "a skeleton picker is not filtered by skeleton");
    }

    #[test]
    fn describe_reads_name_kind_and_skeleton_from_the_file() {
        let text = r#"{"version":1,"id":"aaaaaaaa-0000-4000-8000-000000000001","skeleton":"11111111-1111-4111-8111-111111111111","name":"Walk Cycle","duration":1.0,"fps":30.0,"tracks":[]}"#;
        let c = describe("Content/Anim/walk.animseq", text.as_bytes()).unwrap();
        assert_eq!((c.kind, c.name.as_str()), ("animseq", "Walk Cycle"));
        assert_eq!(c.primary_skeleton.as_deref(), Some("11111111-1111-4111-8111-111111111111"));
        assert!(describe("x.png", b"").is_none());
        assert!(describe("x.animseq", b"nope").is_none());
    }
}

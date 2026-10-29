//! Asset references as the editors use them: pickers filtered by kind and
//! skeleton, the status of a stored reference, the dependency tree, the derived
//! Used By list and the breadcrumb of an open asset.
//!
//! Everything goes through [`RefIndex`], implemented over the project file
//! index in production ([`ProjectRefIndex`]) and by an in-memory index in the
//! tests. Nothing here is stored in an asset file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use skeletal_assets::picker::{filter, Compat, PickerCandidate, PickerEntry, PickerFilter};

use crate::core::asset_index::{self, ProjectAssetSource, Referrer};
use skeletal_assets::dependencies::extension_of;

/// Where an id resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Located {
    Missing,
    One { path: PathBuf },
    Ambiguous { paths: Vec<PathBuf> },
}

pub trait RefIndex {
    /// Every asset of `kind`, described for a picker.
    fn candidates(&self, kind: &str) -> Vec<PickerCandidate>;
    fn used_by(&self, kind: &str, id: &str) -> Vec<Referrer>;
    /// `(target kind, target id, authored field)` for what `(kind, id)` references.
    fn deps_of(&self, kind: &str, id: &str) -> Vec<(String, String, String)>;
    fn locate(&self, kind: &str, id: &str) -> Located;
}

/// The production index: the project file index under `root`.
pub struct ProjectRefIndex {
    root: PathBuf,
}

impl ProjectRefIndex {
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf() }
    }
}

impl ProjectRefIndex {
    /// The file of the one asset `(kind, id)` resolves to, for opening it.
    pub fn locate_path(&self, kind: &str, id: &str) -> Option<PathBuf> {
        match self.locate(kind, id) {
            Located::One { path } => Some(path),
            _ => None,
        }
    }
}

impl RefIndex for ProjectRefIndex {
    fn candidates(&self, kind: &str) -> Vec<PickerCandidate> {
        asset_index::picker_candidates(&self.root, kind)
    }
    fn used_by(&self, kind: &str, id: &str) -> Vec<Referrer> {
        asset_index::used_by(&self.root, kind, id)
    }
    fn deps_of(&self, kind: &str, id: &str) -> Vec<(String, String, String)> {
        asset_index::dependencies_of(&self.root, kind, id).into_iter().map(|(k, i, via, _)| (k, i, via)).collect()
    }
    fn locate(&self, kind: &str, id: &str) -> Located {
        let Some(source) = ProjectAssetSource::open(&self.root) else { return Located::Missing };
        let paths = source.index().paths_for_asset_id_kind(kind, id).unwrap_or_default();
        match paths.len() {
            0 => Located::Missing,
            1 => Located::One { path: paths.into_iter().next().unwrap_or_default() },
            _ => Located::Ambiguous { paths },
        }
    }
}

/// A cached [`RefIndex`] that reads each kind's candidate list once, so checking many
/// references against the same project does not rescan its files each time.
pub struct SnapshotIndex {
    inner: ProjectRefIndex,
    candidates: std::cell::RefCell<std::collections::HashMap<String, Vec<PickerCandidate>>>,
}

impl SnapshotIndex {
    pub fn new(root: &Path) -> Self {
        Self { inner: ProjectRefIndex::new(root), candidates: Default::default() }
    }
}

impl RefIndex for SnapshotIndex {
    fn candidates(&self, kind: &str) -> Vec<PickerCandidate> {
        self.candidates.borrow_mut().entry(kind.to_string()).or_insert_with(|| self.inner.candidates(kind)).clone()
    }
    fn used_by(&self, kind: &str, id: &str) -> Vec<Referrer> {
        self.inner.used_by(kind, id)
    }
    fn deps_of(&self, kind: &str, id: &str) -> Vec<(String, String, String)> {
        self.inner.deps_of(kind, id)
    }
    fn locate(&self, kind: &str, id: &str) -> Located {
        self.inner.locate(kind, id)
    }
}

/// What a stored reference currently is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefStatus {
    Unset,
    Ok { name: String },
    /// The id resolves to no asset of this kind.
    Missing,
    /// The id resolves to several files; keep one.
    Ambiguous { count: usize },
    /// The asset exists but is for another skeleton than the owner's.
    Incompatible { name: String, asset_skeleton: String },
}

impl RefStatus {
    pub fn is_problem(&self) -> bool {
        !matches!(self, Self::Unset | Self::Ok { .. })
    }
    /// One-line description with the fix the editor offers.
    pub fn message(&self) -> Option<String> {
        match self {
            Self::Unset | Self::Ok { .. } => None,
            Self::Missing => Some("Asset not found. Pick another or clear the reference.".into()),
            Self::Ambiguous { count } => Some(format!("{count} files share this id. Remove or re-id the duplicates.")),
            Self::Incompatible { name, .. } => Some(format!("`{name}` is for a different skeleton. Pick a compatible asset.")),
        }
    }
}

/// Picker entries of `kind` for an owner on `skeleton`, incompatible ones kept
/// and flagged so the user can see why they are unavailable.
pub fn picker_entries(index: &dyn RefIndex, kind: &'static str, skeleton: Option<&str>) -> Vec<PickerEntry> {
    filter(index.candidates(kind), &PickerFilter { kind, skeleton: skeleton.map(str::to_string), show_incompatible: true })
}

/// The status of `id` as a reference of `kind` held by an owner on `skeleton`.
pub fn ref_status(index: &dyn RefIndex, kind: &'static str, id: Option<&str>, skeleton: Option<&str>) -> RefStatus {
    let Some(id) = id.filter(|i| !i.is_empty()) else { return RefStatus::Unset };
    match index.locate(kind, id) {
        Located::Missing => return RefStatus::Missing,
        Located::Ambiguous { paths } => return RefStatus::Ambiguous { count: paths.len() },
        Located::One { .. } => {}
    }
    let entries = picker_entries(index, kind, skeleton);
    match entries.into_iter().find(|e| e.candidate.id == id) {
        Some(PickerEntry { candidate, compat: Compat::Incompatible { asset_skeleton } }) => {
            RefStatus::Incompatible { name: candidate.name, asset_skeleton }
        }
        Some(entry) => RefStatus::Ok { name: entry.candidate.name },
        // Located but not describable (unreadable): treat as missing for the picker.
        None => RefStatus::Missing,
    }
}

/// Short display name of a picker entry.
pub fn entry_label(entry: &PickerEntry) -> String {
    match &entry.compat {
        Compat::Incompatible { .. } => format!("{} (other skeleton)", entry.candidate.name),
        _ => entry.candidate.name.clone(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DepState {
    Resolved { path: PathBuf },
    Missing,
    Ambiguous { count: usize },
    /// Already shown above in this branch.
    Cycle,
    /// Not expanded: the depth limit.
    Truncated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepNode {
    pub kind: String,
    pub id: String,
    pub via: String,
    pub state: DepState,
    pub children: Vec<DepNode>,
}

impl DepNode {
    /// Every node of the tree, parent before children.
    pub fn flatten(&self) -> Vec<&DepNode> {
        let mut out = vec![self];
        for child in &self.children {
            out.extend(child.flatten());
        }
        out
    }
}

/// What `(kind, id)` references, recursively, to `max_depth` levels. A target
/// that is its own ancestor is marked [`DepState::Cycle`] and not expanded.
pub fn dependency_tree(index: &dyn RefIndex, kind: &str, id: &str, max_depth: usize) -> Vec<DepNode> {
    let mut path = HashSet::new();
    path.insert((kind.to_string(), id.to_string()));
    expand(index, kind, id, max_depth, &mut path)
}

fn expand(index: &dyn RefIndex, kind: &str, id: &str, depth: usize, ancestors: &mut HashSet<(String, String)>) -> Vec<DepNode> {
    let mut deps = index.deps_of(kind, id);
    deps.sort();
    deps.dedup();
    deps.into_iter()
        .map(|(target_kind, target_id, via)| {
            let key = (target_kind.clone(), target_id.clone());
            let state = match index.locate(&target_kind, &target_id) {
                Located::Missing => DepState::Missing,
                Located::Ambiguous { paths } => DepState::Ambiguous { count: paths.len() },
                Located::One { path } => DepState::Resolved { path },
            };
            let mut children = Vec::new();
            let state = if ancestors.contains(&key) {
                DepState::Cycle
            } else if depth <= 1 && matches!(state, DepState::Resolved { .. }) {
                DepState::Truncated
            } else {
                if matches!(state, DepState::Resolved { .. }) {
                    ancestors.insert(key.clone());
                    children = expand(index, &target_kind, &target_id, depth - 1, ancestors);
                    ancestors.remove(&key);
                }
                state
            };
            DepNode { kind: target_kind, id: target_id, via, state, children }
        })
        .collect()
}

/// Project-relative path segments of an open asset, for the breadcrumb.
pub fn breadcrumb(root: &Path, path: &Path) -> Vec<String> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect()
}

/// File extension an asset of `kind` uses, for opening a reference in its
/// own editor tab.
pub fn extension_for_kind(kind: &str) -> Option<&'static str> {
    extension_of(kind)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    pub struct MemIndex {
        pub assets: Vec<(PickerCandidate, PathBuf)>,
        pub deps: HashMap<(String, String), Vec<(String, String, String)>>,
        pub duplicates: HashSet<(String, String)>,
    }

    impl MemIndex {
        pub fn add(&mut self, kind: &'static str, id: &str, name: &str, skeleton: Option<&str>) {
            let path = PathBuf::from(format!("Content/{name}.{}", extension_of(kind).unwrap_or("x")));
            self.assets.push((
                PickerCandidate { id: id.into(), kind, name: name.into(), path: path.to_string_lossy().into(), primary_skeleton: skeleton.map(Into::into) },
                path,
            ));
        }
        pub fn dep(&mut self, from: (&str, &str), to: (&str, &str), via: &str) {
            self.deps.entry((from.0.into(), from.1.into())).or_default().push((to.0.into(), to.1.into(), via.into()));
        }
    }

    impl RefIndex for MemIndex {
        fn candidates(&self, kind: &str) -> Vec<PickerCandidate> {
            self.assets.iter().filter(|(c, _)| c.kind == kind).map(|(c, _)| c.clone()).collect()
        }
        fn used_by(&self, kind: &str, id: &str) -> Vec<Referrer> {
            self.deps
                .iter()
                .flat_map(|((sk, si), targets)| targets.iter().filter(|(tk, ti, _)| tk == kind && ti == id).map(move |(_, _, via)| (sk, si, via)))
                .map(|(sk, si, via)| Referrer {
                    kind: sk.clone(),
                    id: si.clone(),
                    path: self.assets.iter().find(|(c, _)| &c.id == si).map(|(_, p)| p.clone()).unwrap_or_default(),
                    via: via.clone(),
                    hint: false,
                })
                .collect()
        }
        fn deps_of(&self, kind: &str, id: &str) -> Vec<(String, String, String)> {
            self.deps.get(&(kind.to_string(), id.to_string())).cloned().unwrap_or_default()
        }
        fn locate(&self, kind: &str, id: &str) -> Located {
            if self.duplicates.contains(&(kind.to_string(), id.to_string())) {
                return Located::Ambiguous { paths: vec!["a".into(), "b".into()] };
            }
            match self.assets.iter().find(|(c, _)| c.kind == kind && c.id == id) {
                Some((_, p)) => Located::One { path: p.clone() },
                None => Located::Missing,
            }
        }
    }

    fn index() -> MemIndex {
        let mut i = MemIndex::default();
        i.add("skeleton", "sk-a", "Hero_Skeleton", Some("sk-a"));
        i.add("animseq", "walk", "Walk", Some("sk-a"));
        i.add("animseq", "other", "Dance", Some("sk-b"));
        i.add("blendspace", "loco", "Locomotion", Some("sk-a"));
        i.add("anim_graph", "graph", "LocomotionGraph", Some("sk-a"));
        i.dep(("blendspace", "loco"), ("animseq", "walk"), "samples.0.sequence");
        i.dep(("anim_graph", "graph"), ("blendspace", "loco"), "main.nodes.n1.properties.blend_space");
        i.dep(("animseq", "walk"), ("skeleton", "sk-a"), "skeleton");
        i
    }

    #[test]
    fn picker_lists_only_the_asked_kind_and_flags_other_skeletons() {
        let i = index();
        let entries = picker_entries(&i, "animseq", Some("sk-a"));
        let names: Vec<_> = entries.iter().map(entry_label).collect();
        assert_eq!(names, ["Dance (other skeleton)", "Walk"]);
        assert!(picker_entries(&i, "montage_none", Some("sk-a")).is_empty());
    }

    #[test]
    fn reference_status_reports_unset_missing_ambiguous_incompatible_and_ok() {
        let mut i = index();
        assert_eq!(ref_status(&i, "animseq", None, Some("sk-a")), RefStatus::Unset);
        assert_eq!(ref_status(&i, "animseq", Some("walk"), Some("sk-a")), RefStatus::Ok { name: "Walk".into() });
        assert_eq!(ref_status(&i, "animseq", Some("nope"), Some("sk-a")), RefStatus::Missing);
        assert_eq!(
            ref_status(&i, "animseq", Some("other"), Some("sk-a")),
            RefStatus::Incompatible { name: "Dance".into(), asset_skeleton: "sk-b".into() }
        );
        i.duplicates.insert(("animseq".into(), "walk".into()));
        assert_eq!(ref_status(&i, "animseq", Some("walk"), None), RefStatus::Ambiguous { count: 2 });
        assert!(ref_status(&i, "animseq", Some("walk"), None).message().is_some());
    }

    #[test]
    fn dependency_tree_follows_references_and_stops_at_cycles_and_depth() {
        let mut i = index();
        let tree = dependency_tree(&i, "anim_graph", "graph", 6);
        assert_eq!(tree.len(), 1);
        let loco = &tree[0];
        assert_eq!((loco.kind.as_str(), loco.id.as_str()), ("blendspace", "loco"));
        assert_eq!(loco.children[0].id, "walk");
        assert_eq!(loco.children[0].children[0].id, "sk-a");
        assert_eq!(tree[0].flatten().len(), 3);

        // a cycle is shown once
        i.dep(("animseq", "walk"), ("anim_graph", "graph"), "loop");
        let tree = dependency_tree(&i, "anim_graph", "graph", 6);
        let all: Vec<_> = tree[0].flatten().into_iter().map(|n| n.state.clone()).collect();
        assert!(all.contains(&DepState::Cycle));

        let shallow = dependency_tree(&index(), "anim_graph", "graph", 1);
        assert_eq!(shallow[0].state, DepState::Truncated);
    }

    #[test]
    fn used_by_is_the_inverse_of_the_dependency_edges() {
        let i = index();
        let users = i.used_by("animseq", "walk");
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].kind, "blendspace");
        assert!(i.used_by("animseq", "other").is_empty());
    }

    #[test]
    fn breadcrumb_is_project_relative() {
        let crumbs = breadcrumb(Path::new("/p"), Path::new("/p/Content/Anim/walk.animseq"));
        assert_eq!(crumbs, ["Content", "Anim", "walk.animseq"]);
    }
}

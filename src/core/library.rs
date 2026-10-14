//! The project's skeletons, found by id.
//!
//! Animation sequences refer to a skeleton by [`SkeletonId`] only. The library
//! turns an id back into a file: it scans the project for `.skeleton` files,
//! reads each one's id, and answers lookups. Because the id lives inside the
//! file, renaming or moving a skeleton (in the editor, in the file manager, or
//! in git) never breaks a reference; the next scan just finds it at its new
//! path.
//!
//! Two files can end up with the same id (a copied file). The library does not
//! guess which is "the" skeleton: it reports the id as ambiguous and every
//! reference to it as unresolved until one of the copies gets a new id.
//!
//! This is the UI editor plugin's widget `Library`, specialised to skeletons.
//! Like [`super::ids`] it is shaped to be lifted into a shared asset-id crate
//! once the engine has one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::assets::{SkeletonAsset, SKELETON_EXTENSION};
use super::ids::SkeletonId;
use super::skeleton::Skeleton;

/// Directories never scanned.
const SKIPPED_DIRS: [&str; 5] = ["target", "node_modules", ".git", ".pulsar", ".cargo"];

/// Stop scanning after this many files, so a huge project can't stall the editor.
const MAX_FILES: usize = 200_000;

/// The project directory `file` belongs to: the nearest ancestor with a
/// `Pulsar.toml`, else one with a `.pulsar` directory, else the file's own
/// directory.
pub fn project_root(file: &Path) -> PathBuf {
    let start = file.parent().unwrap_or(file);
    start
        .ancestors()
        .find(|d| d.join("Pulsar.toml").is_file())
        .or_else(|| start.ancestors().find(|d| d.join(".pulsar").is_dir()))
        .unwrap_or(start)
        .to_path_buf()
}

/// One skeleton file in the project.
#[derive(Clone, Debug)]
pub struct Entry {
    pub id: SkeletonId,
    /// Empty for a skeleton that exists only in memory (the sample).
    pub path: PathBuf,
    /// The file name without its extension.
    pub name: String,
    pub skeleton: Arc<Skeleton>,
}

/// A `.skeleton` file the library could not index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub path: PathBuf,
    pub kind: ProblemKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProblemKind {
    /// The file has no id yet. Opening it in the editor assigns one.
    NoId,
    Unreadable(String),
}

/// What a reference to a skeleton currently points at.
#[derive(Clone, Debug)]
pub enum SkeletonLink {
    /// No skeleton chosen.
    Unset,
    /// The project has not been scanned yet.
    Resolving(SkeletonId),
    Resolved(Entry),
    /// No file in the project has this id.
    Missing(SkeletonId),
    /// More than one file has this id.
    Ambiguous { id: SkeletonId, paths: Vec<PathBuf> },
}

/// An index of the skeletons in a project at one moment.
#[derive(Clone, Debug, Default)]
pub struct Library {
    root: PathBuf,
    by_id: HashMap<SkeletonId, Vec<Entry>>,
    problems: Vec<Problem>,
}

impl Library {
    /// An empty library, for before the first scan finishes.
    pub fn empty(root: PathBuf) -> Self {
        Self {
            root,
            ..Default::default()
        }
    }

    /// Scan `root` for skeletons. Reads every `.skeleton` file, so call it off
    /// the UI thread for large projects.
    pub fn scan(root: &Path) -> Self {
        let mut library = Self::empty(root.to_path_buf());
        for path in collect(root, SKELETON_EXTENSION) {
            library.add_path(&path);
        }
        library.problems.sort_by(|a, b| a.path.cmp(&b.path));
        library
    }

    /// Index the skeleton file at `path`, replacing any earlier entry for the
    /// same file.
    pub fn add_path(&mut self, path: &Path) {
        for entries in self.by_id.values_mut() {
            entries.retain(|e| !same_file(&e.path, path));
        }
        self.by_id.retain(|_, v| !v.is_empty());
        self.problems.retain(|p| !same_file(&p.path, path));

        let text = match read(path) {
            Ok(text) => text,
            Err(error) => {
                self.problems.push(Problem {
                    path: path.to_path_buf(),
                    kind: ProblemKind::Unreadable(error),
                });
                return;
            }
        };
        // A file with no id can't be referenced yet; it is not an error.
        if SkeletonAsset::peek_id(&text).is_none() {
            let kind = match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(serde_json::Value::Object(_)) => ProblemKind::NoId,
                _ if text.trim().is_empty() => ProblemKind::NoId,
                _ => ProblemKind::Unreadable("not a skeleton file".to_string()),
            };
            self.problems.push(Problem {
                path: path.to_path_buf(),
                kind,
            });
            return;
        }
        match SkeletonAsset::parse(&text) {
            Ok(asset) => {
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let id = asset.id.clone();
                self.by_id.entry(id.clone()).or_default().push(Entry {
                    id,
                    path: path.to_path_buf(),
                    name,
                    skeleton: Arc::new(asset.skeleton()),
                });
            }
            Err(error) => self.problems.push(Problem {
                path: path.to_path_buf(),
                kind: ProblemKind::Unreadable(error),
            }),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The entry for `id`, unless it is missing or ambiguous.
    pub fn entry(&self, id: &SkeletonId) -> Option<&Entry> {
        match self.by_id.get(id).map(Vec::as_slice) {
            Some([only]) => Some(only),
            _ => None,
        }
    }

    /// Every file that claims `id` (more than one means a copied file).
    pub fn claimants(&self, id: &SkeletonId) -> &[Entry] {
        self.by_id.get(id).map_or(&[], Vec::as_slice)
    }

    pub fn path_of(&self, id: &SkeletonId) -> Option<&Path> {
        self.entry(id).map(|e| e.path.as_path())
    }

    /// The id of the skeleton at `path`, if the library has indexed it.
    pub fn id_at(&self, path: &Path) -> Option<&SkeletonId> {
        self.by_id
            .values()
            .flatten()
            .find(|e| same_file(&e.path, path))
            .map(|e| &e.id)
    }

    /// All skeletons, by name then path.
    pub fn entries(&self) -> Vec<&Entry> {
        let mut all: Vec<&Entry> = self.by_id.values().flatten().collect();
        all.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.path.cmp(&b.path))
        });
        all
    }

    /// Whether any file has this id (even if several do).
    pub fn skeleton_exists(&self, id: &SkeletonId) -> bool {
        self.by_id.contains_key(id)
    }

    pub fn is_ambiguous(&self, id: &SkeletonId) -> bool {
        self.claimants(id).len() > 1
    }

    pub fn len(&self) -> usize {
        self.by_id.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Ids claimed by more than one file.
    pub fn conflicts(&self) -> Vec<(&SkeletonId, &[Entry])> {
        let mut out: Vec<_> = self
            .by_id
            .iter()
            .filter(|(_, v)| v.len() > 1)
            .map(|(k, v)| (k, v.as_slice()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(b.0));
        out
    }

    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    /// What a sequence's `skeleton` field currently points at.
    pub fn link(&self, id: Option<&SkeletonId>) -> SkeletonLink {
        let Some(id) = id else {
            return SkeletonLink::Unset;
        };
        match self.claimants(id) {
            [] => SkeletonLink::Missing(id.clone()),
            [only] => SkeletonLink::Resolved(only.clone()),
            many => SkeletonLink::Ambiguous {
                id: id.clone(),
                paths: many.iter().map(|e| e.path.clone()).collect(),
            },
        }
    }
}

pub(super) fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Every file with `extension` under `root`, in path order.
pub(super) fn collect(root: &Path, extension: &str) -> Vec<PathBuf> {
    let Ok(entries) = engine_fs::virtual_fs::manifest(root) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .into_iter()
        .filter(|e| !e.is_dir)
        .filter(|e| {
            let path = Path::new(&e.path);
            let skipped = path.components().any(|c| {
                let name = c.as_os_str().to_string_lossy();
                SKIPPED_DIRS.contains(&name.as_ref()) || name.starts_with('.')
            });
            !skipped
                && path
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case(extension))
        })
        .map(|e| root.join(e.path))
        .take(MAX_FILES)
        .collect();
    out.sort();
    out
}

/// Write an id into the skeleton file at `path` if it has none, changing
/// nothing else. Used to give new or hand-made files their stable id.
pub fn stamp_file(path: &Path) -> Result<SkeletonId, String> {
    let text = read(path)?;
    if let Some(id) = SkeletonAsset::peek_id(&text) {
        return Ok(id);
    }
    // `parse` supplies the fresh id.
    let asset = SkeletonAsset::parse(&text)?;
    write(path, &asset)?;
    Ok(asset.id)
}

/// Give the skeleton file at `path` a new id. This is how one of two copies
/// that share an id is made distinct; sequences that pointed at the shared id
/// stay pointed at the copy that kept it.
pub fn regenerate_id(path: &Path) -> Result<SkeletonId, String> {
    let mut asset = SkeletonAsset::parse(&read(path)?)?;
    asset.id = SkeletonId::new();
    write(path, &asset)?;
    Ok(asset.id)
}

fn read(path: &Path) -> Result<String, String> {
    engine_fs::virtual_fs::read_file(path)
        .map_err(|e| e.to_string())
        .and_then(|b| String::from_utf8(b).map_err(|e| e.to_string()))
}

fn write(path: &Path, asset: &SkeletonAsset) -> Result<(), String> {
    engine_fs::virtual_fs::write_file_atomically(path, asset.to_text().as_bytes())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::assets::save_skeleton_asset;
    use crate::core::sample;

    fn write_asset(dir: &Path, rel: &str, asset: &SkeletonAsset) -> PathBuf {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        save_skeleton_asset(&path, asset).unwrap();
        path
    }

    fn asset() -> SkeletonAsset {
        SkeletonAsset::new(sample::humanoid_skeleton())
    }

    #[test]
    fn skeletons_are_found_by_id_wherever_they_are() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (asset(), asset());
        write_asset(dir.path(), "rigs/Human.skeleton", &a);
        write_asset(dir.path(), "rigs/deep/er/Horse.SKELETON", &b);
        write_asset(dir.path(), "target/Skipped.skeleton", &asset());
        write_asset(dir.path(), ".git/Hidden.skeleton", &asset());
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();

        let lib = Library::scan(dir.path());
        assert_eq!(lib.len(), 2);
        assert_eq!(lib.entry(&a.id).unwrap().name, "Human");
        assert_eq!(lib.entry(&b.id).unwrap().name, "Horse");
        assert!(lib.problems().is_empty());
        assert_eq!(lib.entry(&a.id).unwrap().skeleton.bones.len(), a.bones.len());
    }

    #[test]
    fn renaming_or_moving_a_file_keeps_references_valid() {
        let dir = tempfile::tempdir().unwrap();
        let skeleton = asset();
        let path = write_asset(dir.path(), "rigs/Human.skeleton", &skeleton);

        let before = Library::scan(dir.path());
        assert!(matches!(before.link(Some(&skeleton.id)), SkeletonLink::Resolved(_)));

        // Rename and move; nothing inside any file changes.
        std::fs::create_dir_all(dir.path().join("shared")).unwrap();
        let moved = dir.path().join("shared/Biped.skeleton");
        std::fs::rename(&path, &moved).unwrap();

        let after = Library::scan(dir.path());
        assert_eq!(after.path_of(&skeleton.id), Some(moved.as_path()));
        assert_eq!(after.id_at(&moved), Some(&skeleton.id));
        match after.link(Some(&skeleton.id)) {
            SkeletonLink::Resolved(entry) => assert_eq!(entry.name, "Biped"),
            other => panic!("expected the moved skeleton to resolve, got {other:?}"),
        }
    }

    #[test]
    fn a_copied_file_makes_its_id_ambiguous_until_one_gets_a_new_id() {
        let dir = tempfile::tempdir().unwrap();
        let skeleton = asset();
        let original = write_asset(dir.path(), "A.skeleton", &skeleton);
        let copy = dir.path().join("A copy.skeleton");
        std::fs::copy(&original, &copy).unwrap();

        let lib = Library::scan(dir.path());
        assert_eq!(lib.conflicts().len(), 1);
        assert!(lib.entry(&skeleton.id).is_none(), "neither copy is picked");
        assert!(lib.is_ambiguous(&skeleton.id));
        assert_eq!(lib.claimants(&skeleton.id).len(), 2);
        match lib.link(Some(&skeleton.id)) {
            SkeletonLink::Ambiguous { paths, .. } => assert_eq!(paths.len(), 2),
            other => panic!("expected ambiguous, got {other:?}"),
        }

        // Giving the copy a new id resolves it, to the file that kept the id.
        let new_id = regenerate_id(&copy).unwrap();
        assert_ne!(new_id, skeleton.id);
        let lib = Library::scan(dir.path());
        assert!(lib.conflicts().is_empty());
        assert_eq!(lib.path_of(&skeleton.id), Some(original.as_path()));
        assert_eq!(lib.path_of(&new_id), Some(copy.as_path()));
    }

    #[test]
    fn an_unknown_or_unset_reference_is_missing_or_unset() {
        let dir = tempfile::tempdir().unwrap();
        write_asset(dir.path(), "A.skeleton", &asset());
        let lib = Library::scan(dir.path());
        let stranger = SkeletonId::new();
        assert!(matches!(lib.link(Some(&stranger)), SkeletonLink::Missing(id) if id == stranger));
        assert!(matches!(lib.link(None), SkeletonLink::Unset));
        assert!(!lib.skeleton_exists(&stranger));
    }

    #[test]
    fn files_without_an_id_are_listed_and_can_be_stamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Old.skeleton");
        std::fs::write(&path, r#"{ "version": 1, "bones": [] }"#).unwrap();
        std::fs::write(dir.path().join("Empty.skeleton"), "").unwrap();
        std::fs::write(dir.path().join("Junk.skeleton"), "{ nope").unwrap();

        let lib = Library::scan(dir.path());
        assert!(lib.is_empty());
        let kinds: Vec<_> = lib
            .problems()
            .iter()
            .map(|p| (p.path.file_name().unwrap().to_string_lossy().into_owned(), p.kind.clone()))
            .collect();
        assert!(kinds.contains(&("Old.skeleton".into(), ProblemKind::NoId)));
        assert!(kinds.contains(&("Empty.skeleton".into(), ProblemKind::NoId)));
        assert!(kinds
            .iter()
            .any(|(n, k)| n == "Junk.skeleton" && matches!(k, ProblemKind::Unreadable(_))));

        let id = stamp_file(&path).unwrap();
        assert_eq!(stamp_file(&path).unwrap(), id, "stamping twice keeps the first id");
        assert_eq!(Library::scan(dir.path()).entry(&id).unwrap().name, "Old");
    }

    #[test]
    fn adding_a_path_updates_the_index_without_a_rescan() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = Library::scan(dir.path());
        let skeleton = asset();
        let path = write_asset(dir.path(), "New.skeleton", &skeleton);
        assert!(lib.entry(&skeleton.id).is_none());
        lib.add_path(&path);
        assert_eq!(lib.id_at(&path), Some(&skeleton.id));
        // Adding the same file again does not make it ambiguous.
        lib.add_path(&path);
        assert!(lib.entry(&skeleton.id).is_some());
    }

    #[test]
    fn the_project_root_is_found_from_a_file_inside_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
        let file = dir.path().join("anim/rigs/Human.skeleton");
        assert_eq!(project_root(&file), dir.path());
        let loose = tempfile::tempdir().unwrap();
        assert_eq!(project_root(&loose.path().join("A.skeleton")), loose.path());
    }
}

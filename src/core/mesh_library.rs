//! The project's skeletal meshes, found by id.
//!
//! The same idea as [`super::library`] for skeletons: a skeleton or animation
//! sequence that previews a mesh stores only its [`SkeletalMeshId`], and this
//! index turns the id back into a file by scanning the project's `.skmesh`
//! files. Renaming or moving a mesh never breaks the link; two files with the
//! same id (a copied file) make the id ambiguous until one gets a new id.
//!
//! An entry keeps only the header facts (id, path, name, the skeleton it is
//! skinned to), not the geometry: a project can hold many large meshes, and the
//! mesh actually shown is loaded from its path on demand. Reading a file to get
//! its id is the cost the shared file index (Pulsar-Native#1322) removes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use skeletal_assets::skmesh::{SkeletalMeshAsset, SKMESH_EXTENSION};

use super::ids::{SkeletalMeshId, SkeletonId};
use super::library::{collect, project_root, same_file};

/// One skeletal mesh file in the project.
#[derive(Clone, Debug)]
pub struct MeshEntry {
    pub id: SkeletalMeshId,
    pub path: PathBuf,
    /// The file name without its extension.
    pub name: String,
    /// The skeleton the mesh is skinned to, as written in the file.
    pub skeleton: Option<SkeletonId>,
}

/// A `.skmesh` file the index could not read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeshProblem {
    pub path: PathBuf,
    pub error: String,
}

/// What a reference to a skeletal mesh currently points at.
#[derive(Clone, Debug, Default)]
pub enum MeshLink {
    /// No mesh chosen.
    #[default]
    Unset,
    /// The project has not been scanned yet.
    Resolving(SkeletalMeshId),
    Resolved(MeshEntry),
    /// No file in the project has this id.
    Missing(SkeletalMeshId),
    /// More than one file has this id.
    Ambiguous {
        id: SkeletalMeshId,
        paths: Vec<PathBuf>,
    },
}

/// An index of the skeletal meshes in a project at one moment.
#[derive(Clone, Debug, Default)]
pub struct MeshLibrary {
    root: PathBuf,
    by_id: HashMap<SkeletalMeshId, Vec<MeshEntry>>,
    problems: Vec<MeshProblem>,
}

impl MeshLibrary {
    /// An empty index, for before the first scan finishes.
    pub fn empty(root: PathBuf) -> Self {
        Self {
            root,
            ..Default::default()
        }
    }

    /// Scan `root` for skeletal meshes. Reads every `.skmesh` file, so call it
    /// off the UI thread.
    pub fn scan(root: &Path) -> Self {
        let mut library = Self::empty(root.to_path_buf());
        for path in collect(root, SKMESH_EXTENSION) {
            library.add_path(&path);
        }
        library.problems.sort_by(|a, b| a.path.cmp(&b.path));
        library
    }

    /// Index the mesh file at `path`, replacing any earlier entry for the same
    /// file.
    pub fn add_path(&mut self, path: &Path) {
        for entries in self.by_id.values_mut() {
            entries.retain(|e| !same_file(&e.path, path));
        }
        self.by_id.retain(|_, v| !v.is_empty());
        self.problems.retain(|p| !same_file(&p.path, path));

        let (id, skeleton) = match SkeletalMeshAsset::peek_header_at(path) {
            Ok(header) => header,
            Err(error) => {
                clear_asset_id(&self.root, path);
                self.problems.push(MeshProblem {
                    path: path.to_path_buf(),
                    error: error.to_string(),
                });
                return;
            }
        };
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        remember_asset_id(&self.root, &id, path);
        self.by_id.entry(id.clone()).or_default().push(MeshEntry {
            id,
            path: path.to_path_buf(),
            name,
            skeleton,
        });
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The entry for `id`, unless it is missing or ambiguous.
    pub fn entry(&self, id: &SkeletalMeshId) -> Option<&MeshEntry> {
        match self.by_id.get(id).map(Vec::as_slice) {
            Some([only]) => Some(only),
            _ => None,
        }
    }

    /// Every file that claims `id` (more than one means a copied file).
    pub fn claimants(&self, id: &SkeletalMeshId) -> &[MeshEntry] {
        self.by_id.get(id).map_or(&[], Vec::as_slice)
    }

    pub fn path_of(&self, id: &SkeletalMeshId) -> Option<&Path> {
        self.entry(id).map(|e| e.path.as_path())
    }

    /// All meshes, by name then path.
    pub fn entries(&self) -> Vec<&MeshEntry> {
        let mut all: Vec<&MeshEntry> = self.by_id.values().flatten().collect();
        all.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.path.cmp(&b.path))
        });
        all
    }

    pub fn is_ambiguous(&self, id: &SkeletalMeshId) -> bool {
        self.claimants(id).len() > 1
    }

    pub fn len(&self) -> usize {
        self.by_id.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Ids claimed by more than one file.
    pub fn conflicts(&self) -> Vec<(&SkeletalMeshId, &[MeshEntry])> {
        let mut out: Vec<_> = self
            .by_id
            .iter()
            .filter(|(_, v)| v.len() > 1)
            .map(|(k, v)| (k, v.as_slice()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(b.0));
        out
    }

    pub fn problems(&self) -> &[MeshProblem] {
        &self.problems
    }

    /// What a `preview_mesh` field currently points at.
    pub fn link(&self, id: Option<&SkeletalMeshId>) -> MeshLink {
        let Some(id) = id else {
            return MeshLink::Unset;
        };
        match self.claimants(id) {
            [] => MeshLink::Missing(id.clone()),
            [only] => MeshLink::Resolved(only.clone()),
            many => MeshLink::Ambiguous {
                id: id.clone(),
                paths: many.iter().map(|e| e.path.clone()).collect(),
            },
        }
    }
}

/// The id of the mesh file at `path`.
pub fn read_id(path: &Path) -> Result<SkeletalMeshId, String> {
    let id = match SkeletalMeshAsset::peek_id_at(path) {
        Ok(id) => id,
        Err(error) => {
            clear_asset_id(&project_root(path), path);
            return Err(error.to_string());
        }
    };
    remember_asset_id(&project_root(path), &id, path);
    Ok(id)
}

/// Give the mesh file at `path` a new id. This is how one of two copies that
/// share an id is made distinct; links to the shared id stay with the copy that
/// kept it.
pub fn regenerate_id(path: &Path) -> Result<SkeletalMeshId, String> {
    let bytes = match engine_fs::virtual_fs::read_file(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            clear_asset_id(&project_root(path), path);
            return Err(error.to_string());
        }
    };
    let mut mesh = match SkeletalMeshAsset::decode(&bytes) {
        Ok(mesh) => mesh,
        Err(error) => {
            clear_asset_id(&project_root(path), path);
            return Err(error.to_string());
        }
    };
    mesh.id = SkeletalMeshId::new();
    engine_fs::virtual_fs::write_file_atomically(path, &mesh.encode())
        .map_err(|e| e.to_string())?;
    remember_asset_id(&project_root(path), &mesh.id, path);
    Ok(mesh.id)
}

fn remember_asset_id(root: &Path, id: &SkeletalMeshId, path: &Path) {
    if let Ok(Some(index)) = engine_fs::open_project_file_index(root) {
        let result = (|| {
            index.refresh_path(path)?;
            let paths = index.paths_for_asset_id(id.as_str())?;
            if !paths.iter().any(|indexed| same_file(indexed, path)) {
                index.record_typed_asset_id("skmesh", id.as_str(), path)?;
            }
            Ok::<(), anyhow::Error>(())
        })();
        if let Err(error) = result {
            log::debug!("Could not cache skeletal mesh id for {path:?}: {error:#}");
        }
    }
}

fn clear_asset_id(root: &Path, path: &Path) {
    if let Ok(Some(index)) = engine_fs::open_project_file_index(root) {
        if let Err(error) = index.clear_asset_ids(path) {
            log::debug!("Could not clear stale skeletal mesh id for {path:?}: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skeletal_assets::skmesh::save_skmesh_asset;

    fn write_mesh(dir: &Path, rel: &str, mesh: &SkeletalMeshAsset) -> PathBuf {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        save_skmesh_asset(&path, mesh).unwrap();
        path
    }

    fn mesh() -> SkeletalMeshAsset {
        SkeletalMeshAsset::new(Some(SkeletonId::new()))
    }

    #[test]
    fn meshes_are_found_by_id_wherever_they_are() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (mesh(), mesh());
        write_mesh(dir.path(), "chars/Hero.skmesh", &a);
        write_mesh(dir.path(), "chars/deep/Horse.SKMESH", &b);
        write_mesh(dir.path(), "target/Skipped.skmesh", &mesh());
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();

        let lib = MeshLibrary::scan(dir.path());
        assert_eq!(lib.len(), 2);
        assert_eq!(lib.entry(&a.id).unwrap().name, "Hero");
        assert_eq!(lib.entry(&a.id).unwrap().skeleton, a.skeleton);
        assert_eq!(lib.entry(&b.id).unwrap().name, "Horse");
        assert!(lib.problems().is_empty());
        let file_index = engine_fs::open_project_file_index(dir.path())
            .unwrap()
            .unwrap();
        assert_eq!(
            file_index.paths_for_asset_id(a.id.as_str()).unwrap(),
            vec![dir.path().join("chars/Hero.skmesh")]
        );
    }

    #[test]
    fn renaming_or_moving_a_file_keeps_links_valid() {
        let dir = tempfile::tempdir().unwrap();
        let m = mesh();
        let path = write_mesh(dir.path(), "chars/Hero.skmesh", &m);
        assert!(matches!(
            MeshLibrary::scan(dir.path()).link(Some(&m.id)),
            MeshLink::Resolved(_)
        ));

        std::fs::create_dir_all(dir.path().join("shared")).unwrap();
        let moved = dir.path().join("shared/Knight.skmesh");
        engine_fs::virtual_fs::rename(&path, &moved).unwrap();
        let after = MeshLibrary::scan(dir.path());
        assert_eq!(after.path_of(&m.id), Some(moved.as_path()));
        match after.link(Some(&m.id)) {
            MeshLink::Resolved(entry) => assert_eq!(entry.name, "Knight"),
            other => panic!("expected the moved mesh to resolve, got {other:?}"),
        }
    }

    #[test]
    fn a_copied_file_makes_its_id_ambiguous_until_one_gets_a_new_id() {
        let dir = tempfile::tempdir().unwrap();
        let m = mesh();
        let original = write_mesh(dir.path(), "A.skmesh", &m);
        let copy = dir.path().join("A copy.skmesh");
        std::fs::copy(&original, &copy).unwrap();

        let lib = MeshLibrary::scan(dir.path());
        assert_eq!(lib.conflicts().len(), 1);
        assert!(lib.entry(&m.id).is_none(), "neither copy is picked");
        match lib.link(Some(&m.id)) {
            MeshLink::Ambiguous { paths, .. } => assert_eq!(paths.len(), 2),
            other => panic!("expected ambiguous, got {other:?}"),
        }

        let new_id = regenerate_id(&copy).unwrap();
        assert_ne!(new_id, m.id);
        let lib = MeshLibrary::scan(dir.path());
        assert!(lib.conflicts().is_empty());
        assert_eq!(lib.path_of(&m.id), Some(original.as_path()));
        assert_eq!(lib.path_of(&new_id), Some(copy.as_path()));
        assert_eq!(read_id(&copy).unwrap(), new_id);
        let file_index = engine_fs::open_project_file_index(dir.path())
            .unwrap()
            .unwrap();
        assert_eq!(
            file_index.paths_for_asset_id(m.id.as_str()).unwrap(),
            vec![original]
        );
        assert_eq!(
            file_index.paths_for_asset_id(new_id.as_str()).unwrap(),
            vec![copy]
        );
    }

    #[test]
    fn unknown_unset_and_unreadable_references_are_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        write_mesh(dir.path(), "A.skmesh", &mesh());
        std::fs::write(dir.path().join("Junk.skmesh"), "not a mesh").unwrap();
        let lib = MeshLibrary::scan(dir.path());
        let stranger = SkeletalMeshId::new();
        assert!(matches!(lib.link(Some(&stranger)), MeshLink::Missing(id) if id == stranger));
        assert!(matches!(lib.link(None), MeshLink::Unset));
        assert_eq!(lib.problems().len(), 1);
        assert!(lib.problems()[0].path.ends_with("Junk.skmesh"));
    }

    #[test]
    fn adding_a_path_updates_the_index_without_a_rescan() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = MeshLibrary::scan(dir.path());
        let m = mesh();
        let path = write_mesh(dir.path(), "New.skmesh", &m);
        assert!(lib.entry(&m.id).is_none());
        lib.add_path(&path);
        lib.add_path(&path);
        assert!(
            lib.entry(&m.id).is_some(),
            "adding twice does not make it ambiguous"
        );
    }

    #[test]
    fn rescanning_a_broken_mesh_clears_its_cached_id() {
        let dir = tempfile::tempdir().unwrap();
        let mesh = mesh();
        let path = write_mesh(dir.path(), "Broken.skmesh", &mesh);
        let _ = MeshLibrary::scan(dir.path());
        let file_index = engine_fs::open_project_file_index(dir.path())
            .unwrap()
            .unwrap();
        assert_eq!(
            file_index.paths_for_asset_id(mesh.id.as_str()).unwrap(),
            vec![path.clone()]
        );

        std::fs::write(&path, "not a mesh").unwrap();
        let broken = MeshLibrary::scan(dir.path());
        assert!(!broken.problems().is_empty());
        assert!(file_index
            .paths_for_asset_id(mesh.id.as_str())
            .unwrap()
            .is_empty());
    }
}

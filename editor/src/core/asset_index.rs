//! The animation assets' connection to the project file index.
//!
//! The index owns id-to-path resolution and the derived `asset_deps` table.
//! This module is the editor side of that contract: it registers an asset's id
//! and re-extracts its dependency edges whenever the asset is opened, saved or
//! imported, resolves ids for the validators, and lists picker candidates and
//! "Used By" referrers. Nothing here is written into an asset file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine_fs::{AssetDep, ProjectFileIndex};
use skeletal_assets::dependencies::{extension_of, extract, kind_of_extension, AssetSource, EdgeRole, Lookup, SkeletonRule};
use skeletal_assets::picker::{describe, PickerCandidate};

/// Resolves `(kind, id)` through the project index and reads the file.
pub struct ProjectAssetSource {
    index: Arc<ProjectFileIndex>,
}

impl ProjectAssetSource {
    pub fn open(root: &Path) -> Option<Self> {
        engine_fs::open_project_file_index(root).ok().flatten().map(|index| Self { index })
    }

    pub fn index(&self) -> &ProjectFileIndex {
        &self.index
    }
}

impl AssetSource for ProjectAssetSource {
    fn read(&self, kind: &str, id: &str) -> Lookup {
        let Ok(paths) = self.index.paths_for_asset_id_kind(kind, id) else {
            return Lookup::Missing;
        };
        match paths.as_slice() {
            [] => Lookup::Missing,
            [one] => {
                let wanted = extension_of(kind);
                let actual = one.extension().and_then(|e| e.to_str());
                if wanted.is_some() && actual.is_some_and(|a| !wanted.is_some_and(|w| w.eq_ignore_ascii_case(a))) {
                    return Lookup::WrongKind(one.display().to_string());
                }
                match engine_fs::virtual_fs::read_file(one) {
                    Ok(bytes) => Lookup::Found(bytes),
                    Err(_) => Lookup::Missing,
                }
            }
            many => Lookup::Ambiguous(many.iter().map(|p| p.display().to_string()).collect()),
        }
    }
}

fn dep_row(kind: &str, id: &str, edge: &skeletal_assets::dependencies::AssetEdge) -> AssetDep {
    AssetDep {
        source_kind: kind.to_string(),
        source_id: id.to_string(),
        target_kind: edge.target_kind.to_string(),
        target_id: edge.target_id.clone(),
        role: edge.role.as_str().to_string(),
        via: edge.via.clone(),
        expect: edge.skeleton.as_str().to_string(),
    }
}

/// Register the asset at `path`: its typed id and its dependency edges.
/// Unreadable assets lose their edges (an unreadable file depends on nothing
/// known) and keep any id they had. Failures are logged, never fatal: the
/// index is derived data and is rebuilt on the next save.
pub fn record_asset(root: &Path, path: &Path) {
    let Some(kind) = path.extension().and_then(|e| e.to_str()).and_then(kind_of_extension) else {
        return;
    };
    let Ok(Some(index)) = engine_fs::open_project_file_index(root) else {
        return;
    };
    let result = (|| -> anyhow::Result<()> {
        index.refresh_path(path)?;
        let bytes = engine_fs::virtual_fs::read_file(path)?;
        match extract(kind, &bytes) {
            Ok(extracted) => {
                index.record_typed_asset_id(kind, &extracted.id, path)?;
                let rows: Vec<AssetDep> = extracted.edges.iter().map(|e| dep_row(kind, &extracted.id, e)).collect();
                index.replace_asset_deps(path, kind, &extracted.id, &rows)?;
            }
            Err(message) => {
                log::debug!("not indexing dependencies of {}: {message}", path.display());
                index.clear_asset_deps(path)?;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        log::debug!("could not index {}: {error:#}", path.display());
    }
}

/// An asset that references another, for the Used By list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Referrer {
    pub kind: String,
    pub id: String,
    pub path: PathBuf,
    /// The authored field holding the reference.
    pub via: String,
    pub hint: bool,
}

/// Every recorded asset that references `(kind, id)`.
pub fn used_by(root: &Path, kind: &str, id: &str) -> Vec<Referrer> {
    let Some(source) = ProjectAssetSource::open(root) else { return Vec::new() };
    source
        .index()
        .asset_used_by(kind, id)
        .unwrap_or_default()
        .into_iter()
        .map(|d| Referrer {
            hint: EdgeRole::parse(&d.dep.role) == Some(EdgeRole::Hint),
            kind: d.dep.source_kind,
            id: d.dep.source_id,
            path: d.path,
            via: d.dep.via,
        })
        .collect()
}

/// What `(kind, id)` references, as recorded.
pub fn dependencies_of(root: &Path, kind: &str, id: &str) -> Vec<(String, String, String, SkeletonRule)> {
    let Some(source) = ProjectAssetSource::open(root) else { return Vec::new() };
    source
        .index()
        .asset_deps_of(kind, id)
        .unwrap_or_default()
        .into_iter()
        .map(|d| (d.target_kind, d.target_id, d.via, SkeletonRule::parse(&d.expect).unwrap_or(SkeletonRule::Unconstrained)))
        .collect()
}

/// Describe every indexed asset of `kind` for a picker. Unreadable files are
/// skipped; a picker shows what it can use.
pub fn picker_candidates(root: &Path, kind: &str) -> Vec<PickerCandidate> {
    let Some(source) = ProjectAssetSource::open(root) else { return Vec::new() };
    source
        .index()
        .assets_of_kind(kind)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, path)| {
            let bytes = engine_fs::virtual_fs::read_file(&path).ok()?;
            let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            describe(&relative, &bytes)
        })
        .collect()
}

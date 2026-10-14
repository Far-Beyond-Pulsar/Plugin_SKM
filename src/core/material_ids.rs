//! Stable identities for material files selected by skeletal mesh slots.
//!
//! Scalar `.mat` documents store `id` in their JSON. Shader graph folders store
//! `pulsar_asset_id` alongside the graph document. The typed project index
//! mirrors those ids so a slot can find a material after its path changes.

use std::path::{Path, PathBuf};

use pulsar_reflection::MaterialRef;

const KIND: &str = "material";
const GRAPH_FILE: &str = "shader_graph_save.json";

/// Return the material's stable id, assigning and persisting one when a legacy
/// material has no identity yet. `asset_path` is the `.mat` file or graph folder
/// path used by the picker.
pub fn id_for_path(root: &Path, asset_path: &Path) -> Result<MaterialRef, String> {
    let document_path = document_path(asset_path);
    let mut document: serde_json::Value = read_json(&document_path)?;

    let embedded = document
        .get("id")
        .or_else(|| document.get("pulsar_asset_id"))
        .and_then(serde_json::Value::as_str)
        .and_then(MaterialRef::parse);
    let index = engine_fs::open_project_file_index(root)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "project file index is unavailable".to_string())?;
    let ids = index
        .asset_ids_for_path_kind(asset_path, KIND)
        .map_err(|error| error.to_string())?;
    let id = ids
        .iter()
        .find_map(|raw| MaterialRef::parse(raw))
        .or(embedded)
        .unwrap_or_default();

    let field = if document_path
        .file_name()
        .is_some_and(|name| name == GRAPH_FILE)
    {
        "pulsar_asset_id"
    } else {
        "id"
    };
    if document.get(field).and_then(serde_json::Value::as_str) != Some(id.as_str()) {
        document[field] = serde_json::Value::String(id.as_str().to_owned());
        let text = serde_json::to_string_pretty(&document).map_err(|error| error.to_string())?;
        engine_fs::virtual_fs::write_file_atomically(&document_path, text.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    index
        .refresh_path(asset_path)
        .and_then(|_| index.record_typed_asset_id(KIND, id.as_str(), asset_path))
        .map_err(|error| error.to_string())?;
    Ok(id)
}

/// Find the current project path for a material id after a move or rename.
pub fn path_for_id(root: &Path, id: &MaterialRef) -> Option<PathBuf> {
    let index = engine_fs::open_project_file_index(root).ok()??;
    index
        .paths_for_asset_id(id.as_str())
        .ok()?
        .into_iter()
        .find(|path| {
            index
                .asset_ids_for_path_kind(path, KIND)
                .is_ok_and(|ids| ids.iter().any(|candidate| candidate == id.as_str()))
        })
}

fn document_path(asset_path: &Path) -> PathBuf {
    if asset_path.is_dir() {
        asset_path.join(GRAPH_FILE)
    } else {
        asset_path.to_path_buf()
    }
}

fn read_json(path: &Path) -> Result<serde_json::Value, String> {
    let bytes = engine_fs::virtual_fs::read_file(path).map_err(|error| error.to_string())?;
    let text = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
    let json_start = text
        .find('{')
        .ok_or_else(|| "material JSON object is missing".to_string())?;
    serde_json::from_str(&text[json_start..])
        .map_err(|error| format!("invalid material JSON: {error}"))
}

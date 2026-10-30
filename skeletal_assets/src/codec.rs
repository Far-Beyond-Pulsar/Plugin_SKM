//! Shared helpers for the versioned JSON animation assets.
//!
//! Every codec reads through [`read_envelope`], which classifies a file as
//! missing a version, older than the codec reads, current, or newer than this
//! build understands, so each format refuses the same inputs in the same way.

use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// Fields a codec does not know. Held verbatim and written back so a newer
/// compatible writer's data survives a round trip through this one.
pub type Extra = Map<String, Value>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodecError {
    /// Not JSON, or JSON of the wrong shape.
    Malformed(String),
    /// The file has no `version` and the format does not allow that.
    MissingVersion,
    /// The file names a version this build does not support yet.
    FutureVersion { found: u64, supported: u32 },
    /// The file is older than the oldest version still read.
    UnsupportedVersion { found: u64, oldest: u32 },
    /// The file has no stable `id`; ids are never invented on read.
    MissingId,
    /// The content parsed but breaks a rule of the format.
    Invalid(String),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "malformed asset: {e}"),
            Self::MissingVersion => f.write_str("asset has no format version"),
            Self::FutureVersion { found, supported } => write!(
                f,
                "asset format version {found} is newer than this build supports ({supported})"
            ),
            Self::UnsupportedVersion { found, oldest } => write!(
                f,
                "asset format version {found} is older than the oldest readable version ({oldest})"
            ),
            Self::MissingId => f.write_str("asset has no stable id"),
            Self::Invalid(e) => write!(f, "invalid asset: {e}"),
        }
    }
}

impl std::error::Error for CodecError {}

impl From<serde_json::Error> for CodecError {
    fn from(e: serde_json::Error) -> Self {
        Self::Malformed(e.to_string())
    }
}

/// What a codec accepts.
#[derive(Clone, Copy, Debug)]
pub struct VersionPolicy {
    pub oldest: u32,
    pub current: u32,
    /// The version assumed for a file with no `version` field, if any.
    pub unversioned_as: Option<u32>,
}

/// Parse `text` as JSON and classify its version.
pub fn read_envelope(text: &str, policy: VersionPolicy) -> Result<(Value, u32), CodecError> {
    let value: Value = serde_json::from_str(text)?;
    if !value.is_object() {
        return Err(CodecError::Malformed("expected a JSON object".into()));
    }
    let version = match value.get("version") {
        None => policy.unversioned_as.ok_or(CodecError::MissingVersion)? as u64,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| CodecError::Malformed("`version` must be an integer".into()))?,
    };
    if version > policy.current as u64 {
        return Err(CodecError::FutureVersion {
            found: version,
            supported: policy.current,
        });
    }
    if version < policy.oldest as u64 {
        return Err(CodecError::UnsupportedVersion {
            found: version,
            oldest: policy.oldest,
        });
    }
    Ok((value, version as u32))
}

pub fn from_value<T: DeserializeOwned>(value: Value) -> Result<T, CodecError> {
    serde_json::from_value(value).map_err(Into::into)
}

/// The `id` string of a parsed asset, if it has a well-formed UUID one.
pub fn peek_uuid(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    uuid::Uuid::parse_str(value.get("id")?.as_str()?)
        .ok()
        .map(|u| u.to_string())
}

/// `Err` naming the first repeated id.
pub fn ensure_unique<'a>(what: &str, ids: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(format!("duplicate {what} id `{id}`"));
        }
    }
    Ok(())
}

pub fn ensure_finite(what: &str, v: f32) -> Result<(), String> {
    if v.is_finite() {
        Ok(())
    } else {
        Err(format!("{what} must be finite"))
    }
}

pub fn read_text(path: &Path) -> anyhow::Result<String> {
    Ok(String::from_utf8(engine_fs::virtual_fs::read_file(path)?)?)
}

pub fn write_text(path: &Path, text: &str) -> anyhow::Result<()> {
    engine_fs::virtual_fs::write_file_atomically(path, text.as_bytes())?;
    Ok(())
}

/// Defines `load_*`/`save_*` for an asset with `parse`/`to_text`.
#[macro_export]
macro_rules! asset_file_io {
    ($asset:ty, $load:ident, $save:ident) => {
        pub fn $load(path: &std::path::Path) -> anyhow::Result<$asset> {
            let text = $crate::codec::read_text(path)?;
            <$asset>::parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
        }
        pub fn $save(path: &std::path::Path, asset: &$asset) -> anyhow::Result<()> {
            $crate::codec::write_text(path, &asset.to_text()?)
        }
    };
}

/// A UUID derived from `namespace` and `key`: the same inputs always give the
/// same id, so migrating the same file twice cannot mint two identities.
pub fn derived_uuid(namespace: &str, key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    hasher.update([0u8]);
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}

/// Lower-case hex SHA-256 of `bytes`: the content hash derived data is keyed by.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Pretty JSON with every object's keys sorted, so documents that hold hash
/// maps (Graphy graphs) write the same bytes every time and diff cleanly.
pub fn to_stable_pretty<T: serde::Serialize>(value: &T) -> Result<String, serde_json::Error> {
    fn sort(value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut entries: Vec<(String, Value)> = map.into_iter().map(|(k, v)| (k, sort(v))).collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                Value::Object(entries.into_iter().collect())
            }
            Value::Array(items) => Value::Array(items.into_iter().map(sort).collect()),
            other => other,
        }
    }
    serde_json::to_string_pretty(&sort(serde_json::to_value(value)?))
}

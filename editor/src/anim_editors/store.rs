//! Where documents live. The editor reads and writes through [`DocStore`] so
//! the document session logic is testable without a project on disk.

use std::path::Path;

pub trait DocStore: Send + Sync {
    /// The file's bytes, or `None` when it does not exist.
    fn read(&self, path: &Path) -> anyhow::Result<Option<Vec<u8>>>;
    /// Replace the file in one step, so a reader never sees half a document.
    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> anyhow::Result<()>;
}

/// The project file system (local, remote or peer provider).
pub struct FsStore;

impl DocStore for FsStore {
    fn read(&self, path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
        match engine_fs::virtual_fs::read_file(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) => {
                if engine_fs::virtual_fs::metadata(path).is_ok() {
                    Err(error)
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            engine_fs::virtual_fs::create_dir_all(parent)?;
        }
        engine_fs::virtual_fs::write_file_atomically(path, bytes)?;
        Ok(())
    }
}

#[cfg(test)]
pub use memory::MemStore;

#[cfg(test)]
mod memory {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// Clones share the same files, like two editor tabs over one project.
    #[derive(Clone, Default)]
    pub struct MemStore(Arc<Mutex<HashMap<PathBuf, Vec<u8>>>>);

    impl MemStore {
        pub fn put(&self, path: &str, text: &str) {
            self.0.lock().unwrap().insert(PathBuf::from(path), text.as_bytes().to_vec());
        }
        pub fn text(&self, path: &str) -> Option<String> {
            self.0.lock().unwrap().get(Path::new(path)).map(|b| String::from_utf8_lossy(b).into_owned())
        }
    }

    impl DocStore for MemStore {
        fn read(&self, path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
            Ok(self.0.lock().unwrap().get(path).cloned())
        }
        fn write_atomic(&self, path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
            self.0.lock().unwrap().insert(path.to_path_buf(), bytes.to_vec());
            Ok(())
        }
    }
}

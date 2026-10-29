//! One open animation asset: the typed value, its undo history, and the
//! revision/conflict check that stops a save from overwriting an edit made
//! elsewhere.
//!
//! The session remembers a hash of the bytes it last read or wrote (its *base*).
//! Before writing it reads the file again; if the bytes differ from the base,
//! another tab, a tool or a source-control operation changed the file and the
//! save is refused with [`SaveOutcome::Conflict`] until the user reloads or
//! explicitly overwrites. Saves go through the store's atomic write.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pulsar_reflection::AssetKind;
use skeletal_assets::anim_graph::AnimationGraphAsset;
use skeletal_assets::blend_space::BlendSpaceAsset;
use skeletal_assets::ik_retarget::IkRetargetAsset;
use skeletal_assets::ik_rig::IkRigAsset;
use skeletal_assets::montage::AnimationMontageAsset;
use skeletal_assets::pose_db::PoseDbAsset;
use skeletal_assets::retarget::RetargetProfileAsset;

use super::history::History;
use super::store::DocStore;

/// What every animation asset kind offers the editor.
pub trait AnimAsset: Clone {
    /// `AssetKind::KIND` of the asset, as recorded in the file index.
    const KIND: &'static str;
    /// Parse and migrate. The flag is true when an id had to be assigned and
    /// the file should be written once so it stays stable.
    fn parse_text(text: &str) -> Result<(Self, bool), String>;
    fn to_text(&self) -> anyhow::Result<String>;
    fn asset_id(&self) -> String;
    fn asset_name(&self) -> String;
    fn set_asset_name(&mut self, name: String);
}

/// FNV-1a: stable across runs, which a revision stamp should be.
pub fn content_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved,
    /// The file on disk is not the one this document was based on.
    Conflict { disk_hash: u64 },
}

pub struct AnimDocument<A: AnimAsset> {
    path: PathBuf,
    value: A,
    history: History<A>,
    store: Arc<dyn DocStore>,
    /// Hash of the bytes last read or written; `None` for a file that does not
    /// exist yet.
    base_hash: Option<u64>,
    /// Bumped on every change to `value`, for views that cache derived data.
    generation: u64,
}

impl<A: AnimAsset> AnimDocument<A> {
    /// Open `path`. A missing or empty file starts from `default_text` and is
    /// written once, so the new asset has its id on disk. A file that cannot be
    /// parsed is an error: it is shown, never overwritten.
    pub fn open(store: Arc<dyn DocStore>, path: &Path, default_text: &str) -> Result<Self, String> {
        let existing = store.read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let is_new = existing.as_ref().map_or(true, |b| b.iter().all(u8::is_ascii_whitespace));
        let bytes = if is_new { default_text.as_bytes().to_vec() } else { existing.clone().unwrap_or_default() };
        let text = String::from_utf8(bytes.clone()).map_err(|_| format!("{}: not UTF-8 text", path.display()))?;
        let (value, assigned) = A::parse_text(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut doc = Self {
            path: path.to_path_buf(),
            value,
            history: History::default(),
            store,
            base_hash: if is_new { None } else { Some(content_hash(&bytes)) },
            generation: 0,
        };
        if is_new || assigned {
            doc.write_current().map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(doc)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn value(&self) -> &A {
        &self.value
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn is_dirty(&self) -> bool {
        self.history.is_dirty()
    }
    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }
    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }
    pub fn undo_label(&self) -> Option<&str> {
        self.history.undo_label()
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.history.redo_label()
    }

    /// Apply an edit. `change` returns whether it changed anything plus a
    /// result; a no-op leaves history and dirty state alone. `key` joins
    /// consecutive edits of one gesture into a single undo step.
    pub fn edit<R>(&mut self, label: &str, key: Option<&str>, change: impl FnOnce(&mut A) -> (bool, R)) -> R {
        let before = self.value.clone();
        let (changed, result) = change(&mut self.value);
        if changed {
            self.history.push(label, key, &before);
            self.generation += 1;
        } else {
            self.value = before;
        }
        result
    }

    /// Like [`edit`](Self::edit) for a change that always applies.
    pub fn apply(&mut self, label: &str, change: impl FnOnce(&mut A)) {
        self.edit(label, None, |a| {
            change(a);
            (true, ())
        });
    }

    /// The mouse or key driving a keyed edit was released.
    pub fn end_gesture(&mut self) {
        self.history.seal();
    }

    pub fn undo(&mut self) -> bool {
        match self.history.undo(&self.value) {
            Some(state) => {
                self.value = state;
                self.generation += 1;
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.history.redo(&self.value) {
            Some(state) => {
                self.value = state;
                self.generation += 1;
                true
            }
            None => false,
        }
    }

    fn write_current(&mut self) -> anyhow::Result<()> {
        let text = self.value.to_text()?;
        self.store.write_atomic(&self.path, text.as_bytes())?;
        self.base_hash = Some(content_hash(text.as_bytes()));
        self.history.mark_saved();
        Ok(())
    }

    fn disk_hash(&self) -> anyhow::Result<Option<u64>> {
        Ok(self.store.read(&self.path)?.map(|b| content_hash(&b)))
    }

    /// Whether the file changed since this document read or wrote it.
    pub fn changed_on_disk(&self) -> bool {
        self.disk_hash().ok().is_some_and(|disk| disk != self.base_hash)
    }

    /// Write the document unless the file changed underneath it.
    pub fn save(&mut self) -> anyhow::Result<SaveOutcome> {
        if let Some(disk) = self.disk_hash()? {
            if Some(disk) != self.base_hash {
                return Ok(SaveOutcome::Conflict { disk_hash: disk });
            }
        }
        self.write_current()?;
        Ok(SaveOutcome::Saved)
    }

    /// Write the document over whatever is on disk (the user chose "keep mine").
    pub fn save_overwrite(&mut self) -> anyhow::Result<()> {
        self.write_current()
    }

    /// Replace the document with the file's current content and clear history
    /// (the user chose "use theirs", or another tab saved).
    pub fn reload(&mut self) -> Result<(), String> {
        let bytes = self.store.read(&self.path).map_err(|e| e.to_string())?.ok_or("the file no longer exists")?;
        let text = String::from_utf8(bytes.clone()).map_err(|_| "not UTF-8 text".to_string())?;
        let (value, _) = A::parse_text(&text)?;
        self.value = value;
        self.base_hash = Some(content_hash(&bytes));
        self.history.reset();
        self.generation += 1;
        Ok(())
    }
}

macro_rules! migrating_asset {
    ($ty:ty, $kind:expr) => {
        impl AnimAsset for $ty {
            const KIND: &'static str = $kind;
            fn parse_text(text: &str) -> Result<(Self, bool), String> {
                <$ty>::parse_migrating(text).map(|(a, m)| (a, m.id_assigned)).map_err(|e| e.to_string())
            }
            fn to_text(&self) -> anyhow::Result<String> {
                <$ty>::to_text(self)
            }
            fn asset_id(&self) -> String {
                self.id.as_str().to_string()
            }
            fn asset_name(&self) -> String {
                self.name.clone()
            }
            fn set_asset_name(&mut self, name: String) {
                self.name = name;
            }
        }
    };
}

macro_rules! plain_asset {
    ($ty:ty, $kind:expr) => {
        impl AnimAsset for $ty {
            const KIND: &'static str = $kind;
            fn parse_text(text: &str) -> Result<(Self, bool), String> {
                <$ty>::parse(text).map(|a| (a, false)).map_err(|e| e.to_string())
            }
            fn to_text(&self) -> anyhow::Result<String> {
                <$ty>::to_text(self)
            }
            fn asset_id(&self) -> String {
                self.id.as_str().to_string()
            }
            fn asset_name(&self) -> String {
                self.name.clone()
            }
            fn set_asset_name(&mut self, name: String) {
                self.name = name;
            }
        }
    };
}

migrating_asset!(BlendSpaceAsset, pulsar_reflection::BlendSpaceAssetKind::KIND);
migrating_asset!(AnimationMontageAsset, pulsar_reflection::AnimationMontageAssetKind::KIND);
migrating_asset!(AnimationGraphAsset, pulsar_reflection::AnimationGraphAssetKind::KIND);
plain_asset!(RetargetProfileAsset, pulsar_reflection::RetargetProfileAssetKind::KIND);
plain_asset!(IkRigAsset, pulsar_reflection::IkRigAssetKind::KIND);
plain_asset!(IkRetargetAsset, pulsar_reflection::IkRetargeterAssetKind::KIND);
plain_asset!(PoseDbAsset, pulsar_reflection::PoseDatabaseAssetKind::KIND);

#[cfg(test)]
mod tests {
    use super::super::store::MemStore;
    use super::*;

    fn open(store: &MemStore) -> AnimDocument<BlendSpaceAsset> {
        AnimDocument::open(Arc::new(store.clone()), Path::new("a.blendspace"), &BlendSpaceAsset::default().to_text().unwrap()).unwrap()
    }

    #[test]
    fn new_files_are_written_once_with_their_id() {
        let store = MemStore::default();
        let doc = open(&store);
        let text = store.text("a.blendspace").expect("written on open");
        assert!(text.contains(&doc.value().asset_id()));
        assert!(!doc.is_dirty());
    }

    #[test]
    fn edit_undo_redo_save_and_reopen_round_trip() {
        let store = MemStore::default();
        let mut doc = open(&store);
        doc.apply("rename", |a| a.name = "Locomotion".into());
        assert!(doc.is_dirty());
        assert_eq!(doc.save().unwrap(), SaveOutcome::Saved);
        assert!(!doc.is_dirty());
        assert!(doc.undo() && doc.is_dirty());
        assert!(doc.redo() && !doc.is_dirty());
        let reopened = open(&store);
        assert_eq!(reopened.value().name, "Locomotion");
        assert_eq!(reopened.value().asset_id(), doc.value().asset_id());
    }

    #[test]
    fn a_no_op_edit_is_not_an_edit() {
        let store = MemStore::default();
        let mut doc = open(&store);
        doc.edit("nothing", None, |_| (false, ()));
        assert!(!doc.is_dirty() && !doc.can_undo());
    }

    #[test]
    fn save_refuses_to_overwrite_an_edit_made_by_another_tab() {
        let store = MemStore::default();
        let mut first = open(&store);
        let mut second = open(&store);
        first.apply("rename", |a| a.name = "From tab one".into());
        assert_eq!(first.save().unwrap(), SaveOutcome::Saved);
        assert!(second.changed_on_disk());
        second.apply("rename", |a| a.name = "From tab two".into());
        assert!(matches!(second.save().unwrap(), SaveOutcome::Conflict { .. }));
        assert!(store.text("a.blendspace").unwrap().contains("From tab one"), "the other tab's edit is intact");
        second.save_overwrite().unwrap();
        assert!(store.text("a.blendspace").unwrap().contains("From tab two"));
        first.reload().unwrap();
        assert_eq!(first.value().name, "From tab two");
        assert!(!first.is_dirty() && !first.can_undo());
    }

    #[test]
    fn an_unreadable_file_is_an_error_and_is_left_alone() {
        let store = MemStore::default();
        store.put("a.blendspace", "{ not json");
        let result = AnimDocument::<BlendSpaceAsset>::open(Arc::new(store.clone()), Path::new("a.blendspace"), "");
        assert!(result.is_err());
        assert_eq!(store.text("a.blendspace").unwrap(), "{ not json");
    }
}

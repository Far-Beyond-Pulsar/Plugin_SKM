//! Snapshot undo/redo with a saved marker, shared by every animation asset
//! editor. Each edit stores the document state before it; undo and redo swap
//! the live state with the stored one, so the history never holds a command
//! that could drift from the data.
//!
//! Dirty is `current revision != saved revision`, so undoing back to the saved
//! state (or redoing forward to it) is clean again.

#[derive(Clone)]
struct Entry<T> {
    label: String,
    state: T,
    revision: u64,
    /// Edits with the same key and no other edit between them form one undo
    /// step (a drag, a typed number).
    key: Option<String>,
}

#[derive(Clone)]
pub struct History<T: Clone> {
    undo: Vec<Entry<T>>,
    redo: Vec<Entry<T>>,
    revision: u64,
    next_revision: u64,
    saved_revision: u64,
    limit: usize,
}

impl<T: Clone> Default for History<T> {
    fn default() -> Self {
        Self::new(200)
    }
}

impl<T: Clone> History<T> {
    pub fn new(limit: usize) -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), revision: 0, next_revision: 1, saved_revision: 0, limit: limit.max(1) }
    }

    /// Record that `before` is the state an edit is about to change.
    pub fn push(&mut self, label: &str, key: Option<&str>, before: &T) {
        let coalesce = key.is_some() && self.undo.last().is_some_and(|e| e.key.as_deref() == key) && self.redo.is_empty();
        if !coalesce {
            self.undo.push(Entry { label: label.to_string(), state: before.clone(), revision: self.revision, key: key.map(str::to_string) });
            if self.undo.len() > self.limit {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.revision = self.next_revision;
        self.next_revision += 1;
        // Coalesced edits keep the revision of the first one's predecessor, so
        // returning to the saved state by undo is still recognised.
    }

    pub fn undo(&mut self, current: &T) -> Option<T> {
        let entry = self.undo.pop()?;
        self.redo.push(Entry { label: entry.label.clone(), state: current.clone(), revision: self.revision, key: None });
        self.revision = entry.revision;
        Some(entry.state)
    }

    pub fn redo(&mut self, current: &T) -> Option<T> {
        let entry = self.redo.pop()?;
        self.undo.push(Entry { label: entry.label.clone(), state: current.clone(), revision: self.revision, key: None });
        self.revision = entry.revision;
        Some(entry.state)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|e| e.label.as_str())
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }
    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }
    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }
    /// A state that did not come from this history (a reload) clears it.
    pub fn reset(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.revision = self.next_revision;
        self.next_revision += 1;
        self.saved_revision = self.revision;
    }
    /// Stop the next edit joining the previous one (mouse released).
    pub fn seal(&mut self) {
        if let Some(last) = self.undo.last_mut() {
            last.key = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_redo_and_dirty_follow_the_saved_state() {
        let mut h = History::<i32>::default();
        let mut v = 0;
        h.push("a", None, &v);
        v = 1;
        h.mark_saved();
        assert!(!h.is_dirty());
        h.push("b", None, &v);
        v = 2;
        assert!(h.is_dirty());
        v = h.undo(&v).unwrap();
        assert_eq!(v, 1);
        assert!(!h.is_dirty(), "undo back to the saved state is clean");
        v = h.undo(&v).unwrap();
        assert!(h.is_dirty());
        v = h.redo(&v).unwrap();
        assert!(!h.is_dirty());
        assert_eq!(v, 1);
        assert_eq!((h.can_undo(), h.can_redo()), (true, true));
        h.push("c", None, &v);
        assert!(!h.can_redo(), "a new edit drops redo");
    }

    #[test]
    fn keyed_edits_join_until_sealed() {
        let mut h = History::<i32>::default();
        let mut v = 0;
        for next in 1..=5 {
            h.push("drag", Some("drag"), &v);
            v = next;
        }
        assert_eq!(h.undo(&v), Some(0), "a drag is one undo step");
        assert!(h.undo(&0).is_none());
        let mut h = History::<i32>::default();
        h.push("drag", Some("drag"), &0);
        h.seal();
        h.push("drag", Some("drag"), &1);
        assert_eq!(h.undo(&2), Some(1));
        assert_eq!(h.undo(&1), Some(0));
    }
}

use crate::model::{Document, Raster};
use std::{collections::HashSet, sync::Arc};
use uuid::Uuid;

#[derive(Clone)]
struct Snapshot {
    document: Document,
    revision: Uuid,
}
struct Entry {
    name: String,
    before: Snapshot,
    after: Snapshot,
}

pub struct History {
    past: Vec<Entry>,
    future: Vec<Entry>,
    revision: Uuid,
    saved: Uuid,
    pub entry_limit: usize,
    pub byte_limit: usize,
}
impl Default for History {
    fn default() -> Self {
        let revision = Uuid::new_v4();
        Self {
            past: vec![],
            future: vec![],
            revision,
            saved: revision,
            entry_limit: 100,
            byte_limit: 256 * 1024 * 1024,
        }
    }
}
impl History {
    pub fn revision(&self) -> Uuid {
        self.revision
    }
    pub fn is_modified(&self) -> bool {
        self.revision != self.saved
    }
    pub fn mark_saved(&mut self, revision: Uuid) {
        self.saved = revision;
    }
    pub fn undo_name(&self) -> Option<&str> {
        self.past.last().map(|e| e.name.as_str())
    }
    pub fn redo_name(&self) -> Option<&str> {
        self.future.last().map(|e| e.name.as_str())
    }
    pub fn record(&mut self, name: impl Into<String>, before: Document, after: &Document) -> bool {
        let mut comparison = before.clone();
        comparison.active_layer_id = after.active_layer_id;
        if comparison == *after {
            return false;
        }
        let previous = self.revision;
        self.revision = Uuid::new_v4();
        self.past.push(Entry {
            name: name.into(),
            before: Snapshot {
                document: before,
                revision: previous,
            },
            after: Snapshot {
                document: after.clone(),
                revision: self.revision,
            },
        });
        self.future.clear();
        self.trim(after);
        true
    }
    pub fn undo(&mut self, document: &mut Document) -> bool {
        let Some(entry) = self.past.pop() else {
            return false;
        };
        *document = entry.before.document.clone();
        self.revision = entry.before.revision;
        self.future.push(entry);
        self.trim(document);
        true
    }
    /// Adjust the most recent interactive operation while preserving its original undo state.
    pub fn amend(&mut self, expected: Uuid, document: &Document) -> bool {
        if self.revision != expected || !self.future.is_empty() {
            return false;
        }
        let Some(entry) = self.past.last_mut() else {
            return false;
        };
        self.revision = Uuid::new_v4();
        entry.after = Snapshot {
            document: document.clone(),
            revision: self.revision,
        };
        self.trim(document);
        true
    }
    pub fn redo(&mut self, document: &mut Document) -> bool {
        let Some(entry) = self.future.pop() else {
            return false;
        };
        *document = entry.after.document.clone();
        self.revision = entry.after.revision;
        self.past.push(entry);
        self.trim(document);
        true
    }
    pub fn retained_bytes(&self, current: &Document) -> usize {
        fn add(r: &Raster, seen: &mut HashSet<usize>) -> usize {
            let mut bytes = 0;
            if seen.insert(Arc::as_ptr(&r.pixels) as usize) {
                bytes += r.pixels.len();
            }
            for t in r.tiles.values() {
                if seen.insert(Arc::as_ptr(t) as usize) {
                    bytes += t.len();
                }
            }
            bytes
        }
        fn visit(d: &Document, seen: &mut HashSet<usize>) -> usize {
            let mut n = 0;
            for l in &d.layers {
                if let Some(r) = &l.image {
                    n += add(r, seen);
                }
                if let Some(m) = &l.mask {
                    n += add(&m.raster, seen);
                }
            }
            if let Some(s) = &d.selection {
                for coverage in s.allocations() {
                    if seen.insert(Arc::as_ptr(coverage) as usize) {
                        n += coverage.len();
                    }
                }
            }
            n
        }
        let mut seen = HashSet::new();
        visit(current, &mut seen);
        self.past
            .iter()
            .chain(&self.future)
            .map(|e| visit(&e.before.document, &mut seen) + visit(&e.after.document, &mut seen))
            .sum()
    }
    fn trim(&mut self, current: &Document) {
        while self.past.len() + self.future.len() > self.entry_limit
            || self.retained_bytes(current) > self.byte_limit
        {
            if !self.past.is_empty() {
                self.past.remove(0);
            } else if !self.future.is_empty() {
                self.future.remove(0);
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Raster;
    #[test]
    fn revisions_and_async_save() {
        let mut d = Document::new(8, 8);
        let mut h = History::default();
        let a = d.clone();
        d.layers[0].name = "A".into();
        h.record("Rename", a, &d);
        let saving = h.revision();
        let a = d.clone();
        d.layers[0].opacity = 0.5;
        h.record("Opacity", a, &d);
        h.mark_saved(saving);
        assert!(h.is_modified());
        assert!(h.undo(&mut d));
        assert!(!h.is_modified());
        assert!(h.redo(&mut d));
        assert!(h.is_modified());
    }
    #[test]
    fn noop_keeps_redo() {
        let mut d = Document::new(4, 4);
        let mut h = History::default();
        let a = d.clone();
        d.layers[0].visible = false;
        h.record("Hide", a, &d);
        h.undo(&mut d);
        assert!(!h.record("No change", d.clone(), &d));
        assert!(h.redo(&mut d));
    }
    #[test]
    fn amended_operation_undoes_to_original_and_invalidates_saved_revision() {
        let mut doc = Document::new(4, 4);
        let original = doc.clone();
        let mut history = History::default();
        doc.layers[0].opacity = 0.5;
        history.record("Gradient", original.clone(), &doc);
        let revision = history.revision();
        history.mark_saved(revision);
        doc.layers[0].opacity = 0.25;
        assert!(history.amend(revision, &doc));
        assert!(history.is_modified());
        let amended = doc.clone();
        assert!(history.undo(&mut doc));
        assert_eq!(doc, original);
        assert!(!history.amend(revision, &doc));
        assert!(history.redo(&mut doc));
        assert_eq!(doc, amended);
    }
    #[test]
    fn history_budget_counts_shared_tiles_once() {
        let mut d = Document::new(4096, 4096);
        d.layers[0].image = Some(Arc::new(Raster::blank(4096, 4096)));
        let mut h = History::default();
        for i in 0..5 {
            let a = d.clone();
            Arc::make_mut(d.layers[0].image.as_mut().unwrap()).set(i * 128, 0, [255; 4]);
            h.record("Paint", a, &d);
        }
        assert_eq!(h.retained_bytes(&d), 0);
        let a = d.clone();
        Arc::make_mut(d.layers[0].image.as_mut().unwrap()).set(0, 0, [0; 4]);
        h.record("Paint", a, &d);
        assert_eq!(h.retained_bytes(&d), 128 * 128 * 4);
    }
}

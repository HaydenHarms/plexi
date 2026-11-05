//! Undo/redo by snapshot. Notes are small enough that copying a page's element list
//! before each edit is simpler and sturdier than recording inverse operations.

use plexi_core::{Element, Page};

/// Which element list an edit touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Page(usize),
    Overlay,
}

#[derive(Clone, Debug)]
pub enum Snapshot {
    Elements(Target, Vec<Element>),
    /// Page list changes (add, delete, reorder) store every page.
    Pages(Vec<Page>, usize),
}

const LIMIT: usize = 300;

#[derive(Default)]
pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl History {
    pub fn push(&mut self, s: Snapshot) {
        self.undo.push(s);
        if self.undo.len() > LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Pop the newest snapshot; the caller swaps it with the current state and hands
    /// the replaced state back through [`History::push_redo`].
    pub fn pop_undo(&mut self) -> Option<Snapshot> {
        self.undo.pop()
    }

    pub fn push_redo(&mut self, s: Snapshot) {
        self.redo.push(s);
    }

    pub fn pop_redo(&mut self) -> Option<Snapshot> {
        self.redo.pop()
    }

    /// Like `push` but keeps the redo stack (used when re-applying a redo).
    pub fn push_keep_redo(&mut self, s: Snapshot) {
        self.undo.push(s);
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

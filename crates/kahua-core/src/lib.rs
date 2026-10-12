//! The editing model shared by Kahua's GUI and CLI: an open document,
//! selection, and an undo/redo history of edits.

mod edit;
mod snapshot;

use std::path::{Path, PathBuf};

use anyhow::Result;
use kahua_io::rbx_dom_weak::types::{Ref, Variant};
use kahua_io::rbx_dom_weak::{Ustr, WeakDom};
use kahua_io::{Project, place, project};

pub use edit::Edit;
pub use kahua_io;
pub use snapshot::Snapshot;

/// Where a document came from and where it is saved to.
pub enum Source {
    /// A Rojo-compatible project directory.
    Project(Project),
    /// A single place or model file.
    File(PathBuf),
}

/// One undoable step, possibly made of several edits.
struct Transaction {
    label: String,
    edits: Vec<Edit>,
}

const MAX_HISTORY: usize = 500;

pub struct Document {
    pub dom: WeakDom,
    pub source: Source,
    pub selection: Vec<Ref>,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    /// Increases on every change; views use it to know when to refresh.
    revision: u64,
    saved_revision: u64,
    /// Key and time of the last coalesced edit, so typing or dragging a value
    /// becomes one undo step.
    coalesce: Option<(u64, std::time::Instant)>,
}

impl Document {
    /// Opens a project directory or a .rbxl/.rbxlx/.rbxm/.rbxmx file.
    pub fn open(path: &Path) -> Result<(Self, Vec<String>)> {
        if path.is_dir() {
            let loaded = Project::open(path)?;
            Ok((Self::new(loaded.dom, Source::Project(loaded.project)), loaded.warnings))
        } else {
            let dom = place::read_file(path)?;
            let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            Ok((Self::new(dom, Source::File(path)), Vec::new()))
        }
    }

    pub fn new(dom: WeakDom, source: Source) -> Self {
        Self {
            dom,
            source,
            selection: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            revision: 0,
            saved_revision: 0,
            coalesce: None,
        }
    }

    pub fn title(&self) -> String {
        match &self.source {
            Source::Project(p) => p.name().to_owned(),
            Source::File(path) => {
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled".into())
            }
        }
    }

    pub fn path(&self) -> &Path {
        match &self.source {
            Source::Project(p) => p.dir(),
            Source::File(path) => path,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Marks the document as changed outside of the history (e.g. live drags).
    pub fn touch(&mut self) {
        self.revision += 1;
    }

    /// Saves to where the document came from. Returns warnings.
    pub fn save(&mut self) -> Result<Vec<String>> {
        let warnings = match &mut self.source {
            Source::Project(project) => project.save(&self.dom)?,
            Source::File(path) => {
                place::write_file(&self.dom, path)?;
                Vec::new()
            }
        };
        self.saved_revision = self.revision;
        Ok(warnings)
    }

    /// Writes the place to a file without changing where the document saves.
    pub fn export(&self, path: &Path) -> Result<()> {
        place::write_file(&self.dom, path)
    }

    /// Converts a file-backed document into a new project directory.
    pub fn save_as_project(&mut self, dir: &Path, name: &str, git: bool) -> Result<Vec<String>> {
        let options = project::CreateOptions { git };
        // Keep editing the same DOM, so refs, selection and history stay valid.
        let (project, warnings) = project::create_from_dom(dir, name, &self.dom, &options)?;
        self.source = Source::Project(project);
        self.saved_revision = self.revision;
        Ok(warnings)
    }

    // -- history ----------------------------------------------------------

    /// Applies edits as one undoable step.
    pub fn perform(&mut self, label: impl Into<String>, edits: Vec<Edit>) {
        if edits.is_empty() {
            return;
        }
        for edit in &edits {
            edit.apply(&mut self.dom);
        }
        self.record(label, edits);
    }

    /// Applies edits, merging them into the previous step if it had the same
    /// `key` and happened less than a second ago (typing, dragging a slider).
    pub fn perform_coalesced(&mut self, label: impl Into<String>, edits: Vec<Edit>, key: u64) {
        if edits.is_empty() {
            return;
        }
        for edit in &edits {
            edit.apply(&mut self.dom);
        }
        let now = std::time::Instant::now();
        let mergeable = self.redo.is_empty()
            && self.coalesce.is_some_and(|(k, t)| k == key && now.duration_since(t).as_secs_f32() < 1.0);
        if let (true, Some(last)) = (mergeable, self.undo.last_mut()) {
            for edit in edits {
                let merged = match &edit {
                    Edit::SetProperty { target, key, new, .. } => last.edits.iter_mut().any(|e| match e {
                        Edit::SetProperty { target: t, key: k, new: n, .. } if t == target && k == key => {
                            *n = new.clone();
                            true
                        }
                        _ => false,
                    }),
                    Edit::Rename { target, new, .. } => last.edits.iter_mut().any(|e| match e {
                        Edit::Rename { target: t, new: n, .. } if t == target => {
                            *n = new.clone();
                            true
                        }
                        _ => false,
                    }),
                    _ => false,
                };
                if !merged {
                    last.edits.push(edit);
                }
            }
            self.revision += 1;
        } else {
            self.record(label, edits);
        }
        self.coalesce = Some((key, now));
    }

    /// Records edits that were already applied (e.g. at the end of a drag).
    pub fn record(&mut self, label: impl Into<String>, edits: Vec<Edit>) {
        self.coalesce = None;
        if edits.is_empty() {
            return;
        }
        self.undo.push(Transaction { label: label.into(), edits });
        if self.undo.len() > MAX_HISTORY {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.revision += 1;
        self.prune_selection();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|t| t.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|t| t.label.as_str())
    }

    pub fn undo(&mut self) -> bool {
        self.coalesce = None;
        let Some(tx) = self.undo.pop() else { return false };
        for edit in tx.edits.iter().rev() {
            edit.revert(&mut self.dom);
        }
        self.redo.push(tx);
        self.revision += 1;
        self.prune_selection();
        true
    }

    pub fn redo(&mut self) -> bool {
        self.coalesce = None;
        let Some(tx) = self.redo.pop() else { return false };
        for edit in &tx.edits {
            edit.apply(&mut self.dom);
        }
        self.undo.push(tx);
        self.revision += 1;
        self.prune_selection();
        true
    }

    fn prune_selection(&mut self) {
        let dom = &self.dom;
        self.selection.retain(|r| dom.get_by_ref(*r).is_some());
    }

    // -- edit builders ----------------------------------------------------

    /// An edit that sets (or with `None`, removes) a property.
    pub fn set_property(&self, target: Ref, key: &str, value: Option<Variant>) -> Option<Edit> {
        let inst = self.dom.get_by_ref(target)?;
        let key = Ustr::from(key);
        let old = inst.properties.get(&key).cloned();
        (old != value).then_some(Edit::SetProperty { target, key, old, new: value })
    }

    pub fn rename(&self, target: Ref, name: &str) -> Option<Edit> {
        let inst = self.dom.get_by_ref(target)?;
        (inst.name != name).then(|| Edit::Rename { target, old: inst.name.clone(), new: name.to_owned() })
    }

    /// An edit that inserts a new instance tree. The new root's ref is `snapshot.referent`.
    pub fn create(&self, parent: Ref, snapshot: Snapshot) -> Edit {
        let index = self.dom.get_by_ref(parent).map_or(0, |p| p.children().len());
        Edit::Create { parent, index, snapshot }
    }

    pub fn destroy(&self, target: Ref) -> Option<Edit> {
        let inst = self.dom.get_by_ref(target)?;
        let parent = inst.parent();
        if parent.is_none() {
            return None; // never delete the DataModel
        }
        let index = sibling_index(&self.dom, target);
        Some(Edit::Destroy { parent, index, snapshot: Snapshot::capture(&self.dom, target) })
    }

    pub fn move_to(&self, target: Ref, new_parent: Ref) -> Option<Edit> {
        let inst = self.dom.get_by_ref(target)?;
        let old_parent = inst.parent();
        if old_parent == new_parent || old_parent.is_none() || self.is_ancestor_or_self(target, new_parent) {
            return None;
        }
        Some(Edit::Move { target, old_parent, old_index: sibling_index(&self.dom, target), new_parent })
    }

    /// True if `ancestor` is `node` or one of its ancestors.
    pub fn is_ancestor_or_self(&self, ancestor: Ref, node: Ref) -> bool {
        let mut current = node;
        while current.is_some() {
            if current == ancestor {
                return true;
            }
            current = self.dom.get_by_ref(current).map_or(Ref::none(), |i| i.parent());
        }
        false
    }

    /// Deletes the selected instances (skipping services and descendants of other selected).
    pub fn delete_selection(&mut self) {
        let roots = self.selection_roots();
        let edits: Vec<Edit> =
            roots.into_iter().filter(|r| !self.is_service(*r)).filter_map(|r| self.destroy(r)).collect();
        self.perform("Delete", edits);
        self.selection.clear();
    }

    /// Duplicates the selected instances next to the originals and selects the copies.
    pub fn duplicate_selection(&mut self) {
        let roots: Vec<Ref> = self.selection_roots().into_iter().filter(|r| !self.is_service(*r)).collect();
        let mut edits = Vec::new();
        let mut copies = Vec::new();
        for r in roots {
            let parent = self.dom.get_by_ref(r).unwrap().parent();
            let snapshot = Snapshot::capture(&self.dom, r).with_fresh_refs();
            copies.push(snapshot.referent);
            edits.push(self.create(parent, snapshot));
        }
        self.perform("Duplicate", edits);
        self.selection = copies;
    }

    /// Selected instances whose ancestors are not also selected.
    pub fn selection_roots(&self) -> Vec<Ref> {
        self.selection
            .iter()
            .copied()
            .filter(|&r| !self.selection.iter().any(|&other| other != r && self.is_ancestor_or_self(other, r)))
            .collect()
    }

    /// Direct children of the DataModel that are services can't be deleted or moved.
    pub fn is_service(&self, r: Ref) -> bool {
        self.dom
            .get_by_ref(r)
            .is_some_and(|i| i.parent() == self.dom.root_ref() && kahua_io::reflect::is_service(&i.class))
    }
}

pub(crate) fn sibling_index(dom: &WeakDom, r: Ref) -> usize {
    let Some(inst) = dom.get_by_ref(r) else { return 0 };
    dom.get_by_ref(inst.parent()).and_then(|p| p.children().iter().position(|c| *c == r)).unwrap_or(0)
}

/// Moves `r` (already a child of its parent) to position `index` among its siblings.
pub(crate) fn place_at_index(dom: &mut WeakDom, r: Ref, index: usize) {
    let parent = dom.get_by_ref(r).unwrap().parent();
    let siblings = dom.get_by_ref(parent).unwrap().children().to_vec();
    let after: Vec<Ref> = siblings.iter().copied().filter(|s| *s != r).skip(index).collect();
    for s in after {
        dom.transfer_within(s, parent);
    }
}

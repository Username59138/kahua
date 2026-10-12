use kahua_io::rbx_dom_weak::types::{Ref, Variant};
use kahua_io::rbx_dom_weak::{Ustr, WeakDom};

use crate::{Snapshot, place_at_index, sibling_index};

/// A reversible change to the DOM.
#[derive(Debug, Clone)]
pub enum Edit {
    SetProperty { target: Ref, key: Ustr, old: Option<Variant>, new: Option<Variant> },
    Rename { target: Ref, old: String, new: String },
    Create { parent: Ref, index: usize, snapshot: Snapshot },
    Destroy { parent: Ref, index: usize, snapshot: Snapshot },
    Move { target: Ref, old_parent: Ref, old_index: usize, new_parent: Ref },
}

impl Edit {
    pub fn apply(&self, dom: &mut WeakDom) {
        match self {
            Edit::SetProperty { target, key, new, .. } => set(dom, *target, *key, new),
            Edit::Rename { target, new, .. } => rename(dom, *target, new),
            Edit::Create { parent, index, snapshot } => insert(dom, *parent, *index, snapshot),
            Edit::Destroy { snapshot, .. } => remove(dom, snapshot.referent),
            Edit::Move { target, new_parent, .. } => {
                if dom.get_by_ref(*target).is_some() && dom.get_by_ref(*new_parent).is_some() {
                    dom.transfer_within(*target, *new_parent);
                }
            }
        }
    }

    pub fn revert(&self, dom: &mut WeakDom) {
        match self {
            Edit::SetProperty { target, key, old, .. } => set(dom, *target, *key, old),
            Edit::Rename { target, old, .. } => rename(dom, *target, old),
            Edit::Create { snapshot, .. } => remove(dom, snapshot.referent),
            Edit::Destroy { parent, index, snapshot } => insert(dom, *parent, *index, snapshot),
            Edit::Move { target, old_parent, old_index, .. } => {
                if dom.get_by_ref(*target).is_some() && dom.get_by_ref(*old_parent).is_some() {
                    dom.transfer_within(*target, *old_parent);
                    place_at_index(dom, *target, *old_index);
                }
            }
        }
    }
}

fn set(dom: &mut WeakDom, target: Ref, key: Ustr, value: &Option<Variant>) {
    if let Some(inst) = dom.get_by_ref_mut(target) {
        match value {
            Some(v) => {
                inst.properties.insert(key, v.clone());
            }
            None => {
                inst.properties.remove(&key);
            }
        }
    }
}

fn rename(dom: &mut WeakDom, target: Ref, name: &str) {
    if let Some(inst) = dom.get_by_ref_mut(target) {
        inst.name = name.to_owned();
    }
}

fn insert(dom: &mut WeakDom, parent: Ref, index: usize, snapshot: &Snapshot) {
    if dom.get_by_ref(parent).is_none() || dom.get_by_ref(snapshot.referent).is_some() {
        return;
    }
    let r = dom.insert(parent, snapshot.to_builder());
    place_at_index(dom, r, index);
    debug_assert_eq!(sibling_index(dom, r), index.min(dom.get_by_ref(parent).unwrap().children().len() - 1));
}

fn remove(dom: &mut WeakDom, r: Ref) {
    if dom.get_by_ref(r).is_some() {
        dom.destroy(r);
    }
}

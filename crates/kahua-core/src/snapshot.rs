use std::collections::HashMap;

use kahua_io::rbx_dom_weak::types::{Ref, Variant};
use kahua_io::rbx_dom_weak::{InstanceBuilder, Ustr, WeakDom};

/// A detached copy of an instance tree, used for undo, duplicate and clipboard.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub referent: Ref,
    pub name: String,
    pub class: Ustr,
    pub properties: Vec<(Ustr, Variant)>,
    pub children: Vec<Snapshot>,
}

impl Snapshot {
    /// A brand new instance with no properties.
    pub fn new(class: &str, name: &str) -> Self {
        Self {
            referent: Ref::new(),
            name: name.to_owned(),
            class: Ustr::from(class),
            properties: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn with_property(mut self, key: &str, value: impl Into<Variant>) -> Self {
        self.properties.push((Ustr::from(key), value.into()));
        self
    }

    /// Copies a subtree, keeping its refs (so it can be restored exactly).
    pub fn capture(dom: &WeakDom, r: Ref) -> Self {
        let inst = dom.get_by_ref(r).expect("snapshot of a live instance");
        let mut properties: Vec<(Ustr, Variant)> = inst.properties.iter().map(|(k, v)| (*k, v.clone())).collect();
        properties.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        Self {
            referent: r,
            name: inst.name.clone(),
            class: inst.class,
            properties,
            children: inst.children().iter().map(|&c| Self::capture(dom, c)).collect(),
        }
    }

    /// A copy with new refs. References between instances inside the copy are
    /// remapped to the new instances, like `Instance:Clone()` in Roblox.
    pub fn with_fresh_refs(&self) -> Self {
        let mut map = HashMap::new();
        self.collect_refs(&mut map);
        self.remap(&map)
    }

    fn collect_refs(&self, map: &mut HashMap<Ref, Ref>) {
        map.insert(self.referent, Ref::new());
        for child in &self.children {
            child.collect_refs(map);
        }
    }

    fn remap(&self, map: &HashMap<Ref, Ref>) -> Self {
        Self {
            referent: map[&self.referent],
            name: self.name.clone(),
            class: self.class,
            properties: self
                .properties
                .iter()
                .map(|(k, v)| {
                    let v = match v {
                        Variant::Ref(target) => Variant::Ref(*map.get(target).unwrap_or(target)),
                        other => other.clone(),
                    };
                    (*k, v)
                })
                .collect(),
            children: self.children.iter().map(|c| c.remap(map)).collect(),
        }
    }

    pub fn to_builder(&self) -> InstanceBuilder {
        InstanceBuilder::new(self.class)
            .with_referent(self.referent)
            .with_name(self.name.clone())
            .with_properties(self.properties.iter().cloned())
            .with_children(self.children.iter().map(Snapshot::to_builder))
    }
}

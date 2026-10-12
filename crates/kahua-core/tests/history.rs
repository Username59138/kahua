use kahua_core::kahua_io::rbx_dom_weak::types::{Ref, Variant};
use kahua_core::kahua_io::rbx_dom_weak::{InstanceBuilder, WeakDom};
use kahua_core::kahua_io::template;
use kahua_core::{Document, Snapshot, Source};

fn doc() -> (Document, Ref) {
    let mut dom: WeakDom = template::baseplate();
    let ws = template::workspace(&dom);
    for name in ["A", "B", "C"] {
        dom.insert(ws, InstanceBuilder::new("Part").with_name(name));
    }
    (Document::new(dom, Source::File("test.rbxl".into())), ws)
}

fn child_names(doc: &Document, parent: Ref) -> Vec<String> {
    doc.dom
        .get_by_ref(parent)
        .unwrap()
        .children()
        .iter()
        .map(|c| doc.dom.get_by_ref(*c).unwrap().name.clone())
        .collect()
}

fn by_name(doc: &Document, parent: Ref, name: &str) -> Ref {
    *doc.dom
        .get_by_ref(parent)
        .unwrap()
        .children()
        .iter()
        .find(|c| doc.dom.get_by_ref(**c).unwrap().name == name)
        .unwrap()
}

#[test]
fn delete_undo_restores_order_and_refs() {
    let (mut doc, ws) = doc();
    let before = child_names(&doc, ws);
    let b = by_name(&doc, ws, "B");
    doc.selection = vec![b];
    doc.delete_selection();
    assert!(doc.dom.get_by_ref(b).is_none());
    assert!(doc.is_dirty());
    doc.undo();
    assert_eq!(child_names(&doc, ws), before);
    assert!(doc.dom.get_by_ref(b).is_some(), "same ref after undo");
    doc.redo();
    assert!(doc.dom.get_by_ref(b).is_none());
}

#[test]
fn services_cannot_be_deleted() {
    let (mut doc, ws) = doc();
    doc.selection = vec![ws];
    doc.delete_selection();
    assert!(doc.dom.get_by_ref(ws).is_some());
    assert!(!doc.can_undo());
}

#[test]
fn duplicate_remaps_internal_references() {
    let (mut doc, ws) = doc();
    let model = Snapshot::new("Model", "Door");
    let model_ref = model.referent;
    let edit = doc.create(ws, model);
    doc.perform("Insert", vec![edit]);
    let part = Snapshot::new("Part", "Hinge");
    let part_ref = part.referent;
    let edit = doc.create(model_ref, part);
    doc.perform("Insert", vec![edit]);
    let edit = doc.set_property(model_ref, "PrimaryPart", Some(Variant::Ref(part_ref))).unwrap();
    doc.perform("Set", vec![edit]);

    doc.selection = vec![model_ref];
    doc.duplicate_selection();
    let copy = doc.selection[0];
    assert_ne!(copy, model_ref);
    let copy_part = doc.dom.get_by_ref(copy).unwrap().children()[0];
    match doc.dom.get_by_ref(copy).unwrap().properties.get(&"PrimaryPart".into()) {
        Some(Variant::Ref(r)) => assert_eq!(*r, copy_part),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn move_and_rename_undo() {
    let (mut doc, ws) = doc();
    let a = by_name(&doc, ws, "A");
    let c = by_name(&doc, ws, "C");
    let edit = doc.move_to(a, c).unwrap();
    doc.perform("Move", vec![edit]);
    assert_eq!(doc.dom.get_by_ref(a).unwrap().parent(), c);
    assert!(doc.move_to(c, a).is_none(), "can't move into own descendant");
    let edit = doc.rename(a, "Renamed").unwrap();
    doc.perform("Rename", vec![edit]);
    doc.undo();
    doc.undo();
    assert_eq!(child_names(&doc, ws)[..2], ["Baseplate".to_string(), "SpawnLocation".into()]);
    assert_eq!(by_name(&doc, ws, "A"), a);
    assert_eq!(child_names(&doc, ws)[2], "A");
}

#[test]
fn coalesced_edits_are_one_undo_step() {
    let (mut doc, ws) = doc();
    let a = by_name(&doc, ws, "A");
    for i in 1..=5 {
        let edit = doc.set_property(a, "Transparency", Some(Variant::Float32(i as f32 / 10.0))).unwrap();
        doc.perform_coalesced("Set Transparency", vec![edit], 42);
    }
    assert_eq!(doc.dom.get_by_ref(a).unwrap().properties.get(&"Transparency".into()), Some(&Variant::Float32(0.5)));
    doc.undo();
    assert_eq!(doc.dom.get_by_ref(a).unwrap().properties.get(&"Transparency".into()), None);
    assert!(!doc.can_undo());
}

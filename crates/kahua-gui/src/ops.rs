//! Editing operations shared by menus, toolbar, shortcuts and the viewport.

use kahua_core::kahua_io::rbx_dom_weak::types::{CFrame, Color3, Enum, Ref, Variant, Vector3};
use kahua_core::kahua_io::rbx_dom_weak::{Ustr, WeakDom};
use kahua_core::kahua_io::reflect;
use kahua_core::{Document, Edit, Snapshot};
use kahua_render::glam::{Quat, Vec3};
use kahua_render::scene::{self, cframe_to_mat4, mat4_to_cframe, world_root};
use kahua_render::{Camera, Scene};

/// Every BasePart that moves when the selection moves.
pub fn parts_to_transform(doc: &Document) -> Vec<Ref> {
    let mut out = Vec::new();
    for root in doc.selection_roots() {
        for inst in doc.dom.descendants_of(root) {
            if scene::is_renderable(inst) && scene::part_cframe(inst).is_some() {
                out.push(inst.referent());
            }
        }
    }
    out
}

/// Models in the selection (and inside it) whose pivot should follow the parts.
pub fn models_to_transform(doc: &Document) -> Vec<Ref> {
    let mut out = Vec::new();
    for root in doc.selection_roots() {
        for inst in doc.dom.descendants_of(root) {
            if reflect::is_a(&inst.class, "Model")
                && inst.class != "Workspace"
                && matches!(scene::prop(inst, "WorldPivot"), Some(Variant::CFrame(_)))
            {
                out.push(inst.referent());
            }
        }
    }
    out
}

pub fn cframe_of(dom: &WeakDom, r: Ref, key: &str) -> Option<CFrame> {
    match dom.get_by_ref(r)?.properties.get(&Ustr::from(key)) {
        Some(Variant::CFrame(cf)) => Some(*cf),
        _ => None,
    }
}

/// World-space bounds of the selected parts.
pub fn selection_bounds(
    doc: &Document,
    scene: &Scene,
    index: &std::collections::HashMap<Ref, usize>,
) -> Option<(Vec3, Vec3)> {
    let corners =
        parts_to_transform(doc).into_iter().filter_map(|r| index.get(&r)).flat_map(|&i| scene.parts[i].corners());
    scene::bounds_of(corners)
}

/// Applies a rigid transform to all selected parts (and model pivots) as one undo step.
pub fn transform_selection(doc: &mut Document, label: &str, f: impl Fn(CFrame) -> CFrame) {
    let mut edits = Vec::new();
    for r in parts_to_transform(doc) {
        if let Some(cf) = cframe_of(&doc.dom, r, "CFrame") {
            edits.extend(doc.set_property(r, "CFrame", Some(Variant::CFrame(f(cf)))));
        }
    }
    for r in models_to_transform(doc) {
        if let Some(cf) = cframe_of(&doc.dom, r, "WorldPivot") {
            edits.extend(doc.set_property(r, "WorldPivot", Some(Variant::CFrame(f(cf)))));
        }
    }
    doc.perform(label, edits);
}

/// Rotates the selection 90° around the given world axis through its center
/// (Ctrl+R rotates around Y, Ctrl+T tilts around X — like Studio).
pub fn rotate_selection(
    doc: &mut Document,
    scene: &Scene,
    index: &std::collections::HashMap<Ref, usize>,
    axis: Vec3,
    label: &str,
) {
    let Some((min, max)) = selection_bounds(doc, scene, index) else { return };
    let center = (min + max) * 0.5;
    let rot = Quat::from_axis_angle(axis, std::f32::consts::FRAC_PI_2);
    transform_selection(doc, label, |cf| {
        let m = cframe_to_mat4(&cf);
        let (_, r, p) = m.to_scale_rotation_translation();
        let new = kahua_render::glam::Mat4::from_rotation_translation(rot * r, center + rot * (p - center));
        snap_rotation(mat4_to_cframe(&new))
    });
}

/// Cleans up floating point noise in rotation matrices after 90° turns.
fn snap_rotation(mut cf: CFrame) -> CFrame {
    let clean = |v: f32| {
        let r = v.round();
        if (v - r).abs() < 1e-5 { r } else { v }
    };
    for row in [&mut cf.orientation.x, &mut cf.orientation.y, &mut cf.orientation.z] {
        row.x = clean(row.x);
        row.y = clean(row.y);
        row.z = clean(row.z);
    }
    cf
}

#[derive(Clone, Copy)]
pub enum PartKind {
    Block,
    Sphere,
    Cylinder,
    Wedge,
    CornerWedge,
}

/// Inserts a part in front of the camera, resting on whatever is there.
pub fn insert_part(doc: &mut Document, scene: &Scene, camera: &Camera, kind: PartKind, snap: Option<f32>) -> Ref {
    let (class, shape, size) = match kind {
        PartKind::Block => ("Part", Some(1), Vec3::new(4.0, 1.0, 2.0)),
        PartKind::Sphere => ("Part", Some(0), Vec3::splat(4.0)),
        PartKind::Cylinder => ("Part", Some(2), Vec3::new(2.0, 4.0, 4.0)),
        PartKind::Wedge => ("WedgePart", None, Vec3::new(4.0, 1.0, 2.0)),
        PartKind::CornerWedge => ("CornerWedgePart", None, Vec3::new(2.0, 2.0, 2.0)),
    };
    let (origin, dir) = (camera.position, camera.forward());
    let mut position = match scene.pick(origin, dir) {
        Some((_, t)) if t < 500.0 => origin + dir * t + Vec3::Y * size.y * 0.5,
        _ => origin + dir * 20.0,
    };
    if let Some(step) = snap {
        position.x = (position.x / step).round() * step;
        position.z = (position.z / step).round() * step;
    }
    let mut snapshot = Snapshot::new(class, class)
        .with_property("Anchored", true)
        .with_property("Size", Vector3::new(size.x, size.y, size.z))
        .with_property(
            "CFrame",
            CFrame::new(
                Vector3::new(position.x, position.y, position.z),
                kahua_core::kahua_io::rbx_dom_weak::types::Matrix3::identity(),
            ),
        )
        .with_property("Color", Color3::new(0.639, 0.635, 0.647));
    if let Some(shape) = shape {
        snapshot = snapshot.with_property("Shape", Enum::from_u32(shape));
    }
    let r = snapshot.referent;
    let parent = world_root(&doc.dom);
    let edit = doc.create(parent, snapshot);
    doc.perform(format!("Insert {class}"), vec![edit]);
    doc.selection = vec![r];
    r
}

/// Classes offered in the "Insert Object" menus.
pub const INSERTABLE: &[&str] = &[
    "Part",
    "Model",
    "Folder",
    "Script",
    "LocalScript",
    "ModuleScript",
    "SpawnLocation",
    "WedgePart",
    "CornerWedgePart",
    "TrussPart",
    "Seat",
    "PointLight",
    "SpotLight",
    "Sound",
    "Decal",
    "Texture",
    "Attachment",
    "WeldConstraint",
    "ClickDetector",
    "ProximityPrompt",
    "ScreenGui",
    "Frame",
    "TextLabel",
    "TextButton",
    "ImageLabel",
    "StringValue",
    "NumberValue",
    "BoolValue",
    "IntValue",
    "ObjectValue",
    "RemoteEvent",
    "RemoteFunction",
    "BindableEvent",
    "Tool",
    "Humanoid",
];

/// Inserts a new instance of `class` into `parent` and selects it.
pub fn insert_object(doc: &mut Document, parent: Ref, class: &str, camera: &Camera, scene: &Scene) -> Ref {
    let mut snapshot = Snapshot::new(class, class);
    if reflect::is_script(class) {
        let source = match class {
            "ModuleScript" => "local module = {}\n\nreturn module\n",
            _ => "print(\"Hello world!\")\n",
        };
        snapshot = snapshot.with_property("Source", source);
    }
    if reflect::is_base_part(class) {
        let (origin, dir) = (camera.position, camera.forward());
        let p = match scene.pick(origin, dir) {
            Some((_, t)) if t < 500.0 => origin + dir * t + Vec3::Y * 0.5,
            _ => origin + dir * 20.0,
        };
        snapshot = snapshot.with_property("Anchored", true).with_property(
            "CFrame",
            CFrame::new(
                Vector3::new(p.x.round(), p.y, p.z.round()),
                kahua_core::kahua_io::rbx_dom_weak::types::Matrix3::identity(),
            ),
        );
    }
    let r = snapshot.referent;
    let edit = doc.create(parent, snapshot);
    doc.perform(format!("Insert {class}"), vec![edit]);
    doc.selection = vec![r];
    r
}

/// Ctrl+G: put the selection into a new Model.
pub fn group(doc: &mut Document) {
    let roots: Vec<Ref> = doc.selection_roots().into_iter().filter(|r| !doc.is_service(*r)).collect();
    let Some(&first) = roots.first() else { return };
    let parent = doc.dom.get_by_ref(first).unwrap().parent();
    let model = Snapshot::new("Model", "Model");
    let model_ref = model.referent;
    let mut edits = vec![doc.create(parent, model)];
    // Apply the create first so the moves can target the new model.
    edits[0].apply(&mut doc.dom);
    for r in &roots {
        if let Some(e) = doc.move_to(*r, model_ref) {
            e.apply(&mut doc.dom);
            edits.push(e);
        }
    }
    doc.record("Group", edits);
    doc.selection = vec![model_ref];
}

/// Ctrl+U: replace selected Models/Folders with their children.
pub fn ungroup(doc: &mut Document) {
    let mut edits: Vec<Edit> = Vec::new();
    let mut new_selection = Vec::new();
    for r in doc.selection_roots() {
        let Some(inst) = doc.dom.get_by_ref(r) else { continue };
        if !(inst.class == "Model" || inst.class == "Folder") {
            continue;
        }
        let parent = inst.parent();
        for child in inst.children().to_vec() {
            if let Some(e) = doc.move_to(child, parent) {
                e.apply(&mut doc.dom);
                edits.push(e);
                new_selection.push(child);
            }
        }
        if let Some(e) = doc.destroy(r) {
            e.apply(&mut doc.dom);
            edits.push(e);
        }
    }
    doc.record("Ungroup", edits);
    doc.selection = new_selection;
}

pub fn copy(doc: &Document) -> Vec<Snapshot> {
    doc.selection_roots().into_iter().filter(|r| !doc.is_service(*r)).map(|r| Snapshot::capture(&doc.dom, r)).collect()
}

pub fn paste(doc: &mut Document, clipboard: &[Snapshot], into: Option<Ref>) {
    if clipboard.is_empty() {
        return;
    }
    let parent = into
        .or_else(|| {
            doc.selection.first().and_then(|r| doc.dom.get_by_ref(*r)).map(|i| i.parent()).filter(|p| p.is_some())
        })
        .unwrap_or_else(|| world_root(&doc.dom));
    let mut edits = Vec::new();
    let mut selection = Vec::new();
    for snapshot in clipboard {
        let fresh = snapshot.with_fresh_refs();
        selection.push(fresh.referent);
        edits.push(doc.create(parent, fresh));
    }
    doc.perform("Paste", edits);
    doc.selection = selection;
}

/// What clicking a part selects: its outermost Model ancestor, like Studio
/// (hold Alt to select the part itself).
pub fn click_target(dom: &WeakDom, part: Ref, select_part: bool) -> Ref {
    if select_part {
        return part;
    }
    let world = world_root(dom);
    let mut target = part;
    let mut current = dom.get_by_ref(part).map_or(Ref::none(), |i| i.parent());
    while current.is_some() && current != world {
        let Some(inst) = dom.get_by_ref(current) else { break };
        if inst.class == "Model" || inst.class == "Tool" {
            target = current;
        }
        current = inst.parent();
    }
    target
}

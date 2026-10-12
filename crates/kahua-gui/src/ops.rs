//! Editing operations shared by menus, toolbar, shortcuts and the viewport.

use kahua_core::kahua_io::rbx_dom_weak::types::{CFrame, Color3, Enum, Ref, Variant, Vector3};
use kahua_core::kahua_io::rbx_dom_weak::{Ustr, WeakDom};
use kahua_core::kahua_io::reflect;
use kahua_core::{Document, Edit, Snapshot};
use kahua_render::glam::{Mat4, Quat, Vec3};
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
    transform_selection(doc, label, |cf| rotate_cframe_about(cf, center, axis, std::f32::consts::FRAC_PI_2));
}

/// Turns a CFrame by `angle` radians around the world-space `axis` (unit length)
/// passing through `center`. The position orbits the center and the orientation turns with it.
pub fn rotate_cframe_about(cf: CFrame, center: Vec3, axis: Vec3, angle: f32) -> CFrame {
    let turn = Quat::from_axis_angle(axis, angle);
    let (_, rot, pos) = cframe_to_mat4(&cf).to_scale_rotation_translation();
    let new = Mat4::from_rotation_translation(turn * rot, center + turn * (pos - center));
    snap_rotation(mat4_to_cframe(&new))
}

/// The smallest Size a part can be dragged to, in studs.
pub const MIN_PART_SIZE: f32 = 0.05;

/// The new length and center of a box after one of its faces moved `outward` studs
/// along the unit vector `dir`. The opposite face stays in place, unless `symmetric`:
/// then both faces move and the center stays. `snap` rounds the new length.
pub fn resize_along(
    center: Vec3,
    dir: Vec3,
    size: f32,
    outward: f32,
    symmetric: bool,
    snap: Option<f32>,
) -> (f32, Vec3) {
    let raw = if symmetric { size + 2.0 * outward } else { size + outward };
    let snapped = match snap {
        Some(step) => (raw / step).round() * step,
        None => raw,
    };
    let new = snapped.max(MIN_PART_SIZE);
    let new_center = if symmetric { center } else { center + dir * (new - size) * 0.5 };
    (new, new_center)
}

/// Ctrl+A: selects every direct child of the world root that is not locked.
pub fn select_all(doc: &mut Document) {
    let world = world_root(&doc.dom);
    let Some(inst) = doc.dom.get_by_ref(world) else { return };
    let selection = inst
        .children()
        .iter()
        .copied()
        .filter(|r| {
            doc.dom
                .get_by_ref(*r)
                .is_some_and(|child| !matches!(scene::prop(child, "Locked"), Some(Variant::Bool(true))))
        })
        .collect();
    doc.selection = selection;
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

#[cfg(test)]
mod tests {
    use super::*;
    use kahua_core::kahua_io::rbx_dom_weak::types::Matrix3;

    fn cf_at(p: Vec3) -> CFrame {
        CFrame::new(Vector3::new(p.x, p.y, p.z), Matrix3::identity())
    }

    fn pos(cf: &CFrame) -> Vec3 {
        Vec3::new(cf.position.x, cf.position.y, cf.position.z)
    }

    #[test]
    fn rotating_about_an_axis_orbits_the_center() {
        // A quarter turn about +Y maps +X to -Z.
        let turned =
            rotate_cframe_about(cf_at(Vec3::new(2.0, 0.0, 0.0)), Vec3::ZERO, Vec3::Y, std::f32::consts::FRAC_PI_2);
        assert!(pos(&turned).abs_diff_eq(Vec3::new(0.0, 0.0, -2.0), 1e-5));

        // Around an off-origin center the same quarter turn orbits that point instead.
        let turned =
            rotate_cframe_about(cf_at(Vec3::new(2.0, 0.0, 0.0)), Vec3::X, Vec3::Y, std::f32::consts::FRAC_PI_2);
        assert!(pos(&turned).abs_diff_eq(Vec3::new(1.0, 0.0, -1.0), 1e-5));
    }

    #[test]
    fn rotating_turns_the_orientation_too() {
        let turned = rotate_cframe_about(cf_at(Vec3::ZERO), Vec3::ZERO, Vec3::Y, std::f32::consts::FRAC_PI_2);
        let x_axis = kahua_render::scene::cframe_to_mat4(&turned).x_axis.truncate();
        assert!(x_axis.abs_diff_eq(Vec3::new(0.0, 0.0, -1.0), 1e-5));
    }

    #[test]
    fn resizing_keeps_the_opposite_face_in_place() {
        // Pushing the +X face out by one stud on a 4 stud part keeps the -X face at -2.
        let (size, center) = resize_along(Vec3::ZERO, Vec3::X, 4.0, 1.0, false, None);
        assert!((size - 5.0).abs() < 1e-6);
        assert!(center.abs_diff_eq(Vec3::new(0.5, 0.0, 0.0), 1e-6));
        let minus_x_face = center - Vec3::X * size * 0.5;
        assert!((minus_x_face.x + 2.0).abs() < 1e-6);
    }

    #[test]
    fn symmetric_resizing_keeps_the_center() {
        let (size, center) = resize_along(Vec3::new(1.0, 2.0, 3.0), Vec3::X, 4.0, 1.0, true, None);
        assert!((size - 6.0).abs() < 1e-6);
        assert!(center.abs_diff_eq(Vec3::new(1.0, 2.0, 3.0), 1e-6));
    }

    #[test]
    fn resizing_snaps_and_clamps() {
        let (size, _) = resize_along(Vec3::ZERO, Vec3::X, 4.0, 0.3, false, Some(1.0));
        assert!((size - 4.0).abs() < 1e-6);
        let (size, center) = resize_along(Vec3::ZERO, Vec3::X, 1.0, -5.0, false, None);
        assert!((size - MIN_PART_SIZE).abs() < 1e-6);
        assert!((center.x - (MIN_PART_SIZE - 1.0) * 0.5).abs() < 1e-6);
    }
}

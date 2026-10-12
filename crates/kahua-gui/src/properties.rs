//! The Properties panel: every property of the selection, editable in place.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use eframe::egui::{self, Color32, DragValue, RichText, Ui};
use kahua_core::Document;
use kahua_core::kahua_io::rbx_dom_weak::types::{
    Color3, Color3uint8, ContentId, Enum, NumberRange, Ref, UDim, UDim2, Variant, VariantType, Vector2, Vector3,
};
use kahua_core::kahua_io::rbx_reflection::{DataType, PropertyKind, PropertySerialization, PropertyTag, Scriptability};
use kahua_core::kahua_io::reflect;
use kahua_render::glam::{EulerRot, Mat3, Quat, Vec3};
use kahua_render::scene::{cframe_from_pos_rot, cframe_to_mat4};

use crate::icons;
use crate::theme;

#[derive(Clone)]
struct PropInfo {
    name: String,
    data_type: PropType,
    editable: bool,
}

#[derive(Clone)]
enum PropType {
    Value(VariantType),
    Enum(String),
}

/// Shown first, in this order; everything else follows alphabetically.
const PINNED: &[&str] = &[
    "Anchored",
    "CanCollide",
    "CanTouch",
    "CanQuery",
    "Locked",
    "Shape",
    "Color",
    "BrickColor",
    "Material",
    "Transparency",
    "Reflectance",
    "CastShadow",
    "Enabled",
    "Disabled",
    "RunContext",
    "Value",
    "Text",
];

/// Properties shown in other ways (header, Position/Orientation, script tabs).
const HIDDEN: &[&str] =
    &["Name", "Parent", "Source", "CFrame", "Position", "Orientation", "Rotation", "UniqueId", "HistoryId"];

#[derive(Default)]
pub struct Properties {
    cache: HashMap<String, Vec<PropInfo>>,
    filter: String,
}

pub enum PropertiesAction {
    OpenScript(Ref),
}

fn props_for(class: &str) -> Vec<PropInfo> {
    let db = reflect::db();
    let Some(desc) = db.classes.get(class) else { return Vec::new() };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    // Parts are colored through Color; BrickColor is a legacy view of the same value.
    let is_part = reflect::is_base_part(class);
    for c in db.superclasses_iter(desc) {
        for (name, p) in &c.properties {
            if !seen.insert(*name) || HIDDEN.contains(name) || (is_part && *name == "BrickColor") {
                continue;
            }
            if p.tags.contains(&PropertyTag::Deprecated)
                || p.tags.contains(&PropertyTag::Hidden)
                || p.tags.contains(&PropertyTag::NotBrowsable)
            {
                continue;
            }
            let PropertyKind::Canonical { serialization } = &p.kind else { continue };
            if matches!(serialization, PropertySerialization::DoesNotSerialize) {
                continue;
            }
            let editable = match p.scriptability {
                Scriptability::None => continue,
                Scriptability::Read => false,
                _ => !p.tags.contains(&PropertyTag::ReadOnly),
            };
            let data_type = match &p.data_type {
                DataType::Enum(e) => PropType::Enum((*e).to_owned()),
                DataType::Value(t) => PropType::Value(*t),
                _ => continue,
            };
            out.push(PropInfo { name: (*name).to_owned(), data_type, editable });
        }
    }
    out.sort_by(|a, b| {
        let pa = PINNED.iter().position(|p| *p == a.name).unwrap_or(usize::MAX);
        let pb = PINNED.iter().position(|p| *p == b.name).unwrap_or(usize::MAX);
        pa.cmp(&pb).then_with(|| a.name.cmp(&b.name))
    });
    out
}

fn coalesce_key(targets: &[Ref], name: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    targets.hash(&mut h);
    name.hash(&mut h);
    h.finish()
}

impl Properties {
    pub fn ui(&mut self, ui: &mut Ui, doc: &mut Document) -> Option<PropertiesAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Properties").strong());
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter").desired_width(f32::INFINITY));
        });
        ui.add_space(2.0);

        let selection: Vec<Ref> = doc.selection.iter().copied().filter(|r| doc.dom.get_by_ref(*r).is_some()).collect();
        let Some(&first) = selection.first() else {
            ui.add_space(12.0);
            ui.label(RichText::new("Select an instance to see its properties.").color(theme::TEXT_WEAK));
            return None;
        };
        let inst = doc.dom.get_by_ref(first).unwrap();
        let class = inst.class.to_string();
        let inst_name = inst.name.clone();
        let props = self.cache.entry(class.clone()).or_insert_with(|| props_for(&class)).clone();

        // Header: badge, name and class.
        ui.horizontal(|ui| {
            icons::class_icon(ui, &class);
            if selection.len() > 1 {
                ui.label(RichText::new(format!("{} items", selection.len())).strong());
            } else {
                let mut name = inst_name.clone();
                let editable = !doc.is_service(first);
                let response = ui.add_enabled(editable, egui::TextEdit::singleline(&mut name).desired_width(170.0));
                if response.changed() && !name.is_empty() {
                    let edits = doc.rename(first, &name).into_iter().collect();
                    doc.perform_coalesced("Rename", edits, coalesce_key(&[first], "Name"));
                }
            }
            ui.label(RichText::new(&class).color(theme::TEXT_WEAK));
        });
        if reflect::is_script(&class) && ui.add(icons::button(icons::Icon::Pen, "Edit Script")).clicked() {
            action = Some(PropertiesAction::OpenScript(first));
        }
        ui.separator();

        let filter = self.filter.to_lowercase();
        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("props_scroll").show(ui, |ui| {
            egui::Grid::new("props_grid").num_columns(2).striped(true).spacing([10.0, 4.0]).min_col_width(110.0).show(
                ui,
                |ui| {
                    if reflect::is_base_part(&class) {
                        transform_rows(ui, doc, &selection, &filter);
                    }
                    for prop in &props {
                        if !filter.is_empty() && !prop.name.to_lowercase().contains(&filter) {
                            continue;
                        }
                        let inst = doc.dom.get_by_ref(first).unwrap();
                        let current = inst
                            .properties
                            .get(&prop.name.as_str().into())
                            .cloned()
                            .or_else(|| reflect::default_value(&class, &prop.name).cloned());
                        ui.label(&prop.name).on_hover_text(&prop.name);
                        let new = ui
                            .add_enabled_ui(prop.editable, |ui| {
                                value_widget(ui, &prop.name, &prop.data_type, current.as_ref(), doc)
                            })
                            .inner;
                        ui.end_row();
                        if let Some(value) = new {
                            let edits = selection
                                .iter()
                                .filter(|r| {
                                    doc.dom
                                        .get_by_ref(**r)
                                        .is_some_and(|i| reflect::property(&i.class, &prop.name).is_some())
                                })
                                .filter_map(|r| doc.set_property(*r, &prop.name, Some(value.clone())))
                                .collect();
                            doc.perform_coalesced(
                                format!("Set {}", prop.name),
                                edits,
                                coalesce_key(&selection, &prop.name),
                            );
                        }
                    }
                },
            );
        });
        action
    }
}

/// Position and Orientation rows, edited through CFrame like in Studio.
fn transform_rows(ui: &mut Ui, doc: &mut Document, selection: &[Ref], filter: &str) {
    let Some(Variant::CFrame(cf)) =
        doc.dom.get_by_ref(selection[0]).and_then(|i| i.properties.get(&"CFrame".into())).cloned()
    else {
        return;
    };
    let m = cframe_to_mat4(&cf);
    let (_, rot, pos) = m.to_scale_rotation_translation();
    let (ry, rx, rz) = rot.to_euler(EulerRot::YXZ);
    // Round away float noise (and -0) so 90° shows as 90, not 89.99998.
    let tidy = |v: f32| ((v * 1000.0).round() / 1000.0) + 0.0;
    let orientation = Vec3::new(tidy(rx.to_degrees()), tidy(ry.to_degrees()), tidy(rz.to_degrees()));
    let pos = Vec3::new(tidy(pos.x), tidy(pos.y), tidy(pos.z));

    for (label, value) in [("Position", pos), ("Orientation", orientation)] {
        if !filter.is_empty() && !label.to_lowercase().contains(filter) {
            continue;
        }
        ui.label(label);
        let mut v = value;
        let changed = ui
            .horizontal(|ui| {
                let speed = if label == "Position" { 0.1 } else { 1.0 };
                let mut c = false;
                for (axis, comp) in [("X", &mut v.x), ("Y", &mut v.y), ("Z", &mut v.z)] {
                    ui.label(RichText::new(axis).small().color(theme::TEXT_WEAK));
                    c |= ui.add(DragValue::new(comp).speed(speed).max_decimals(3)).changed();
                }
                c
            })
            .inner;
        ui.end_row();
        if !changed {
            continue;
        }
        // Apply only the components that changed, to every selected part.
        let edits: Vec<_> = selection
            .iter()
            .filter_map(|&r| {
                let Some(Variant::CFrame(cf)) = doc.dom.get_by_ref(r)?.properties.get(&"CFrame".into()).cloned() else {
                    return None;
                };
                let (_, rot, pos) = cframe_to_mat4(&cf).to_scale_rotation_translation();
                let new_cf = if label == "Position" {
                    let p = Vec3::new(
                        if v.x != value.x { v.x } else { pos.x },
                        if v.y != value.y { v.y } else { pos.y },
                        if v.z != value.z { v.z } else { pos.z },
                    );
                    cframe_from_pos_rot(p, rot)
                } else {
                    let (ry, rx, rz) = rot.to_euler(EulerRot::YXZ);
                    let o = Vec3::new(
                        if v.x != value.x { v.x.to_radians() } else { rx },
                        if v.y != value.y { v.y.to_radians() } else { ry },
                        if v.z != value.z { v.z.to_radians() } else { rz },
                    );
                    let q = Quat::from_euler(EulerRot::YXZ, o.y, o.x, o.z);
                    cframe_from_pos_rot(pos, Quat::from_mat3(&Mat3::from_quat(q)))
                };
                doc.set_property(r, "CFrame", Some(Variant::CFrame(new_cf)))
            })
            .collect();
        doc.perform_coalesced(format!("Set {label}"), edits, coalesce_key(selection, label));
    }
}

/// Draws an editor for one value. Returns the new value when it changed.
fn value_widget(ui: &mut Ui, name: &str, ty: &PropType, value: Option<&Variant>, doc: &Document) -> Option<Variant> {
    match (ty, value) {
        (PropType::Enum(enum_name), value) => {
            let current = match value {
                Some(Variant::Enum(e)) => e.to_u32(),
                _ => 0,
            };
            let descriptor = reflect::db().enums.get(enum_name.as_str())?;
            let mut items: Vec<(&str, u32)> = descriptor.items.iter().map(|(k, v)| (*k, *v)).collect();
            items.sort_by_key(|(_, v)| *v);
            let current_name = items.iter().find(|(_, v)| *v == current).map(|(k, _)| *k).unwrap_or("?");
            let mut selected = current;
            egui::ComboBox::from_id_salt(("enum", name))
                .selected_text(current_name)
                .width(160.0)
                .height(300.0)
                .show_ui(ui, |ui| {
                    for (item, v) in &items {
                        ui.selectable_value(&mut selected, *v, *item);
                    }
                });
            (selected != current).then(|| Variant::Enum(Enum::from_u32(selected)))
        }
        (_, Some(Variant::Bool(b))) => {
            let mut b = *b;
            ui.checkbox(&mut b, "").changed().then_some(Variant::Bool(b))
        }
        (_, Some(Variant::Float32(f))) => {
            let mut f = *f;
            let mut drag = DragValue::new(&mut f).speed(0.01).max_decimals(4);
            if matches!(name, "Transparency" | "Reflectance") {
                drag = drag.range(0.0..=1.0);
            }
            ui.add(drag).changed().then_some(Variant::Float32(f))
        }
        (_, Some(Variant::Float64(f))) => {
            let mut f = *f;
            ui.add(DragValue::new(&mut f).speed(0.01).max_decimals(4)).changed().then_some(Variant::Float64(f))
        }
        (_, Some(Variant::Int32(i))) => {
            let mut i = *i;
            ui.add(DragValue::new(&mut i)).changed().then_some(Variant::Int32(i))
        }
        (_, Some(Variant::Int64(i))) => {
            let mut i = *i;
            ui.add(DragValue::new(&mut i)).changed().then_some(Variant::Int64(i))
        }
        (_, Some(Variant::String(s))) => {
            let mut s = s.clone();
            ui.add(egui::TextEdit::singleline(&mut s).desired_width(170.0)).changed().then_some(Variant::String(s))
        }
        (_, Some(Variant::ContentId(c))) => {
            let mut s = c.as_str().to_owned();
            ui.add(egui::TextEdit::singleline(&mut s).desired_width(170.0).hint_text("rbxassetid://"))
                .changed()
                .then(|| Variant::ContentId(ContentId::from(s)))
        }
        (_, Some(Variant::Content(c))) => {
            let mut s = c.as_uri().unwrap_or_default().to_owned();
            ui.add(egui::TextEdit::singleline(&mut s).desired_width(170.0).hint_text("rbxassetid://"))
                .changed()
                .then(|| Variant::Content(kahua_core::kahua_io::rbx_types::Content::from_uri(s)))
        }
        (_, Some(Variant::Vector3(v))) => {
            let mut a = [v.x, v.y, v.z];
            drag_row(ui, &mut a, 0.1).then(|| Variant::Vector3(Vector3::new(a[0], a[1], a[2])))
        }
        (_, Some(Variant::Vector2(v))) => {
            let mut a = [v.x, v.y];
            drag_row(ui, &mut a, 0.1).then(|| Variant::Vector2(Vector2::new(a[0], a[1])))
        }
        (_, Some(Variant::Color3(c))) => {
            let mut rgb = [to_u8(c.r), to_u8(c.g), to_u8(c.b)];
            color_row(ui, &mut rgb).then(|| {
                Variant::Color3(Color3::new(rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0))
            })
        }
        (_, Some(Variant::Color3uint8(c))) => {
            let mut rgb = [c.r, c.g, c.b];
            color_row(ui, &mut rgb).then(|| Variant::Color3uint8(Color3uint8::new(rgb[0], rgb[1], rgb[2])))
        }
        (_, Some(Variant::UDim(u))) => {
            let (mut s, mut o) = (u.scale, u.offset);
            let changed = ui
                .horizontal(|ui| {
                    ui.add(DragValue::new(&mut s).speed(0.01)).changed() | ui.add(DragValue::new(&mut o)).changed()
                })
                .inner;
            changed.then(|| Variant::UDim(UDim::new(s, o)))
        }
        (_, Some(Variant::UDim2(u))) => {
            let (mut xs, mut xo, mut ys, mut yo) = (u.x.scale, u.x.offset, u.y.scale, u.y.offset);
            let changed = ui
                .vertical(|ui| {
                    let a = ui
                        .horizontal(|ui| {
                            ui.label(RichText::new("X").small().color(theme::TEXT_WEAK));
                            ui.add(DragValue::new(&mut xs).speed(0.01)).changed()
                                | ui.add(DragValue::new(&mut xo)).changed()
                        })
                        .inner;
                    let b = ui
                        .horizontal(|ui| {
                            ui.label(RichText::new("Y").small().color(theme::TEXT_WEAK));
                            ui.add(DragValue::new(&mut ys).speed(0.01)).changed()
                                | ui.add(DragValue::new(&mut yo)).changed()
                        })
                        .inner;
                    a | b
                })
                .inner;
            changed.then(|| Variant::UDim2(UDim2::new(UDim::new(xs, xo), UDim::new(ys, yo))))
        }
        (_, Some(Variant::NumberRange(r))) => {
            let mut a = [r.min, r.max];
            drag_row(ui, &mut a, 0.1).then(|| Variant::NumberRange(NumberRange::new(a[0], a[1])))
        }
        (_, Some(Variant::BrickColor(b))) => {
            let c = b.to_color3uint8();
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(c.r, c.g, c.b));
                ui.label(format!("{b}"));
            });
            None
        }
        (_, Some(Variant::Ref(r))) => {
            let text = if r.is_none() {
                "nil".to_owned()
            } else {
                doc.dom.get_by_ref(*r).map(|i| i.name.clone()).unwrap_or_else(|| "(missing)".into())
            };
            ui.label(RichText::new(text).color(theme::TEXT_WEAK));
            None
        }
        (_, Some(other)) => {
            let mut text = format!("{other:?}");
            if text.len() > 60 {
                text.truncate(57);
                text.push('…');
            }
            ui.label(RichText::new(text).small().color(theme::TEXT_WEAK));
            None
        }
        (PropType::Value(VariantType::Ref), None) => {
            ui.label(RichText::new("nil").color(theme::TEXT_WEAK));
            None
        }
        (PropType::Value(t), None) => {
            ui.label(RichText::new(format!("— ({t:?})")).color(theme::TEXT_WEAK));
            None
        }
    }
}

fn to_u8(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn drag_row<const N: usize>(ui: &mut Ui, values: &mut [f32; N], speed: f32) -> bool {
    ui.horizontal(|ui| {
        let mut changed = false;
        for v in values.iter_mut() {
            changed |= ui.add(DragValue::new(v).speed(speed).max_decimals(3)).changed();
        }
        changed
    })
    .inner
}

fn color_row(ui: &mut Ui, rgb: &mut [u8; 3]) -> bool {
    ui.horizontal(|ui| {
        let changed = ui.color_edit_button_srgb(rgb).changed();
        ui.label(RichText::new(format!("{}, {}, {}", rgb[0], rgb[1], rgb[2])).small().color(theme::TEXT_WEAK));
        changed
    })
    .inner
}

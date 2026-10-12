//! Helpers around the Roblox reflection database shipped with rbx-dom.

use rbx_reflection::{ClassDescriptor, DataType, PropertyDescriptor, ReflectionDatabase};
use rbx_types::Variant;

/// The reflection database bundled with `rbx_reflection_database`.
pub fn db() -> &'static ReflectionDatabase<'static> {
    rbx_reflection_database::get().expect("bundled reflection database must load")
}

pub fn class(name: &str) -> Option<&'static ClassDescriptor<'static>> {
    db().classes.get(name)
}

/// Returns true if `class_name` is `base` or inherits from it.
pub fn is_a(class_name: &str, base: &str) -> bool {
    let db = db();
    let Some(desc) = db.classes.get(class_name) else {
        return class_name == base;
    };
    db.superclasses_iter(desc).any(|c| c.name == base)
}

pub fn is_script(class_name: &str) -> bool {
    matches!(class_name, "Script" | "LocalScript" | "ModuleScript")
}

pub fn is_service(class_name: &str) -> bool {
    class(class_name).is_some_and(|c| c.tags.contains(&rbx_reflection::ClassTag::Service))
}

pub fn is_base_part(class_name: &str) -> bool {
    is_a(class_name, "BasePart")
}

/// Default value of a property for a class, walking up superclasses.
pub fn default_value(class_name: &str, prop: &str) -> Option<&'static Variant> {
    let db = db();
    let desc = db.classes.get(class_name)?;
    db.find_default_property(desc, prop)
}

/// Looks up the descriptor of a property, walking up superclasses.
pub fn property(class_name: &str, prop: &str) -> Option<&'static PropertyDescriptor<'static>> {
    let db = db();
    let desc = db.classes.get(class_name)?;
    db.superclasses_iter(desc).find_map(|c| c.properties.get(prop))
}

/// Name of the enum a property uses, if it is an enum property.
pub fn enum_name(class_name: &str, prop: &str) -> Option<&'static str> {
    match property(class_name, prop)?.data_type {
        DataType::Enum(name) => Some(name),
        _ => None,
    }
}

/// Properties that change on every save in Studio and only add noise to diffs.
pub const NOISY_PROPERTIES: &[&str] = &["UniqueId", "HistoryId"];

/// Whether a stored property can be dropped because it equals the class default
/// (rbx_binary and Rojo both fill missing properties with their defaults).
pub fn is_default(class_name: &str, prop: &str, value: &Variant) -> bool {
    if NOISY_PROPERTIES.contains(&prop) {
        return true;
    }
    match default_value(class_name, prop) {
        Some(default) => default == value,
        None => false,
    }
}

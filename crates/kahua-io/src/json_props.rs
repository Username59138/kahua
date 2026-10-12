//! Conversion between rbx-dom property values and Rojo-style JSON
//! (`.model.json`, `.meta.json`, `$properties` in project files).
//!
//! Kahua always writes the explicit form (`{"Vector3": [1, 2, 3]}`), which is
//! exactly what Rojo accepts. When reading, the implicit form
//! (`"Anchored": true`) is also understood for common types.

use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};
use rbx_dom_weak::{Instance, Ustr};
use rbx_reflection::DataType;
use rbx_types::{Content, Enum, Variant, VariantType};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::reflect;

/// Rojo `.model.json` instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonModel {
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "Name")]
    pub name: Option<String>,
    #[serde(rename = "className", alias = "ClassName")]
    pub class_name: String,
    #[serde(default, skip_serializing_if = "Map::is_empty", alias = "Properties")]
    pub properties: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty", alias = "Children")]
    pub children: Vec<JsonModel>,
}

/// Rojo `.meta.json` / `init.meta.json`. `name` is a Kahua extension used when
/// an instance name can't be represented as a file name (Rojo ignores it).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub properties: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore_unknown_instances: Option<bool>,
}

impl JsonMeta {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.class_name.is_none()
            && self.properties.is_empty()
            && self.attributes.is_none()
            && self.ignore_unknown_instances.is_none()
    }
}

/// Can this value be stored in JSON without losing information?
pub fn is_json_representable(value: &Variant) -> bool {
    match value {
        Variant::Ref(r) => r.is_none(),
        Variant::SharedString(_) | Variant::BinaryString(_) => false,
        other => match serde_json::to_value(other) {
            Ok(json) => !contains_null(&json),
            Err(_) => false,
        },
    }
}

fn contains_null(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.iter().any(contains_null),
        Value::Object(map) => map.values().any(contains_null),
        _ => false,
    }
}

/// Encodes a property in Rojo's explicit form, or `None` if it can't be represented.
pub fn encode_value(value: &Variant) -> Option<Value> {
    if !is_json_representable(value) {
        return None;
    }
    serde_json::to_value(value).ok()
}

/// Encodes the non-default properties of an instance, sorted by name.
/// Returns the JSON map and the names of properties that had to be dropped.
pub fn encode_properties(inst: &Instance, skip: &[&str]) -> (Map<String, Value>, Vec<String>) {
    let sorted: BTreeMap<&str, &Variant> = inst.properties.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let mut map = Map::new();
    let mut lost = Vec::new();
    for (key, value) in sorted {
        if skip.contains(&key) || reflect::is_default(&inst.class, key, value) {
            continue;
        }
        match encode_value(value) {
            Some(json) => {
                map.insert(key.to_owned(), json);
            }
            None => lost.push(key.to_owned()),
        }
    }
    (map, lost)
}

/// Decodes one property value from JSON (explicit or implicit form).
pub fn decode_value(class_name: &str, key: &str, value: &Value) -> Result<Variant> {
    if let Value::Object(map) = value
        && map.len() == 1
        && map.keys().next().is_some_and(|k| k.starts_with(|c: char| c.is_ascii_uppercase()))
        && let Ok(variant) = serde_json::from_value::<Variant>(value.clone())
    {
        return Ok(variant);
    }

    let descriptor = reflect::property(class_name, key).ok_or_else(|| {
        anyhow!("unknown property {class_name}.{key}; use the explicit form like {{\"String\": ...}}")
    })?;

    match &descriptor.data_type {
        DataType::Enum(enum_name) => decode_enum(enum_name, value),
        DataType::Value(ty) => decode_implicit(*ty, value),
        _ => bail!("unsupported property type for {class_name}.{key}"),
    }
}

fn decode_enum(enum_name: &str, value: &Value) -> Result<Variant> {
    match value {
        Value::Number(n) => {
            let n = n.as_u64().context("enum value must be a non-negative integer")?;
            Ok(Variant::Enum(Enum::from_u32(n as u32)))
        }
        Value::String(item) => {
            let descriptor = reflect::db().enums.get(enum_name).with_context(|| format!("unknown enum {enum_name}"))?;
            let item = item.rsplit('.').next().unwrap_or(item);
            let n =
                descriptor.items.get(item).with_context(|| format!("{item} is not a member of Enum.{enum_name}"))?;
            Ok(Variant::Enum(Enum::from_u32(*n)))
        }
        _ => bail!("enum value must be a number or item name"),
    }
}

fn decode_implicit(ty: VariantType, value: &Value) -> Result<Variant> {
    if ty == VariantType::Content
        && let Value::String(uri) = value
    {
        return Ok(Variant::Content(Content::from(uri.as_str())));
    }
    let tag = format!("{ty:?}");
    let mut wrapped = Map::new();
    wrapped.insert(tag.clone(), value.clone());
    serde_json::from_value::<Variant>(Value::Object(wrapped))
        .with_context(|| format!("value {value} is not a valid {tag}"))
}

/// Decodes a JSON property map, appending problems to `warnings`.
pub fn decode_properties(
    class_name: &str,
    props: &Map<String, Value>,
    attributes: Option<&Value>,
    context: &str,
    warnings: &mut Vec<String>,
) -> Vec<(Ustr, Variant)> {
    let mut out = Vec::with_capacity(props.len());
    for (key, value) in props {
        match decode_value(class_name, key, value) {
            Ok(v) => out.push((Ustr::from(key.as_str()), v)),
            Err(e) => warnings.push(format!("{context}: property {key}: {e:#}")),
        }
    }
    if let Some(attrs) = attributes {
        let mut wrapped = Map::new();
        wrapped.insert("Attributes".into(), attrs.clone());
        match serde_json::from_value::<Variant>(Value::Object(wrapped)) {
            Ok(v) => out.push((Ustr::from("Attributes"), v)),
            Err(e) => warnings.push(format!("{context}: attributes: {e}")),
        }
    }
    out
}

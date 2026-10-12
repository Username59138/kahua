//! Diff-friendly JSON formatting: objects one key per line, arrays of plain
//! values on a single line (`"Size": { "Vector3": [4.0, 1.0, 2.0] }`).

use serde_json::Value;

pub fn to_string<T: serde::Serialize>(value: &T) -> String {
    let value = serde_json::to_value(value).expect("JSON encoding cannot fail");
    let mut out = String::new();
    write(&value, 0, &mut out);
    out.push('\n');
    out
}

fn is_compact(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().all(|v| !v.is_object() && is_compact(v)),
        Value::Object(map) => map.is_empty(),
        _ => true,
    }
}

fn write(value: &Value, indent: usize, out: &mut String) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            out.push_str("{\n");
            for (i, (key, v)) in map.iter().enumerate() {
                push_indent(indent + 1, out);
                out.push_str(&serde_json::to_string(key).unwrap());
                out.push_str(": ");
                write(v, indent + 1, out);
                if i + 1 < map.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(indent, out);
            out.push('}');
        }
        Value::Array(items) if !items.is_empty() && !is_compact(value) => {
            out.push_str("[\n");
            for (i, v) in items.iter().enumerate() {
                push_indent(indent + 1, out);
                write(v, indent + 1, out);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(indent, out);
            out.push(']');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write(v, indent, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap()),
    }
}

fn push_indent(level: usize, out: &mut String) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn formats_compactly() {
        let v = json!({"a": {"Vector3": [1.0, 2.0, 3.0]}, "m": [[1, 0], [0, 1]], "c": [{"x": 1}], "e": {}});
        assert_eq!(
            super::to_string(&v),
            "{\n  \"a\": {\n    \"Vector3\": [1.0, 2.0, 3.0]\n  },\n  \"m\": [[1, 0], [0, 1]],\n  \"c\": [\n    {\n      \"x\": 1\n    }\n  ],\n  \"e\": {}\n}\n"
        );
    }
}

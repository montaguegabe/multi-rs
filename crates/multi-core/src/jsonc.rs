//! JSON / JSONC helpers: lenient reading of VS Code config files and writing
//! of generated files with provenance comments.

use std::path::Path;

use serde_json::{Map, Value};

use crate::error::Result;
use crate::fsx;

/// Key injected into merged list items to remember which file they came from.
/// It is rendered as a `// Imported from:` comment, never as JSON.
pub const SOURCE_KEY: &str = "_multi_source";

/// Strip `//` comments (outside strings) and trailing commas, then parse.
/// Returns an empty object when the text cannot be parsed, like the original
/// lenient reader.
pub fn soft_parse_jsonc_text(text: &str) -> Map<String, Value> {
    match parse_jsonc_text(text) {
        Ok(Value::Object(map)) => map,
        Ok(_) => Map::new(),
        Err(err) => {
            log::warn!("Could not parse JSONC content: {err}, skipping...");
            Map::new()
        }
    }
}

/// Parse JSONC text into any JSON value (used where arrays are expected too).
pub fn parse_jsonc_text(text: &str) -> serde_json::Result<Value> {
    let without_comments = strip_line_comments(text);
    let without_trailing_commas = strip_trailing_commas(&without_comments);
    serde_json::from_str(&without_trailing_commas)
}

fn strip_line_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let chars: Vec<char> = line.chars().collect();
        let mut in_string = false;
        let mut string_char = '"';
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if (c == '"' || c == '\'') && (i == 0 || chars[i - 1] != '\\') {
                if !in_string {
                    in_string = true;
                    string_char = c;
                } else if string_char == c {
                    in_string = false;
                }
            }
            if !in_string && c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
                // Keep the newline so line numbers in errors stay meaningful.
                if line.ends_with('\n') {
                    out.push('\n');
                }
                break;
            }
            out.push(c);
            i += 1;
        }
    }
    out
}

fn strip_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }
        if c == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Load a JSON/JSONC file if it exists, otherwise return an empty object.
pub fn soft_read_json_file(path: &Path) -> Map<String, Value> {
    match std::fs::read_to_string(path) {
        Ok(text) => soft_parse_jsonc_text(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(err) => {
            log::warn!("Could not parse {}: {err}, skipping...", path.display());
            Map::new()
        }
    }
}

/// Strict read of a JSON object file (used for `multi.json` round trips).
pub fn read_json_object(path: &Path) -> Result<Map<String, Value>> {
    let text = fsx::read_to_string(path)?;
    let value: Value = serde_json::from_str(&text)?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(crate::error::Error::Config(format!(
            "{} must contain a top-level object.",
            path.display()
        ))),
    }
}

/// Write pretty JSON with two-space indent and a trailing newline (the format
/// `multi init`/`add`/`remove` use for `multi.json`).
pub fn write_json_object_2(path: &Path, data: &Map<String, Value>) -> Result<()> {
    let mut text = serde_json::to_string_pretty(&Value::Object(data.clone()))?;
    text.push('\n');
    fsx::write(path, text)
}

/// Write a JSON file with four-space indentation and an optional `//` header.
pub fn write_json_file(path: &Path, data: &Value, header_comment: Option<&str>) -> Result<()> {
    let mut buf = Vec::new();
    if let Some(header) = header_comment {
        buf.extend_from_slice(format!("// {header}\n").as_bytes());
    }
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut buf, formatter);
    serde::Serialize::serialize(data, &mut serializer)?;
    fsx::write(path, buf)
}

fn serialize_value(value: &Value, indent_level: usize, out: &mut String) {
    const INDENT: &str = "    ";
    let current_indent = INDENT.repeat(indent_level);
    let next_indent = INDENT.repeat(indent_level + 1);
    match value {
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                let mut owned_item = None;
                if let Value::Object(map) = item {
                    if let Some(Value::String(source)) = map.get(SOURCE_KEY) {
                        out.push_str(&format!("{next_indent}// Imported from: {source}\n"));
                        let stripped: Map<String, Value> = map
                            .iter()
                            .filter(|(k, _)| k.as_str() != SOURCE_KEY)
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect();
                        owned_item = Some(Value::Object(stripped));
                    }
                }
                out.push_str(&next_indent);
                serialize_value(owned_item.as_ref().unwrap_or(item), indent_level + 1, out);
                if index + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&current_indent);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (index, (key, item)) in map.iter().enumerate() {
                out.push_str(&next_indent);
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push_str(": ");
                serialize_value(item, indent_level + 1, out);
                if index + 1 < map.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&current_indent);
            out.push('}');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

/// Render a JSONC document: four-space indent, `// Imported from:` comments
/// above list items tagged with [`SOURCE_KEY`], optional header line.
pub fn render_jsonc(data: &Value, header_comment: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(header) = header_comment {
        out.push_str(&format!("// {header}\n"));
    }
    serialize_value(data, 0, &mut out);
    out.push('\n');
    out
}

pub fn write_jsonc_file(path: &Path, data: &Value, header_comment: Option<&str>) -> Result<()> {
    fsx::write(path, render_jsonc(data, header_comment))
}

fn is_list_default_convention(value: &Value) -> bool {
    matches!(value, Value::Array(items) if items.len() == 1 && items[0].is_object())
}

/// Fill `target` with values from `defaults` for every missing key, recursing
/// into objects. Two conventions describe per-item defaults for lists:
/// `[{...}]` and `{"apply_to_list_items": {...}}`.
pub fn apply_defaults_to_structure(target: Option<&Value>, defaults: &Value) -> Value {
    if defaults.is_null() {
        return target.cloned().unwrap_or(Value::Null);
    }

    let item_defaults = if is_list_default_convention(defaults) {
        Some(&defaults[0])
    } else if let Value::Object(map) = defaults {
        if map.len() == 1 {
            map.get("apply_to_list_items")
        } else {
            None
        }
    } else {
        None
    };
    if let Some(item_defaults) = item_defaults {
        return match target {
            Some(Value::Array(items)) => Value::Array(
                items
                    .iter()
                    .map(|item| {
                        if item.is_object() {
                            apply_defaults_to_structure(Some(item), item_defaults)
                        } else {
                            item.clone()
                        }
                    })
                    .collect(),
            ),
            None | Some(Value::Null) => Value::Array(Vec::new()),
            Some(other) => other.clone(),
        };
    }

    if let Value::Object(default_map) = defaults {
        return match target {
            Some(Value::Object(target_map)) => {
                let mut result = target_map.clone();
                for (key, default_value) in default_map {
                    let next = apply_defaults_to_structure(target_map.get(key), default_value);
                    result.insert(key.clone(), next);
                }
                Value::Object(result)
            }
            None | Some(Value::Null) => {
                apply_defaults_to_structure(Some(&Value::Object(Map::new())), defaults)
            }
            Some(other) => other.clone(),
        };
    }

    match target {
        None | Some(Value::Null) => defaults.clone(),
        Some(other) => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_comments_and_trailing_commas() {
        let text = "{\n  // comment\n  \"a\": \"http://x\", // trailing\n  \"b\": [1, 2,],\n}\n";
        let parsed = soft_parse_jsonc_text(text);
        assert_eq!(parsed["a"], "http://x");
        assert_eq!(parsed["b"], json!([1, 2]));
    }

    #[test]
    fn renders_source_comments() {
        let data = json!({"tasks": [{"label": "x", SOURCE_KEY: "/a/tasks.json"}]});
        let rendered = render_jsonc(&data, Some("header"));
        assert!(rendered.starts_with("// header\n{\n"));
        assert!(rendered.contains("        // Imported from: /a/tasks.json\n        {\n"));
        assert!(!rendered.contains(SOURCE_KEY));
    }

    #[test]
    fn applies_defaults() {
        let defaults = json!({"monoRepo": false, "vscode": {"skipSettings": ["x"]}, "repos": []});
        let target = json!({"vscode": {}});
        let result = apply_defaults_to_structure(Some(&target), &defaults);
        assert_eq!(result["monoRepo"], false);
        assert_eq!(result["vscode"]["skipSettings"], json!(["x"]));
        assert_eq!(result["repos"], json!([]));

        let list_defaults = json!({"tasks": {"apply_to_list_items": {"options": {"cwd": "w"}}}});
        let target = json!({"tasks": [{"label": "a"}, {"label": "b", "options": {"cwd": "c"}}]});
        let result = apply_defaults_to_structure(Some(&target), &list_defaults);
        assert_eq!(result["tasks"][0]["options"]["cwd"], "w");
        assert_eq!(result["tasks"][1]["options"]["cwd"], "c");
    }
}

//! Compact YAML printer for `serde_json::Value`.
//!
//! Why not `serde_yaml`: it emits every map and list in block style, so a
//! node ten levels deep pays 20 spaces of indentation on every line and a
//! four-element padding list costs five lines. Measured on a 181-node
//! `node-info` view, half the bytes were leading whitespace. It also escapes
//! non-ASCII keys (`\u{1f4a0} Before Icon` for Figma's `💠 Before Icon`) and
//! prints integral floats as `12.0`.
//!
//! This printer keeps block style for the tree skeleton and switches to flow
//! style (`{k: v, ...}` / `[a, b]`) for *leaf containers*: anything nesting
//! at most two container levels whose flow rendering fits in
//! [`FLOW_MAX_WIDTH`] characters (list items are exempt from the width cap:
//! one record per row). Items directly under a `children` key stay block so
//! nodes always look alike. Output is standard YAML 1.2 that
//! round-trips through any YAML parser (the tests prove it with
//! `serde_yaml_ng`); it is not a new grammar.
//!
//! Scalars are quoted only when a plain scalar would be misread: empty,
//! leading/trailing whitespace, indicator characters, `: ` / ` #`, YAML 1.1
//! booleans/null, anything numeric-looking (including `1_000`, `0x1f`,
//! sexagesimal `1:30`, and ISO dates), and flow indicators inside a flow
//! context. Quoted strings use JSON escaping, which YAML double-quoted
//! scalars accept verbatim.

use serde_json::{Map, Value};
use std::fmt::Write;

/// Widest flow rendering of a map value (excluding the key) before falling
/// back to block. List items are exempt: a list of records reads best as one
/// row per line, however wide.
const FLOW_MAX_WIDTH: usize = 100;
const INDENT: usize = 2;

/// Render `value` as YAML. Always ends with a newline.
pub fn to_yaml(value: &Value) -> String {
    let mut out = String::new();
    match value {
        Value::Object(_) | Value::Array(_) if !is_empty_container(value) => {
            write_block(value, 0, &mut out, false);
        }
        _ => {
            out.push_str(&flow(value, false));
            out.push('\n');
        }
    }
    out
}

fn is_empty_container(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// Number of nested container levels: scalars 0, `{a: 1}` 1, `{a: [1]}` 2.
fn container_depth(v: &Value) -> usize {
    match v {
        Value::Object(m) => 1 + m.values().map(container_depth).max().unwrap_or(0),
        Value::Array(a) => 1 + a.iter().map(container_depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// Flow rendering of `v` if it qualifies as a leaf container (or scalar).
/// `max_width` is the flow-length cap; `None` means only the depth rule
/// applies.
fn try_flow(v: &Value, force_block: bool, max_width: Option<usize>) -> Option<String> {
    if force_block && !is_empty_container(v) {
        return None;
    }
    if container_depth(v) > 2 {
        return None;
    }
    let s = flow(v, false);
    let is_container = v.is_object() || v.is_array();
    match max_width {
        Some(w) if is_container && s.chars().count() > w => None,
        _ => Some(s),
    }
}

/// Unconditional flow rendering. `in_flow` widens the quoting rules for
/// scalars nested inside `{}`/`[]`.
fn flow(v: &Value, in_flow: bool) -> String {
    match v {
        Value::Object(m) => {
            let items: Vec<String> = m
                .iter()
                .map(|(k, x)| format!("{}: {}", scalar_str(k, true), flow(x, true)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
        Value::Array(a) => {
            let items: Vec<String> = a.iter().map(|x| flow(x, true)).collect();
            format!("[{}]", items.join(", "))
        }
        _ => scalar(v, in_flow),
    }
}

fn write_block(v: &Value, indent: usize, out: &mut String, force_block_items: bool) {
    let pad = " ".repeat(indent);
    match v {
        Value::Object(m) => write_map(m, &pad, indent, out),
        Value::Array(a) => {
            for item in a {
                // List items are records: one row per line reads like a
                // table, so the width cap does not apply to them.
                if let Some(f) = try_flow(item, force_block_items, None) {
                    let _ = writeln!(out, "{pad}- {f}");
                } else {
                    // Render the item as its own block one level deeper, then
                    // splice the `- ` marker over the first line's padding.
                    let mut sub = String::new();
                    write_block(item, indent + INDENT, &mut sub, false);
                    let first_end = sub.find('\n').unwrap_or(sub.len());
                    let _ = writeln!(out, "{pad}- {}", sub[..first_end].trim_start());
                    out.push_str(&sub[(first_end + 1).min(sub.len())..]);
                }
            }
        }
        _ => {
            let _ = writeln!(out, "{pad}{}", scalar(v, false));
        }
    }
}

fn write_map(m: &Map<String, Value>, pad: &str, indent: usize, out: &mut String) {
    for (k, x) in m {
        let key = scalar_str(k, false);
        let block_items = k == "children";
        match try_flow(x, false, Some(FLOW_MAX_WIDTH)) {
            Some(f) if !block_items || is_empty_container(x) => {
                let _ = writeln!(out, "{pad}{key}: {f}");
            }
            _ => {
                let _ = writeln!(out, "{pad}{key}:");
                write_block(x, indent + INDENT, out, block_items);
            }
        }
    }
}

fn scalar(v: &Value, in_flow: bool) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => number(n),
        Value::String(s) => scalar_str(s, in_flow),
        Value::Object(_) | Value::Array(_) => flow(v, in_flow),
    }
}

fn number(n: &serde_json::Number) -> String {
    if let Some(f) = n.as_f64() {
        if n.is_f64() {
            if f.is_nan() {
                return ".nan".into();
            }
            if f.is_infinite() {
                return if f > 0.0 { ".inf" } else { "-.inf" }.into();
            }
            if f.abs() >= 1e15 {
                return format!("{f:e}");
            }
            if f.fract() == 0.0 {
                return format!("{}", f as i64);
            }
            return format!("{f}");
        }
    }
    n.to_string()
}

fn scalar_str(s: &str, in_flow: bool) -> String {
    if plain_ok(s, in_flow) {
        s.to_owned()
    } else {
        // JSON string escaping is a subset of YAML double-quoted escaping.
        serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"))
    }
}

/// Whether `s` can be written as a plain (unquoted) scalar without a YAML
/// parser reading it back as something other than this exact string.
fn plain_ok(s: &str, in_flow: bool) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if s.trim() != s {
        return false;
    }
    if s.chars().any(|c| c.is_control()) {
        return false;
    }
    if ",[]{}#&*!|>'\"%@`".contains(first) {
        return false;
    }
    if "-?:".contains(first) {
        // `-foo` is a plain scalar, `- foo` / `-` are not.
        match chars.next() {
            None => return false,
            Some(c) if c.is_whitespace() => return false,
            Some(c) if in_flow && ",[]{}".contains(c) => return false,
            _ => {}
        }
    }
    if s.contains(": ") || s.ends_with(':') || s.contains(" #") {
        return false;
    }
    if in_flow && s.chars().any(|c| ",[]{}".contains(c)) {
        return false;
    }
    if is_reserved_word(s) || looks_numeric(s) || looks_like_date(s) {
        return false;
    }
    true
}

fn is_reserved_word(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "true" | "false" | "null" | "~" | "yes" | "no" | "on" | "off"
    )
}

/// Conservative: anything a YAML 1.1 or 1.2 parser might read as a number.
fn looks_numeric(s: &str) -> bool {
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    if body.is_empty() {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    if matches!(lower.as_str(), ".inf" | ".nan" | "inf" | "infinity" | "nan") {
        return true;
    }
    if lower.starts_with("0x") || lower.starts_with("0o") || lower.starts_with("0b") {
        return body.len() > 2;
    }
    // Digits, `_` separators, one `.`, optional exponent — YAML 1.1 ints/floats.
    let cleaned: String = body.chars().filter(|c| *c != '_').collect();
    if cleaned.parse::<f64>().is_ok() && cleaned.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    // Sexagesimal (`1:30`, `1:30:00`): YAML 1.1 reads it as 90. This also
    // catches short Figma ids like `1:17`; long ones (`1:17418`) stay plain.
    let parts: Vec<&str> = body.split(':').collect();
    parts.len() >= 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit()))
        && parts[0].chars().all(|c| c.is_ascii_digit())
}

/// `YYYY-MM-DD` prefix: YAML 1.1 timestamps.
fn looks_like_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Integral floats print as ints, so compare after normalising both sides.
    fn canon(v: &Value) -> Value {
        match v {
            Value::Number(n) if n.is_f64() => {
                let f = n.as_f64().unwrap();
                if f.fract() == 0.0 && f.abs() < 1e15 {
                    json!(f as i64)
                } else {
                    v.clone()
                }
            }
            Value::Object(m) => {
                Value::Object(m.iter().map(|(k, x)| (k.clone(), canon(x))).collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(canon).collect()),
            _ => v.clone(),
        }
    }

    fn roundtrip(v: &Value) {
        let text = to_yaml(v);
        let back: Value =
            serde_yaml_ng::from_str(&text).unwrap_or_else(|e| panic!("{e}\n---\n{text}"));
        assert_eq!(canon(&back), canon(v), "roundtrip mismatch for:\n{text}");
    }

    #[test]
    fn leaf_maps_and_short_lists_go_flow_tree_stays_block() {
        let v = json!({
            "id": "10:200", "type": "FRAME", "name": "Card",
            "bounds": {"x": 10.0, "y": 20.5, "width": 100.0, "height": 40.0},
            "layout": {"mode": "HORIZONTAL", "padding": [0.0, 12.0, 0.0, 12.0], "counter_axis": {"align": "CENTER"}},
            "children": [
                {"id": "10:300", "type": "TEXT", "name": "Label"}
            ]
        });
        let expected = "\
id: 10:200
type: FRAME
name: Card
bounds: {x: 10, y: 20.5, width: 100, height: 40}
layout: {mode: HORIZONTAL, padding: [0, 12, 0, 12], counter_axis: {align: CENTER}}
children:
  - id: 10:300
    type: TEXT
    name: Label
";
        assert_eq!(to_yaml(&v), expected);
        roundtrip(&v);
    }

    #[test]
    fn deep_or_wide_containers_fall_back_to_block() {
        let long_name = "x".repeat(60);
        let v = json!({
            "deep": {"a": {"b": {"c": 1}}},
            "wide_rows": [{"name": long_name, "other": long_name}, {"name": long_name}],
            "wide_map": {"name": long_name, "other": long_name}
        });
        let text = to_yaml(&v);
        assert!(text.starts_with("deep:\n  a: {b: {c: 1}}\n"), "{text}");
        assert!(
            text.contains("wide_rows:\n  - {name: xxx"),
            "list rows are never width-capped: {text}"
        );
        assert!(
            text.contains("wide_map:\n  name: xxx"),
            "wide map values go block: {text}"
        );
        roundtrip(&v);
    }

    #[test]
    fn unicode_keys_and_values_stay_unescaped() {
        let v = json!({"💠 Before Icon": {"instance": "687:73278"}, "✏️ Label": "Undo approval"});
        let text = to_yaml(&v);
        assert_eq!(
            text,
            "💠 Before Icon: {instance: 687:73278}\n✏️ Label: Undo approval\n"
        );
        roundtrip(&v);
    }

    #[test]
    fn strings_that_would_be_misread_are_quoted() {
        let tricky = [
            "",
            " lead",
            "trail ",
            "yes",
            "No",
            "null",
            "~",
            "true",
            "12",
            "1.5",
            "1e3",
            "0x1f",
            "1_000",
            "1:30",
            "-",
            "- x",
            "-1",
            "+5",
            ".5",
            "5.",
            ".inf",
            "2026-09-05",
            "a: b",
            "a:",
            "a #b",
            "#fff",
            "[x]",
            "{y}",
            "a, b",
            "@at",
            "%pct",
            "`bq",
            "'q'",
            "\"dq\"",
            "!tag",
            "&anc",
            "*ali",
            "|",
            ">",
            "?",
            "? q",
            "line\nbreak",
            "tab\there",
            "-foo",
            "1:17418",
            "I1:17;2:3",
            "🙂",
            "über",
            "a:b",
            "x/y",
            "HUG/FIXED",
        ];
        let mut m = Map::new();
        for (i, s) in tricky.iter().enumerate() {
            m.insert(format!("k{i}"), json!(s));
            m.insert((*s).to_owned(), json!(i));
        }
        let mut flow_map = Map::new();
        for (i, s) in tricky.iter().enumerate() {
            flow_map.insert(format!("f{i}"), json!(s));
        }
        m.insert("flow".into(), json!({"inner": Value::Object(flow_map)}));
        m.insert(
            "list".into(),
            Value::Array(tricky.iter().map(|s| json!(s)).collect()),
        );
        let v = Value::Object(m);
        roundtrip(&v);
        let text = to_yaml(&v);
        // Spot-check that the common Figma shapes stay unquoted.
        assert!(text.contains(": 1:17418\n"), "{text}");
        assert!(text.contains(": HUG/FIXED\n"), "{text}");
        assert!(text.contains(": \"1:30\"\n"), "{text}");
        assert!(text.contains("\n1:17418: "), "{text}");
    }

    #[test]
    fn numbers_and_specials() {
        let v = json!({
            "int": 42, "neg": -3, "float": 0.25, "integral": 12.0, "big": 1.5e20,
            "nan": Value::Null, "empty_map": {}, "empty_list": [], "nested_empty": {"a": {}},
            "bools": [true, false], "nulls": [null]
        });
        let text = to_yaml(&v);
        assert!(text.contains("integral: 12\n"), "{text}");
        assert!(text.contains("empty_map: {}\n"), "{text}");
        assert!(text.contains("nested_empty: {a: {}}\n"), "{text}");
        roundtrip(&v);
    }

    #[test]
    fn nested_lists_of_blocks() {
        let v = json!({
            "matrix": [[1.0, 0.0, 5.0], [0.0, 1.0, 7.0]],
            "items": [
                {"id": "a", "children": [{"id": "b", "text": {"characters": "hi", "style": {"font_size": 12, "f": {"g": 1}}}}]},
                [{"x": 1}, {"y": {"z": {"w": 2}}}]
            ]
        });
        roundtrip(&v);
        let text = to_yaml(&v);
        assert!(text.contains("matrix: [[1, 0, 5], [0, 1, 7]]\n"), "{text}");
    }

    #[test]
    fn scalar_and_empty_top_levels() {
        assert_eq!(to_yaml(&json!("hi")), "hi\n");
        assert_eq!(to_yaml(&json!({})), "{}\n");
        assert_eq!(to_yaml(&json!([])), "[]\n");
        assert_eq!(to_yaml(&json!(null)), "null\n");
    }
}

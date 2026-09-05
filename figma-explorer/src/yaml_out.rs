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
//! [`FLOW_MAX_WIDTH`] characters ([`LIST_ITEM_FLOW_MAX_WIDTH`] for list
//! items, so a list of records reads as one row per line). A list renders
//! uniformly: if any item needs block style, every item does, so sibling
//! nodes always look alike. Multi-line strings render as `|-` literal
//! blocks (one source line per line) in block context; a container holding
//! one is never rendered flow. Output is standard YAML that round-trips
//! through YAML 1.2 *and* YAML 1.1 parsers (the tests prove the former with
//! `serde_yaml_ng`; the quoting rules below are written for the latter —
//! PyYAML, Psych — whose implicit typing is far more eager); it is not a
//! new grammar.
//!
//! Scalars are quoted only when a plain scalar would be misread: empty,
//! leading/trailing whitespace, control / line-separator characters,
//! indicator characters, `: ` / ` #`, YAML 1.1 booleans/null, anything
//! numeric-looking (including `1_000`, `1,000`, `0x1f`, sexagesimal `1:30`
//! and `687:45`, and ISO dates), and flow indicators or a leading `:`/`?`
//! inside a flow context. Quoted strings use double-quote escaping with
//! every character a libyaml-based parser rejects (C0/C1 controls,
//! U+2028/U+2029, BOM, non-characters) written as `\uXXXX`.

use serde_json::{Map, Value};
use std::fmt::Write;

/// Widest flow rendering of a map value (excluding the key) before falling
/// back to block.
const FLOW_MAX_WIDTH: usize = 100;
/// Widest flow rendering of a list item. Wider than [`FLOW_MAX_WIDTH`]
/// because a list of records reads best as one row per line, but capped so
/// a comment thread doesn't collapse into a single 400-character row next
/// to block-rendered siblings.
const LIST_ITEM_FLOW_MAX_WIDTH: usize = 200;
const INDENT: usize = 2;

/// Render `value` as YAML. Always ends with a newline.
pub fn to_yaml(value: &Value) -> String {
    let mut out = String::new();
    match value {
        Value::Object(_) | Value::Array(_) if !is_empty_container(value) => {
            write_block(value, 0, &mut out);
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

/// Does `v` (or anything inside it) want a `|-` literal block? Such values
/// only exist in block context, so their containers can't be flow.
fn contains_literal(v: &Value) -> bool {
    match v {
        Value::String(s) => literal_ok(s),
        Value::Object(m) => m.values().any(contains_literal),
        Value::Array(a) => a.iter().any(contains_literal),
        _ => false,
    }
}

/// Flow rendering of `v` if it qualifies as a leaf container (or scalar).
/// `max_width` is the flow-length cap for containers.
fn try_flow(v: &Value, max_width: usize) -> Option<String> {
    if container_depth(v) > 2 || contains_literal(v) {
        return None;
    }
    let s = flow(v, false);
    let is_container = v.is_object() || v.is_array();
    if is_container && s.chars().count() > max_width {
        return None;
    }
    Some(s)
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

fn write_block(v: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match v {
        Value::Object(m) => write_map(m, &pad, indent, out),
        Value::Array(a) => {
            // Uniform per list: one deep item makes every item block, so a
            // list of node records never mixes `- {…}` rows with `- id:` blocks.
            let flows: Vec<Option<String>> = a
                .iter()
                .map(|item| try_flow(item, LIST_ITEM_FLOW_MAX_WIDTH))
                .collect();
            if flows.iter().all(Option::is_some) {
                for f in flows.into_iter().flatten() {
                    let _ = writeln!(out, "{pad}- {f}");
                }
                return;
            }
            for item in a {
                if let Value::String(s) = item {
                    if literal_ok(s) {
                        let _ = writeln!(out, "{pad}- |-");
                        write_literal_lines(s, indent + INDENT, out);
                        continue;
                    }
                }
                // Render the item as its own block one level deeper, then
                // splice the `- ` marker over the first line's padding.
                let mut sub = String::new();
                write_block(item, indent + INDENT, &mut sub);
                let first_end = sub.find('\n').unwrap_or(sub.len());
                let _ = writeln!(out, "{pad}- {}", sub[..first_end].trim_start());
                out.push_str(&sub[(first_end + 1).min(sub.len())..]);
            }
        }
        Value::String(s) if literal_ok(s) => {
            let _ = writeln!(out, "{pad}|-");
            write_literal_lines(s, indent + INDENT, out);
        }
        _ => {
            let _ = writeln!(out, "{pad}{}", scalar(v, false));
        }
    }
}

fn write_map(m: &Map<String, Value>, pad: &str, indent: usize, out: &mut String) {
    for (k, x) in m {
        let key = scalar_str(k, false);
        match x {
            Value::String(s) if literal_ok(s) => {
                let _ = writeln!(out, "{pad}{key}: |-");
                write_literal_lines(s, indent + INDENT, out);
            }
            _ => match try_flow(x, FLOW_MAX_WIDTH) {
                Some(f) => {
                    let _ = writeln!(out, "{pad}{key}: {f}");
                }
                None => {
                    let _ = writeln!(out, "{pad}{key}:");
                    write_block(x, indent + INDENT, out);
                }
            },
        }
    }
}

/// Body of a `|-` literal block: each source line on its own line at
/// `indent`; empty lines stay empty (no trailing padding).
fn write_literal_lines(s: &str, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    for line in s.split('\n') {
        if line.is_empty() {
            out.push('\n');
        } else {
            let _ = writeln!(out, "{pad}{line}");
        }
    }
}

/// Can `s` be written as a `|-` literal block and read back verbatim?
/// Conservative: needs an interior newline, no leading/trailing newline
/// (chomping would have to vary), no other character that needs escaping,
/// and no leading whitespace on the first line (it would be taken as the
/// block's indentation) or trailing whitespace on any line.
fn literal_ok(s: &str) -> bool {
    if !s.contains('\n') || s.starts_with('\n') || s.ends_with('\n') {
        return false;
    }
    if s.chars().any(|c| c != '\n' && needs_escape(c)) {
        return false;
    }
    let mut lines = s.split('\n');
    let first = lines.next().unwrap_or("");
    if first.starts_with(char::is_whitespace) {
        return false;
    }
    std::iter::once(first)
        .chain(lines)
        .all(|l| !l.ends_with(char::is_whitespace))
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
                return exponent_form(f);
            }
            if f.fract() == 0.0 {
                return format!("{}", f as i64);
            }
            return format!("{f}");
        }
    }
    n.to_string()
}

/// `1.5e+20`, not Rust's `1.5e20`: YAML 1.1's float regex needs a dot in the
/// mantissa and a signed exponent, otherwise the value reads back as a
/// string. YAML 1.2 accepts both forms.
fn exponent_form(f: f64) -> String {
    let s = format!("{f:e}");
    let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let mantissa = if mantissa.contains('.') {
        mantissa.to_owned()
    } else {
        format!("{mantissa}.0")
    };
    let exp = if exp.starts_with('-') {
        exp.to_owned()
    } else {
        format!("+{exp}")
    };
    format!("{mantissa}e{exp}")
}

fn scalar_str(s: &str, in_flow: bool) -> String {
    if plain_ok(s, in_flow) {
        s.to_owned()
    } else {
        quote(s)
    }
}

/// Characters that can't appear raw in a plain *or* double-quoted scalar:
/// C0/C1 controls (incl. DEL and NEL), the Unicode line/paragraph
/// separators, the BOM, and the two non-characters. libyaml rejects the
/// document outright on any of them; JSON's escaper leaves most through.
fn needs_escape(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{2028}' | '\u{2029}' | '\u{FEFF}' | '\u{FFFE}' | '\u{FFFF}'
        )
}

/// Double-quoted YAML scalar. Escapes are the JSON subset plus `\uXXXX` for
/// everything in [`needs_escape`], which YAML double-quoted scalars accept.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if needs_escape(c) => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
    if s.chars().any(needs_escape) {
        return false;
    }
    if ",[]{}#&*!|>'\"%@`".contains(first) {
        return false;
    }
    if "-?:".contains(first) {
        // `-foo` is a plain scalar, `- foo` / `-` are not. Inside `{}`/`[]`
        // libyaml reads a leading `?`/`:` as the key/value indicator.
        if in_flow && first != '-' {
            return false;
        }
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
    // Digits with `_` (YAML 1.1) or `,` (Psych) separators, one `.`,
    // optional exponent.
    let cleaned: String = body.chars().filter(|c| !matches!(c, '_' | ',')).collect();
    if cleaned.parse::<f64>().is_ok() && cleaned.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    // Sexagesimal (`1:30`, `1:30:00`, `1:30.5`): YAML 1.1 reads it as base
    // 60 — `[1-9][0-9_]*(:[0-5]?[0-9])+`. The first part has no length cap
    // (`687:45` is 41,265), later parts are at most two digits, so Figma
    // ids like `1:17418` stay plain and `10:5` gets quoted.
    let (whole, frac) = match body.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (body, None),
    };
    if frac.is_some_and(|f| !f.chars().all(|c| c.is_ascii_digit())) {
        return false;
    }
    let parts: Vec<&str> = whole.split(':').collect();
    let digits = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit());
    parts.len() >= 2 && digits(parts[0]) && parts[1..].iter().all(|p| digits(p) && p.len() <= 2)
}

/// `YYYY-M-D` / `YYYY-MM-DD` prefix (one- or two-digit month and day, as
/// YAML 1.1's timestamp regex allows).
fn looks_like_date(s: &str) -> bool {
    let b = s.as_bytes();
    let take_digits = |i: usize, max: usize| -> Option<usize> {
        let n = b[i..]
            .iter()
            .take(max)
            .take_while(|c| c.is_ascii_digit())
            .count();
        (n > 0).then_some(i + n)
    };
    if b.len() < 8 || !b[..4].iter().all(u8::is_ascii_digit) || b[4] != b'-' {
        return false;
    }
    let Some(i) = take_digits(5, 2) else {
        return false;
    };
    if b.get(i) != Some(&b'-') {
        return false;
    }
    take_digits(i + 1, 2).is_some()
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
                {"id": "10:300", "type": "TEXT", "name": "Label",
                 "text": {"characters": "Hi", "style": {"font_size": 12}}},
                {"id": "10:301", "type": "RECTANGLE", "name": "Bg"}
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
    text: {characters: Hi, style: {font_size: 12}}
  - id: 10:301
    type: RECTANGLE
    name: Bg
";
        assert_eq!(to_yaml(&v), expected);
        roundtrip(&v);
    }

    #[test]
    fn lists_render_uniformly_flow_rows_or_all_block() {
        let v = json!({
            "hidden_children": [
                {"id": "1:200", "name": "A rather long layer name that pushes the row past", "type": "TEXT"},
                {"id": "1:300", "name": "B", "type": "FRAME"}
            ],
            "mixed": [{"a": 1}, {"deep": {"b": {"c": 2}}}]
        });
        let text = to_yaml(&v);
        assert!(
            text.contains("hidden_children:\n  - {id: 1:200, name: A rather long"),
            "a 100+ char row still fits the list-item cap: {text}"
        );
        assert!(
            text.contains("mixed:\n  - a: 1\n  - deep: {b: {c: 2}}\n"),
            "one deep item makes the whole list block: {text}"
        );
        roundtrip(&v);
    }

    #[test]
    fn deep_or_wide_containers_fall_back_to_block() {
        let long_name = "x".repeat(60);
        let v = json!({
            "deep": {"a": {"b": {"c": 1}}},
            "wide_rows": [{"name": long_name, "other": long_name}, {"name": long_name}],
            "wider_rows": [{"name": long_name, "other": long_name, "third": long_name, "fourth": long_name}],
            "wide_map": {"name": long_name, "other": long_name}
        });
        let text = to_yaml(&v);
        assert!(text.starts_with("deep:\n  a: {b: {c: 1}}\n"), "{text}");
        assert!(
            text.contains("wide_rows:\n  - {name: xxx"),
            "list rows get a wider cap than map values: {text}"
        );
        assert!(
            text.contains("wider_rows:\n  - name: xxx"),
            "but a 250-char row still goes block: {text}"
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
            "1,000",
            "1:30",
            "687:45",
            "10:5",
            "1:30.5",
            "-",
            "- x",
            "-1",
            "+5",
            ".5",
            "5.",
            ".inf",
            "2026-09-05",
            "2026-9-5",
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
            "?Help",
            ":)",
            ":hover",
            "line\nbreak",
            "tab\there",
            "soft\u{2028}break",
            "para\u{2029}sep",
            "nel\u{85}here",
            "del\u{7f}",
            "c1\u{92}quote",
            "\u{feff}bom",
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
        // A multi-line string forces block rendering, so keep those out of
        // the map that must stay flow.
        let mut flow_map = Map::new();
        for (i, s) in tricky.iter().enumerate().filter(|(_, s)| !s.contains('\n')) {
            flow_map.insert(format!("f{i}"), json!(s));
        }
        m.insert("flow".into(), json!({"inner": Value::Object(flow_map)}));
        m.insert(
            "flow_small".into(),
            json!({"a": ":hover", "b": "?Help", "c": ":)", "d": "-foo"}),
        );
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
        assert!(text.contains(": \"687:45\"\n"), "{text}");
        assert!(text.contains("\n1:17418: "), "{text}");
        // Block context keeps a leading `:` plain; flow context quotes it.
        assert!(text.contains(": :hover\n"), "{text}");
        assert!(
            text.contains("flow_small: {a: \":hover\", b: \"?Help\", c: \":)\", d: -foo}\n"),
            "{text}"
        );
        // Every character libyaml rejects is escaped, never raw.
        assert!(text.contains("\"soft\\u2028break\""), "{text}");
        assert!(text.contains("\"nel\\u0085here\""), "{text}");
        assert!(text.contains("\"del\\u007F\""), "{text}");
        assert!(text.contains("\"c1\\u0092quote\""), "{text}");
        for c in [
            '\u{2028}', '\u{2029}', '\u{85}', '\u{7f}', '\u{92}', '\u{feff}',
        ] {
            assert!(!text.contains(c), "raw {:?} in output", c);
        }
    }

    #[test]
    fn numbers_and_specials() {
        let v = json!({
            "int": 42, "neg": -3, "float": 0.25, "integral": 12.0, "big": 1.5e20, "exact": 1e15,
            "neg_big": -2.5e16, "tiny": 1e-7,
            "nan": Value::Null, "empty_map": {}, "empty_list": [], "nested_empty": {"a": {}},
            "bools": [true, false], "nulls": [null]
        });
        let text = to_yaml(&v);
        assert!(text.contains("integral: 12\n"), "{text}");
        assert!(text.contains("empty_map: {}\n"), "{text}");
        assert!(text.contains("nested_empty: {a: {}}\n"), "{text}");
        // YAML 1.1 needs a dotted mantissa and a signed exponent.
        assert!(text.contains("big: 1.5e+20\n"), "{text}");
        assert!(text.contains("exact: 1.0e+15\n"), "{text}");
        assert!(text.contains("neg_big: -2.5e+16\n"), "{text}");
        roundtrip(&v);
    }

    #[test]
    fn multi_line_strings_render_as_literal_blocks() {
        let v = json!({
            "text": {"characters": "First line\nSecond line\n\nFourth", "style": {"font_size": 12}},
            "items": ["one\ntwo", "plain"],
            "quoted_anyway": " leading space\nx",
            "trailing_newline": "a\nb\n",
            "with_tab": "a\tb\nc"
        });
        let text = to_yaml(&v);
        let expected_text = "\
text:
  characters: |-
    First line
    Second line

    Fourth
  style: {font_size: 12}
items:
  - |-
    one
    two
  - plain
";
        assert!(text.starts_with(expected_text), "{text}");
        assert!(
            text.contains("quoted_anyway: \" leading space\\nx\"\n"),
            "{text}"
        );
        assert!(text.contains("trailing_newline: \"a\\nb\\n\"\n"), "{text}");
        assert!(text.contains("with_tab: \"a\\tb\\nc\"\n"), "{text}");
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

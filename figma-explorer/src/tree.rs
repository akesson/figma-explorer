//! Compact ASCII tree renderer for Figma nodes.
//!
//! Output is purely text — one node per line, with box-drawing characters
//! for the structure and a short payload (type, name, dimensions, primary
//! color when applicable). Invisible nodes are skipped.
//!
//! Two parallel rendering APIs live here:
//! - `&serde_json::Value` paths (`render`, `render_compact`, `render_structured`,
//!   `format_node_line`) for live-data consumers like `context`, which need
//!   access to fills/strokes for the color suffix.
//! - `_cache` suffixed paths over `&CacheNode` for the cached structural
//!   commands (`tree`, `pages`, `frames`, `search`). These drop the color
//!   suffix because the projection doesn't carry fills.

use serde_json::{json, Map, Value};
use std::fmt::Write;

use crate::cache::CacheNode;
use crate::node::{bounds, children, id, is_visible, name, primary_solid_hex, type_str};

/// Render `root` as a tree, traversing at most `max_depth` levels of children
/// (0 = just the node itself).
pub fn render(root: &Value, max_depth: usize) -> String {
    let mut out = String::new();
    if !is_visible(root) {
        return out;
    }
    let _ = writeln!(out, "{}", format_node_line(root));
    let kids: Vec<&Value> = children(root).iter().filter(|n| is_visible(n)).collect();
    render_children(&kids, &mut out, "", max_depth, 1);
    out
}

fn render_children(
    siblings: &[&Value],
    out: &mut String,
    prefix: &str,
    max_depth: usize,
    depth: usize,
) {
    if depth > max_depth {
        // We're already at the depth limit; show an ellipsis if there's more
        // to descend so the truncation is visible.
        if !siblings.is_empty() {
            let _ = writeln!(out, "{}└─ … ({} more)", prefix, siblings.len());
        }
        return;
    }
    let last = siblings.len().saturating_sub(1);
    for (i, node) in siblings.iter().enumerate() {
        let is_last = i == last;
        let branch = if is_last { "└─ " } else { "├─ " };
        let _ = writeln!(out, "{}{}{}", prefix, branch, format_node_line(node));
        let next_prefix = format!("{}{}", prefix, if is_last { "   " } else { "│  " });
        let kids: Vec<&Value> = children(node).iter().filter(|n| is_visible(n)).collect();
        render_children(&kids, out, &next_prefix, max_depth, depth + 1);
    }
}

/// Render `root` as a compact YAML-friendly tree: each node is its
/// `format_node_line` string. Leaves stay as scalar strings; parents become
/// single-key maps `{ "<line>": [children] }` so hierarchy round-trips
/// through YAML nesting. Depth truncation appends ` [+N children]` to the
/// parent's line and leaves it a scalar. Invisible nodes are dropped.
pub fn render_compact(root: &Value, max_depth: usize) -> Value {
    fn build(node: &Value, max_depth: usize, depth: usize) -> Value {
        let line = format_node_line(node);
        let kids: Vec<&Value> = children(node).iter().filter(|n| is_visible(n)).collect();
        if kids.is_empty() {
            return Value::String(line);
        }
        if depth >= max_depth {
            return Value::String(format!("{} [+{} children]", line, kids.len()));
        }
        let rendered: Vec<Value> = kids
            .iter()
            .map(|c| build(c, max_depth, depth + 1))
            .collect();
        let mut obj = Map::new();
        obj.insert(line, Value::Array(rendered));
        Value::Object(obj)
    }
    if !is_visible(root) {
        return Value::Null;
    }
    build(root, max_depth, 0)
}

/// Render `root` as a nested JSON tree, one object per visible node, with
/// child arrays. Truncates at `max_depth` with a `truncated` marker so
/// downstream consumers can detect cut-offs. Invisible nodes are dropped.
pub fn render_structured(root: &Value, max_depth: usize) -> Value {
    fn build(node: &Value, max_depth: usize, depth: usize) -> Value {
        let mut obj = Map::new();
        if let Some(nid) = id(node) {
            obj.insert("id".into(), json!(nid));
        }
        if let Some(t) = type_str(node) {
            obj.insert("type".into(), json!(t));
        }
        if let Some(nm) = name(node) {
            obj.insert("name".into(), json!(nm));
        }
        if let Some(b) = bounds(node) {
            obj.insert("bounds".into(), json!(b.to_string()));
        }
        if let Some(hex) = primary_solid_hex(node) {
            obj.insert("fill".into(), json!(hex));
        }
        let kids: Vec<&Value> = children(node).iter().filter(|n| is_visible(n)).collect();
        if !kids.is_empty() {
            if depth >= max_depth {
                obj.insert("truncated".into(), json!({ "children": kids.len() }));
            } else {
                let rendered: Vec<Value> = kids
                    .iter()
                    .map(|c| build(c, max_depth, depth + 1))
                    .collect();
                obj.insert("children".into(), Value::Array(rendered));
            }
        }
        Value::Object(obj)
    }
    if !is_visible(root) {
        return Value::Null;
    }
    build(root, max_depth, 0)
}

/// Single-line summary of a node: `TYPE "name" (bounds) #hex id:nid`.
///
/// Bounds and color are emitted only when present. Shared with the `pages`
/// and `frames` compact renderings so node-listing commands look consistent.
pub fn format_node_line(node: &Value) -> String {
    let kind = type_str(node).unwrap_or("?");
    let nm = name(node).unwrap_or("");
    let mut s = format!("{} \"{}\"", kind, nm);
    if let Some(b) = bounds(node) {
        let _ = write!(s, " ({})", b);
    }
    if let Some(hex) = primary_solid_hex(node) {
        let _ = write!(s, " {}", hex);
    }
    if let Some(nid) = id(node) {
        let _ = write!(s, " id:{}", nid);
    }
    s
}

// ─────────────────────────────────────────────────────────────────────────────
// CacheNode-typed renderers (cache consumers)
// ─────────────────────────────────────────────────────────────────────────────

/// Render a `CacheNode` as a compact YAML-friendly tree. Mirrors
/// `render_compact` over `&Value` but operates on the typed projection;
/// drops the color suffix because the cache doesn't carry fills.
pub fn render_compact_cache(root: &CacheNode, max_depth: usize) -> Value {
    fn build(node: &CacheNode, max_depth: usize, depth: usize) -> Value {
        let line = format_cache_node_line(node);
        let kids: Vec<&CacheNode> = node.children.iter().filter(|n| n.visible).collect();
        if kids.is_empty() {
            return Value::String(line);
        }
        if depth >= max_depth {
            return Value::String(format!("{} [+{} children]", line, kids.len()));
        }
        let rendered: Vec<Value> = kids
            .iter()
            .map(|c| build(c, max_depth, depth + 1))
            .collect();
        let mut obj = Map::new();
        obj.insert(line, Value::Array(rendered));
        Value::Object(obj)
    }
    if !root.visible {
        return Value::Null;
    }
    build(root, max_depth, 0)
}

/// Render a `CacheNode` as a nested JSON tree (one object per visible node,
/// child arrays). Truncates at `max_depth` with a `truncated` marker.
pub fn render_structured_cache(root: &CacheNode, max_depth: usize) -> Value {
    fn build(node: &CacheNode, max_depth: usize, depth: usize) -> Value {
        let mut obj = Map::new();
        obj.insert("id".into(), json!(node.id));
        obj.insert("type".into(), json!(node.type_));
        obj.insert("name".into(), json!(node.name));
        if let Some(b) = node.bounds {
            obj.insert("bounds".into(), json!(b.to_string()));
        }
        let kids: Vec<&CacheNode> = node.children.iter().filter(|n| n.visible).collect();
        if !kids.is_empty() {
            if depth >= max_depth {
                obj.insert("truncated".into(), json!({ "children": kids.len() }));
            } else {
                let rendered: Vec<Value> = kids
                    .iter()
                    .map(|c| build(c, max_depth, depth + 1))
                    .collect();
                obj.insert("children".into(), Value::Array(rendered));
            }
        }
        Value::Object(obj)
    }
    if !root.visible {
        return Value::Null;
    }
    build(root, max_depth, 0)
}

/// Single-line summary of a `CacheNode`: `TYPE "name" (bounds) id:nid`.
/// No color suffix — fills aren't in the cache projection.
pub fn format_cache_node_line(node: &CacheNode) -> String {
    let kind = if node.type_.is_empty() { "?" } else { node.type_.as_str() };
    let mut s = format!("{} \"{}\"", kind, node.name);
    if let Some(b) = node.bounds {
        let _ = write!(s, " ({})", b);
    }
    let _ = write!(s, " id:{}", node.id);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_simple_tree() {
        let n = json!({
            "id": "1", "type": "FRAME", "name": "Hero",
            "absoluteBoundingBox": { "x": 0.0, "y": 0.0, "width": 1440.0, "height": 800.0 },
            "children": [
                { "id": "2", "type": "TEXT", "name": "Title",
                  "absoluteBoundingBox": { "x": 0.0, "y": 0.0, "width": 200.0, "height": 40.0 } },
                { "id": "3", "type": "RECTANGLE", "name": "bg",
                  "absoluteBoundingBox": { "x": 0.0, "y": 0.0, "width": 1440.0, "height": 800.0 },
                  "fills": [{ "type": "SOLID", "color": { "r": 1.0, "g": 1.0, "b": 1.0, "a": 1.0 } }] }
            ]
        });
        let out = render(&n, 2);
        assert!(out.contains("FRAME \"Hero\" (1440×800 @0,0) id:1"));
        assert!(out.contains("├─ TEXT \"Title\" (200×40 @0,0) id:2"));
        assert!(out.contains("└─ RECTANGLE \"bg\" (1440×800 @0,0) #ffffff id:3"));
    }

    #[test]
    fn skips_invisible_children() {
        let n = json!({
            "id": "1", "type": "FRAME", "name": "F",
            "children": [
                { "id": "2", "type": "TEXT", "name": "shown" },
                { "id": "3", "type": "TEXT", "name": "hidden", "visible": false }
            ]
        });
        let out = render(&n, 2);
        assert!(out.contains("shown"));
        assert!(!out.contains("hidden"));
    }

    #[test]
    fn truncates_at_max_depth() {
        let n = json!({
            "type": "FRAME", "name": "root",
            "children": [
                { "type": "FRAME", "name": "L1",
                  "children": [{ "type": "TEXT", "name": "L2" }] }
            ]
        });
        let out = render(&n, 1);
        assert!(out.contains("L1"));
        assert!(out.contains("…"));
        assert!(!out.contains("L2"));
    }

    #[test]
    fn structured_nests_visible_children() {
        let n = json!({
            "id": "1", "type": "FRAME", "name": "Hero",
            "absoluteBoundingBox": { "x": 0.0, "y": 0.0, "width": 1440.0, "height": 800.0 },
            "children": [
                { "id": "2", "type": "TEXT", "name": "Title" },
                { "id": "3", "type": "TEXT", "name": "hidden", "visible": false },
            ]
        });
        let out = render_structured(&n, 4);
        assert_eq!(out["id"], "1");
        assert_eq!(out["bounds"], "1440×800 @0,0");
        let kids = out["children"].as_array().unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0]["name"], "Title");
    }

    #[test]
    fn compact_leaf_is_scalar_string() {
        let n = json!({ "id": "1", "type": "FRAME", "name": "Hero" });
        let out = render_compact(&n, 4);
        assert_eq!(out, json!("FRAME \"Hero\" id:1"));
    }

    #[test]
    fn compact_parent_is_single_key_map() {
        let n = json!({
            "id": "1", "type": "FRAME", "name": "Hero",
            "children": [
                { "id": "2", "type": "TEXT", "name": "Title" },
                { "id": "3", "type": "TEXT", "name": "hidden", "visible": false },
                { "id": "4", "type": "FRAME", "name": "Body",
                  "children": [{ "id": "5", "type": "TEXT", "name": "Sub" }] }
            ]
        });
        let out = render_compact(&n, 4);
        let parent_key = "FRAME \"Hero\" id:1";
        let kids = out[parent_key].as_array().unwrap();
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[0], json!("TEXT \"Title\" id:2"));
        let body = &kids[1];
        let body_key = "FRAME \"Body\" id:4";
        assert_eq!(body[body_key][0], json!("TEXT \"Sub\" id:5"));
    }

    #[test]
    fn compact_truncates_appends_child_count_suffix() {
        let n = json!({
            "type": "FRAME", "name": "root", "id": "0",
            "children": [
                { "type": "FRAME", "name": "L1", "id": "1",
                  "children": [
                      { "type": "TEXT", "name": "L2a", "id": "2" },
                      { "type": "TEXT", "name": "L2b", "id": "3" }
                  ]}
            ]
        });
        let out = render_compact(&n, 1);
        let root_key = "FRAME \"root\" id:0";
        let l1 = &out[root_key][0];
        assert_eq!(l1, &json!("FRAME \"L1\" id:1 [+2 children]"));
    }

    #[test]
    fn structured_truncates_marks_remaining_count() {
        let n = json!({
            "type": "FRAME", "name": "root",
            "children": [
                { "type": "FRAME", "name": "L1",
                  "children": [
                      { "type": "TEXT", "name": "L2a" },
                      { "type": "TEXT", "name": "L2b" }
                  ]}
            ]
        });
        let out = render_structured(&n, 1);
        let l1 = &out["children"][0];
        assert!(l1.get("children").is_none());
        assert_eq!(l1["truncated"]["children"], 2);
    }
}

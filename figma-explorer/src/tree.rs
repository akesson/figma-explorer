//! Compact ASCII tree renderer for Figma nodes.
//!
//! Output is purely text — one node per line, with box-drawing characters
//! for the structure and a short payload (type, name, dimensions, primary
//! color when applicable). Invisible nodes are skipped.

use serde_json::Value;
use std::fmt::Write;

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

fn format_node_line(node: &Value) -> String {
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
}

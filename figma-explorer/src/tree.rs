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
// Flat renderer (new ls/find output format — pipe rail, padded left columns)
// ─────────────────────────────────────────────────────────────────────────────

/// Max characters rendered for any single `"name"` / path-component string
/// in the flat YAML output. Figma auto-names TEXT nodes by their content, so
/// designers pasting paragraphs of design notes turn a frame title into a
/// 1000-char wall. Truncating at the renderer keeps the line scannable
/// without losing the underlying data (`--json` paths emit the full name).
pub const NAME_DISPLAY_MAX: usize = 200;

/// UTF-8-safe character truncation. Returns the input unchanged when it fits
/// in `max` chars; otherwise returns `<first max-1 chars>…`. The trailing
/// horizontal-ellipsis is one character, so the result is at most `max` chars.
pub fn truncate_display(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if max == 0 {
        return std::borrow::Cow::Borrowed("");
    }
    let count = s.chars().count();
    if count <= max {
        return std::borrow::Cow::Borrowed(s);
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    std::borrow::Cow::Owned(format!("{head}…"))
}

/// Per-listing context that lets every line align column 1 (id) and column 2
/// (bounds) at the same width. Both widths are computed once across the
/// entire listing in `render_flat` so the pipe rail stays at a fixed column.
#[derive(Clone, Copy, Debug)]
pub struct FormatCtx {
    pub file_synth: u32,
    pub max_id_width: usize,
    pub max_bounds_width: usize,
}

/// Render one node as a single line in the new flat format:
///
/// ```text
/// file:N:x:y    1440x80@0,0     | [indent]TYPE  "name"  [extras]
/// ```
///
/// The left rail (id + bounds) is padded to per-listing local maxima so the
/// pipe lands at a fixed column. Depth indentation lives *after* the pipe in
/// column 3 so `awk '{print $1}'` and `awk '{print $2}'` stay stable.
///
/// `truncated_child_count` lets the caller mark "this node has more visible
/// descendants but we stopped descending here" — rendered as `[+N children]`.
pub fn format_cache_line(
    node: &CacheNode,
    depth: usize,
    ctx: &FormatCtx,
    truncated_child_count: Option<usize>,
) -> String {
    // Empty node id is the marker for a synthesized file-root row — render
    // as the bare `file:N` form so callers can hide the underlying DOCUMENT
    // node (id 0:0) and present a cleaner top line.
    let id_str = if node.id.is_empty() {
        format!("file:{}", ctx.file_synth)
    } else {
        format!("file:{}:{}", ctx.file_synth, node.id)
    };
    let bounds_str = node
        .bounds
        .map(|b| b.compact())
        .unwrap_or_else(|| "-".to_owned());
    let kind = if node.type_.is_empty() {
        "?"
    } else {
        node.type_.as_str()
    };
    let indent = "  ".repeat(depth);
    let mut s = format!(
        "{id:<id_w$}  {b:<b_w$}  | {indent}{kind}  \"{name}\"",
        id = id_str,
        b = bounds_str,
        indent = indent,
        kind = kind,
        name = truncate_display(&node.name, NAME_DISPLAY_MAX),
        id_w = ctx.max_id_width,
        b_w = ctx.max_bounds_width,
    );
    if let Some(n) = truncated_child_count {
        let _ = write!(s, "  [+{n} children]");
    }
    s
}

/// Pre-order DFS that collects (node, depth, optional truncated-count) tuples
/// for every visible node down to `max_depth` levels of children. Invisible
/// nodes (and their subtrees) are skipped at every level.
pub(crate) fn collect_visible<'a>(
    node: &'a CacheNode,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<(&'a CacheNode, usize, Option<usize>)>,
) {
    if !node.visible {
        return;
    }
    let visible_count = node.children.iter().filter(|c| c.visible).count();
    let truncated = if depth >= max_depth && visible_count > 0 {
        Some(visible_count)
    } else {
        None
    };
    out.push((node, depth, truncated));
    if depth < max_depth {
        for c in node.children.iter().filter(|c| c.visible) {
            collect_visible(c, depth + 1, max_depth, out);
        }
    }
}

/// Render `root` and `max_depth` levels of descendants as a vector of flat
/// lines, one per visible node. Two-pass: first walk collects nodes (and
/// computes truncation markers); second pass measures id/bounds widths and
/// formats. Returns owned `String`s ready to be joined with `\n`.
pub fn render_flat(root: &CacheNode, file_synth: u32, max_depth: usize) -> Vec<String> {
    render_flat_with_comments(root, file_synth, max_depth, &[])
}

/// Synthetic row for a comment thread head, injected into [`render_flat_with_comments`]
/// output. Bounds stay `-` and the kind column is always `COMMENT`; everything
/// after the kind is assembled by the caller into `display`.
#[derive(Debug, Clone)]
pub struct CommentRow {
    /// Pre-formatted id column, e.g. `"file:2:comm:34"`.
    pub id: String,
    /// Anchor node id (matches a `CacheNode::id` in the tree). `None` for
    /// canvas-level or stale-reference comments — those float to the trailing
    /// block under the file root.
    pub anchor_node_id: Option<String>,
    /// Trailing payload after the kind label. The caller controls
    /// truncation, quoting, suffixes (`(+N replies)`), etc.
    pub display: String,
}

/// Partition comment rows into (anchored-by-node, canvas-level). Anchored rows
/// are keyed by `anchor_node_id` for O(1) lookup during render; `None`-anchored
/// rows collect into the canvas vec. The returned maps borrow from `rows`.
///
/// Single home for a partition that every comment-aware renderer needs (flat
/// YAML, JSON subtree, and `ls`'s row builder). Callers that don't surface
/// canvas-level comments themselves can ignore the second element.
pub(crate) fn group_comments(
    rows: &[CommentRow],
) -> (
    std::collections::HashMap<&str, Vec<&CommentRow>>,
    Vec<&CommentRow>,
) {
    let mut by_node: std::collections::HashMap<&str, Vec<&CommentRow>> =
        std::collections::HashMap::new();
    let mut canvas: Vec<&CommentRow> = Vec::new();
    for row in rows {
        match row.anchor_node_id.as_deref() {
            Some(nid) => by_node.entry(nid).or_default().push(row),
            None => canvas.push(row),
        }
    }
    (by_node, canvas)
}

/// Like [`render_flat`] but interleaves comment rows under their anchor
/// nodes. Canvas-level (`anchor_node_id == None`) rows emit as a trailing
/// block at depth 1, after every node row. The column-alignment pass
/// considers both row kinds so the pipe rail stays at a fixed column.
pub fn render_flat_with_comments(
    root: &CacheNode,
    file_synth: u32,
    max_depth: usize,
    comment_rows: &[CommentRow],
) -> Vec<String> {
    render_flat_impl(root, file_synth, max_depth, comment_rows, true)
}

/// Like [`render_flat_with_comments`] but renders no COMMENT rows. Instead, a
/// node that anchors N thread heads gets a trailing `[N comments]` suffix, and
/// canvas-level (unanchored) threads are omitted entirely — the caller's
/// file-level header accounts for them. Keeps `ls` output scannable when
/// discovery, not discussion, is the goal.
pub fn render_flat_with_comment_counts(
    root: &CacheNode,
    file_synth: u32,
    max_depth: usize,
    comment_rows: &[CommentRow],
) -> Vec<String> {
    render_flat_impl(root, file_synth, max_depth, comment_rows, false)
}

/// The `  [N comments]` suffix (pluralized) shared by the flat renderer and
/// `ls`'s root/project row printer, mirroring the `[+N children]` idiom.
pub fn comment_count_suffix(count: usize) -> String {
    let label = if count == 1 { "comment" } else { "comments" };
    format!("  [{count} {label}]")
}

/// Shared body of [`render_flat_with_comments`] (`inline = true`) and
/// [`render_flat_with_comment_counts`] (`inline = false`). In inline mode each
/// anchored thread becomes its own COMMENT row and canvas-level threads trail
/// at depth 1; in counts mode anchored threads fold into a `[N comments]`
/// suffix and canvas-level threads are dropped.
fn render_flat_impl(
    root: &CacheNode,
    file_synth: u32,
    max_depth: usize,
    comment_rows: &[CommentRow],
    inline: bool,
) -> Vec<String> {
    let mut items: Vec<(&CacheNode, usize, Option<usize>)> = Vec::new();
    collect_visible(root, 0, max_depth, &mut items);
    if items.is_empty() {
        return Vec::new();
    }

    // Group comments by anchor node id for O(1) lookup at render time.
    let (by_node, canvas) = group_comments(comment_rows);

    // Width pass — id and bounds widths span node rows, plus comment rows only
    // when they'll actually be emitted (inline mode).
    let node_id_widths = items.iter().map(|(n, _, _)| {
        if n.id.is_empty() {
            format!("file:{}", file_synth).len()
        } else {
            format!("file:{}:{}", file_synth, n.id).len()
        }
    });
    let node_bounds_widths = items
        .iter()
        .map(|(n, _, _)| n.bounds.map(|b| b.compact().len()).unwrap_or(1));
    let (max_id_width, max_bounds_width) = if inline {
        // Comment rows always render bounds as "-" (1 char); including 1 in the
        // max is a no-op but keeps the intent visible.
        (
            node_id_widths
                .chain(comment_rows.iter().map(|r| r.id.len()))
                .max()
                .unwrap_or(0),
            node_bounds_widths
                .chain(comment_rows.iter().map(|_| 1usize))
                .max()
                .unwrap_or(1),
        )
    } else {
        (
            node_id_widths.max().unwrap_or(0),
            node_bounds_widths.max().unwrap_or(1),
        )
    };

    let ctx = FormatCtx {
        file_synth,
        max_id_width,
        max_bounds_width,
    };
    let mut out: Vec<String> = Vec::with_capacity(items.len() + comment_rows.len());
    for (n, depth, trunc) in &items {
        let anchored = by_node.get(n.id.as_str());
        if inline {
            out.push(format_cache_line(n, *depth, &ctx, *trunc));
            if let Some(rows) = anchored {
                for row in rows {
                    out.push(format_comment_line(row, depth + 1, &ctx));
                }
            }
        } else {
            let mut line = format_cache_line(n, *depth, &ctx, *trunc);
            if let Some(rows) = anchored {
                line.push_str(&comment_count_suffix(rows.len()));
            }
            out.push(line);
        }
    }
    // Canvas-level threads: inline mode renders them after every anchored row,
    // at depth 1 (direct children of the file root); counts mode drops them.
    if inline {
        for row in &canvas {
            out.push(format_comment_line(row, 1, &ctx));
        }
    }
    out
}

/// Render one [`CommentRow`] as a flat line matching the node-row layout.
/// Bounds column is always `-`; the kind column is `COMMENT` followed by the
/// caller-assembled `display` payload.
pub fn format_comment_line(row: &CommentRow, depth: usize, ctx: &FormatCtx) -> String {
    let indent = "  ".repeat(depth);
    format!(
        "{id:<id_w$}  {b:<b_w$}  | {indent}COMMENT  {display}",
        id = row.id,
        b = "-",
        indent = indent,
        display = row.display,
        id_w = ctx.max_id_width,
        b_w = ctx.max_bounds_width,
    )
}

/// JSON tree builder. Attaches a `comments` array per node from the
/// pre-associated `comment_rows` slice; canvas-level threads are handled by
/// the caller and not emitted on individual nodes.
pub(crate) fn render_subtree_json_with_comments(
    file_synth: u32,
    node: &CacheNode,
    max_depth: usize,
    comment_rows: &[CommentRow],
) -> Value {
    // Only the anchored half is used here; canvas-level threads are emitted by
    // the caller at the file root.
    let (by_node, _canvas) = group_comments(comment_rows);
    fn build(
        node: &CacheNode,
        file_synth: u32,
        depth: usize,
        max_depth: usize,
        by_node: &std::collections::HashMap<&str, Vec<&CommentRow>>,
    ) -> Value {
        let mut obj = Map::new();
        let id_str = if node.id.is_empty() {
            format!("file:{file_synth}")
        } else {
            format!("file:{file_synth}:{}", node.id)
        };
        obj.insert("id".into(), json!(id_str));
        obj.insert("type".into(), json!(node.type_));
        obj.insert("name".into(), json!(node.name));
        if let Some(b) = node.bounds {
            obj.insert("bounds".into(), json!(b.compact()));
        }
        if let Some(rows) = by_node.get(node.id.as_str()) {
            let comments: Vec<Value> = rows.iter().map(|r| comment_row_json(r)).collect();
            obj.insert("comments".into(), Value::Array(comments));
        }
        let kids: Vec<&CacheNode> = node.children.iter().filter(|c| c.visible).collect();
        if !kids.is_empty() {
            if depth >= max_depth {
                obj.insert("truncated".into(), json!({ "children": kids.len() }));
            } else {
                let rendered: Vec<Value> = kids
                    .iter()
                    .map(|c| build(c, file_synth, depth + 1, max_depth, by_node))
                    .collect();
                obj.insert("children".into(), Value::Array(rendered));
            }
        }
        Value::Object(obj)
    }
    build(node, file_synth, 0, max_depth, &by_node)
}

/// Serialize a `CommentRow` for the JSON output. Currently includes id and
/// display; the row's `anchor_node_id` is implied by its position in the
/// parent node's `comments` array (or `canvas_comments` at the file root).
pub(crate) fn comment_row_json(row: &CommentRow) -> Value {
    json!({
        "id": row.id,
        "display": row.display,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Bounds;
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

    fn leaf_cache(id: &str, type_: &str, name: &str, bounds: Option<Bounds>) -> CacheNode {
        CacheNode {
            id: id.into(),
            type_: type_.into(),
            name: name.into(),
            visible: true,
            bounds,
            characters: None,
            children: vec![],
        }
    }

    fn sample_tree() -> CacheNode {
        // Header
        // ├─ Title
        // └─ Search [+1 hidden child below depth]
        //    └─ Icon (would be at depth 2)
        let mut search = leaf_cache(
            "1094:66602",
            "INSTANCE",
            "Search",
            Some(Bounds {
                x: 720.0,
                y: 20.0,
                width: 320.0,
                height: 40.0,
            }),
        );
        search.children.push(leaf_cache(
            "1094:66602:1",
            "VECTOR",
            "icon",
            Some(Bounds {
                x: 732.0,
                y: 32.0,
                width: 16.0,
                height: 16.0,
            }),
        ));
        let mut header = leaf_cache(
            "1094:66600",
            "FRAME",
            "Header",
            Some(Bounds {
                x: 0.0,
                y: 0.0,
                width: 1440.0,
                height: 80.0,
            }),
        );
        header.children.push(leaf_cache(
            "1094:66601",
            "TEXT",
            "Title",
            Some(Bounds {
                x: 24.0,
                y: 24.0,
                width: 200.0,
                height: 32.0,
            }),
        ));
        header.children.push(search);
        header
    }

    #[test]
    fn truncate_display_passes_short_strings_through() {
        assert_eq!(truncate_display("hello", 200), "hello");
        assert_eq!(truncate_display("", 200), "");
    }

    #[test]
    fn truncate_display_caps_long_strings_with_ellipsis() {
        let long: String = "a".repeat(500);
        let truncated = truncate_display(&long, 200);
        assert_eq!(truncated.chars().count(), 200);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_display_is_utf8_safe() {
        // Multi-byte chars must not be split mid-codepoint.
        let mixed = "é".repeat(300); // each é is 2 bytes
        let truncated = truncate_display(&mixed, 100);
        assert_eq!(truncated.chars().count(), 100);
        assert!(truncated.ends_with('…'));
        // Round-trip through bytes to confirm validity.
        let _check = truncated.as_bytes();
    }

    #[test]
    fn flat_format_truncates_long_names() {
        let mut node = leaf_cache("1:1", "TEXT", "x", None);
        node.name = "a".repeat(500);
        let lines = render_flat(&node, 2, 0);
        // 200-char name + 2 quote chars = 202; ensure the line contains an
        // ellipsis and is much shorter than 500.
        let line = &lines[0];
        assert!(line.contains('…'), "expected ellipsis, got: {line}");
        // Whole rendered name field should be <= 202 chars (200 + 2 quotes).
        let start = line.find('"').unwrap();
        let end = line[start + 1..].find('"').unwrap() + start + 1;
        let quoted = &line[start..=end];
        assert!(
            quoted.chars().count() <= 202,
            "quoted name not capped: {} chars",
            quoted.chars().count()
        );
    }

    #[test]
    fn flat_format_emits_qualified_id_in_column_one() {
        let lines = render_flat(&sample_tree(), 2, 2);
        // Every line starts with the qualified id "file:2:..."
        for l in &lines {
            assert!(l.starts_with("file:2:"), "bad prefix: {l}");
        }
    }

    #[test]
    fn flat_format_pads_id_column_to_local_max() {
        let lines = render_flat(&sample_tree(), 2, 2);
        // All lines should land the pipe at the same column.
        let pipe_cols: Vec<usize> = lines
            .iter()
            .map(|l| l.find('|').expect("pipe present"))
            .collect();
        let first = pipe_cols[0];
        assert!(
            pipe_cols.iter().all(|c| *c == first),
            "pipe wobble: {pipe_cols:?}"
        );
    }

    #[test]
    fn flat_format_emits_bounds_compact_no_spaces() {
        let lines = render_flat(&sample_tree(), 2, 2);
        let header_line = lines.iter().find(|l| l.contains("\"Header\"")).unwrap();
        // Compact form: 1440x80@0,0 — no spaces inside the bounds field.
        assert!(
            header_line.contains("1440x80@0,0"),
            "expected compact bounds, got: {header_line}"
        );
        // Make sure the legacy `1440×80 @0,0` form did NOT sneak in.
        assert!(
            !header_line.contains("1440×80 @0,0"),
            "legacy form leaked: {header_line}"
        );
    }

    #[test]
    fn flat_format_indents_after_pipe_only() {
        let lines = render_flat(&sample_tree(), 2, 2);
        // The Title line is a child of Header (depth 1). The indent after
        // the pipe should start with "  " (two spaces) for depth 1.
        let title_line = lines.iter().find(|l| l.contains("\"Title\"")).unwrap();
        let pipe = title_line.find('|').unwrap();
        let after = &title_line[pipe + 2..]; // skip "| "
        assert!(
            after.starts_with("  TEXT"),
            "expected '  TEXT' after pipe, got {after:?}"
        );
    }

    #[test]
    fn flat_format_marks_truncation_with_child_count() {
        // Render depth 1 — Search (which has 1 child) should show [+1 children].
        let lines = render_flat(&sample_tree(), 2, 1);
        let search_line = lines.iter().find(|l| l.contains("\"Search\"")).unwrap();
        assert!(
            search_line.contains("[+1 children]"),
            "missing trunc marker: {search_line}"
        );
        // No deeper lines should appear.
        assert!(!lines.iter().any(|l| l.contains("\"icon\"")));
    }

    #[test]
    fn flat_format_skips_invisible_nodes() {
        let mut hidden_child = leaf_cache("1:99", "TEXT", "hidden", None);
        hidden_child.visible = false;
        let mut root = leaf_cache("1:1", "FRAME", "Root", None);
        root.children.push(hidden_child);
        root.children.push(leaf_cache("1:2", "TEXT", "shown", None));
        let lines = render_flat(&root, 1, 3);
        assert!(lines.iter().any(|l| l.contains("\"shown\"")));
        assert!(!lines.iter().any(|l| l.contains("\"hidden\"")));
    }

    #[test]
    fn flat_format_bounds_dash_when_missing() {
        let leaf = leaf_cache("0:0", "DOCUMENT", "doc", None);
        let lines = render_flat(&leaf, 1, 0);
        let line = &lines[0];
        // Bounds column shows "-" — verify by checking the bounds field
        // between the id and the pipe.
        let pipe = line.find('|').unwrap();
        let head = &line[..pipe];
        assert!(
            head.contains(" -  "),
            "expected '-' bounds marker, got: {line}"
        );
    }

    #[test]
    fn flat_format_with_comments_splices_anchored_rows() {
        let tree = sample_tree();
        let rows = vec![
            CommentRow {
                id: "file:2:comm:1".into(),
                anchor_node_id: Some("1094:66601".into()), // Title
                display: "\"looks great\"  by @alice".into(),
            },
            CommentRow {
                id: "file:2:comm:2".into(),
                anchor_node_id: Some("1094:66602".into()), // Search
                display: "\"missing icon\"  by @bob  +2".into(),
            },
        ];
        let lines = render_flat_with_comments(&tree, 2, 3, &rows);
        // Find the Title row's index, then assert the next line is comm:1.
        let title_i = lines.iter().position(|l| l.contains("\"Title\"")).unwrap();
        assert!(lines[title_i + 1].contains("file:2:comm:1"));
        assert!(lines[title_i + 1].contains("COMMENT"));
        assert!(lines[title_i + 1].contains("looks great"));

        // Same for Search → comm:2.
        let search_i = lines.iter().position(|l| l.contains("\"Search\"")).unwrap();
        assert!(lines[search_i + 1].contains("file:2:comm:2"));
        assert!(lines[search_i + 1].contains("+2"));
    }

    #[test]
    fn flat_format_with_comments_canvas_rows_trail_at_depth_one() {
        let tree = sample_tree();
        let rows = vec![CommentRow {
            id: "file:2:comm:7".into(),
            anchor_node_id: None,
            display: "\"unanchored thought\"  by @carol".into(),
        }];
        let lines = render_flat_with_comments(&tree, 2, 3, &rows);
        // Canvas-level rows appear at the end.
        let last = lines.last().unwrap();
        assert!(last.contains("file:2:comm:7"));
        assert!(last.contains("COMMENT"));
        // Their indent (depth 1, 2 spaces after the pipe) lines up with
        // direct children of the root.
        let pipe = last.find('|').unwrap();
        let after = &last[pipe + 2..];
        assert!(
            after.starts_with("  COMMENT"),
            "expected depth-1 indent, got: {after}"
        );
    }

    #[test]
    fn flat_counts_mode_appends_suffix_and_omits_rows() {
        let tree = sample_tree();
        let rows = vec![
            CommentRow {
                id: "file:2:comm:1".into(),
                anchor_node_id: Some("1094:66601".into()), // Title
                display: "\"looks great\"  by @alice".into(),
            },
            CommentRow {
                id: "file:2:comm:2".into(),
                anchor_node_id: Some("1094:66601".into()), // Title (second thread)
                display: "\"and again\"  by @bob".into(),
            },
        ];
        let lines = render_flat_with_comment_counts(&tree, 2, 3, &rows);
        let title_line = lines.iter().find(|l| l.contains("\"Title\"")).unwrap();
        assert!(
            title_line.ends_with("[2 comments]"),
            "expected count suffix: {title_line}"
        );
        // No COMMENT rows anywhere in counts mode.
        assert!(
            !lines.iter().any(|l| l.contains("COMMENT")),
            "counts mode leaked a COMMENT row: {lines:?}"
        );
    }

    #[test]
    fn flat_counts_mode_drops_canvas_level_rows() {
        let tree = sample_tree();
        let rows = vec![CommentRow {
            id: "file:2:comm:7".into(),
            anchor_node_id: None, // canvas-level
            display: "\"unanchored\"  by @carol".into(),
        }];
        let lines = render_flat_with_comment_counts(&tree, 2, 3, &rows);
        assert!(
            !lines.iter().any(|l| l.contains("comm:7")),
            "canvas-level thread must not render in counts mode: {lines:?}"
        );
        assert!(!lines.iter().any(|l| l.contains("[")), "no suffixes either");
    }

    #[test]
    fn flat_format_with_comments_keeps_pipe_aligned() {
        let tree = sample_tree();
        let rows = vec![CommentRow {
            id: "file:2:comm:1000".into(), // Wider than any node id in sample_tree
            anchor_node_id: Some("1094:66600".into()),
            display: "x".into(),
        }];
        let lines = render_flat_with_comments(&tree, 2, 3, &rows);
        let pipe_cols: Vec<usize> = lines.iter().map(|l| l.find('|').unwrap()).collect();
        let first = pipe_cols[0];
        assert!(
            pipe_cols.iter().all(|c| *c == first),
            "comment row broke pipe alignment: {pipe_cols:?}"
        );
    }

    #[test]
    fn flat_format_awk_friendly_extraction() {
        let lines = render_flat(&sample_tree(), 2, 2);
        // Simulate `awk '{print $1}'` — first whitespace-delimited token.
        for l in &lines {
            let first = l.split_whitespace().next().unwrap();
            assert!(first.starts_with("file:2:"), "first token isn't id: {l}");
        }
        // Simulate `awk '{print $2}'` — second token is bounds (or "-").
        for l in &lines {
            let second = l.split_whitespace().nth(1).unwrap();
            assert!(
                second == "-" || second.contains('@'),
                "second token isn't bounds: {l}"
            );
        }
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

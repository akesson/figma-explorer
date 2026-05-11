//! Resolve a user-facing target (page name, frame name, or fuzzy query)
//! to a concrete node id within a Figma document.
//!
//! Match precedence (mirrors figma-mcp behavior):
//!   1. Exact case-insensitive match on `name`.
//!   2. Case-insensitive substring match.
//!   3. Fuzzy rank via nucleo-matcher.
//!
//! Hidden nodes (`visible: false`) are skipped from candidate sets — they
//! aren't part of the design surface.

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Matcher,
};
use serde_json::Value;

use crate::node::{children, id, is_visible, name, type_str};

/// A single fuzzy-search hit, with its path through page → frame → … for
/// disambiguation.
#[derive(Clone, Debug)]
pub struct Hit<'a> {
    pub node: &'a Value,
    pub node_id: String,
    pub name: String,
    pub kind: String,
    pub path: Vec<String>,
}

/// Top-level pages of the document (CANVAS nodes), in order.
pub fn pages(doc: &Value) -> &[Value] {
    children(doc)
}

/// First page that matches `query` (exact → substring → fuzzy).
pub fn resolve_page<'a>(doc: &'a Value, query: &str) -> Option<&'a Value> {
    pick_match(pages(doc), query)
}

/// First direct child of `page` that matches `query`.
pub fn resolve_frame<'a>(page: &'a Value, query: &str) -> Option<&'a Value> {
    pick_match(children(page), query)
}

/// First descendant of `root` that matches `query`. Walks the whole subtree.
pub fn resolve_descendant<'a>(root: &'a Value, query: &str) -> Option<&'a Value> {
    let mut all: Vec<&'a Value> = Vec::new();
    collect_visible(root, &mut all);
    pick_match_slice(&all, query)
}

/// Find a node by exact node id anywhere in the tree. Node ids are stable
/// identifiers; we don't filter on visibility here.
pub fn resolve_node_id<'a>(doc: &'a Value, node_id: &str) -> Option<&'a Value> {
    fn find<'a>(n: &'a Value, target: &str) -> Option<&'a Value> {
        if id(n) == Some(target) {
            return Some(n);
        }
        for c in children(n) {
            if let Some(hit) = find(c, target) {
                return Some(hit);
            }
        }
        None
    }
    find(doc, node_id)
}

/// Fuzzy search across the whole document. Up to `limit` hits, ranked by
/// score (best first). Skips invisible nodes.
pub fn fuzzy_search<'a>(doc: &'a Value, query: &str, limit: usize) -> Vec<Hit<'a>> {
    let mut candidates: Vec<(&Value, Vec<String>)> = Vec::new();
    collect_with_path(doc, &mut Vec::new(), &mut candidates);

    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

    let mut buf: Vec<char> = Vec::new();
    let mut scored: Vec<(u32, Hit<'a>)> = candidates
        .into_iter()
        .filter_map(|(node, path)| {
            let n = name(node)?;
            buf.clear();
            buf.extend(n.chars());
            let score =
                pattern.score(nucleo_matcher::Utf32Str::new(n, &mut buf), &mut matcher)?;
            Some((
                score,
                Hit {
                    node,
                    node_id: id(node).unwrap_or("").to_owned(),
                    name: n.to_owned(),
                    kind: type_str(node).unwrap_or("").to_owned(),
                    path,
                },
            ))
        })
        .collect();
    scored.sort_by_key(|h| std::cmp::Reverse(h.0));
    scored.truncate(limit);
    scored.into_iter().map(|(_, h)| h).collect()
}

/// Pick the best match from a slice of node references.
fn pick_match_slice<'a>(candidates: &[&'a Value], query: &str) -> Option<&'a Value> {
    let q = query.trim();
    if q.is_empty() {
        return candidates.iter().find(|n| is_visible(n)).copied();
    }
    let q_lower = q.to_lowercase();

    if let Some(hit) = candidates
        .iter()
        .find(|n| is_visible(n) && name(n).is_some_and(|nm| nm.eq_ignore_ascii_case(q)))
    {
        return Some(*hit);
    }
    if let Some(hit) = candidates.iter().find(|n| {
        is_visible(n) && name(n).is_some_and(|nm| nm.to_lowercase().contains(&q_lower))
    }) {
        return Some(*hit);
    }
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = Pattern::parse(q, CaseMatching::Ignore, Normalization::Smart);
    let mut buf: Vec<char> = Vec::new();
    candidates
        .iter()
        .filter_map(|n| {
            if !is_visible(n) {
                return None;
            }
            let nm = name(n)?;
            buf.clear();
            buf.extend(nm.chars());
            let score =
                pattern.score(nucleo_matcher::Utf32Str::new(nm, &mut buf), &mut matcher)?;
            Some((score, *n))
        })
        .max_by_key(|(s, _)| *s)
        .map(|(_, n)| n)
}

/// Pick the best match from a slice of owned `Value`s (typical case: results
/// of `children(...)`).
fn pick_match<'a>(candidates: &'a [Value], query: &str) -> Option<&'a Value> {
    let refs: Vec<&Value> = candidates.iter().collect();
    pick_match_slice(&refs, query)
}

fn collect_visible<'a>(root: &'a Value, out: &mut Vec<&'a Value>) {
    if !is_visible(root) {
        return;
    }
    for c in children(root) {
        out.push(c);
        collect_visible(c, out);
    }
}

fn collect_with_path<'a>(
    node: &'a Value,
    path: &mut Vec<String>,
    out: &mut Vec<(&'a Value, Vec<String>)>,
) {
    if !is_visible(node) {
        return;
    }
    if let Some(nm) = name(node) {
        if !nm.is_empty() {
            out.push((node, path.clone()));
        }
    }
    let nm = name(node).unwrap_or("").to_owned();
    path.push(nm);
    for c in children(node) {
        collect_with_path(c, path, out);
    }
    path.pop();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc() -> Value {
        json!({
            "type": "DOCUMENT",
            "name": "doc",
            "children": [
                { "id": "1:0", "type": "CANVAS", "name": "Home", "children": [
                    { "id": "1:1", "type": "FRAME", "name": "Hero", "children": [
                        { "id": "1:2", "type": "TEXT", "name": "Title" },
                        { "id": "1:3", "type": "TEXT", "name": "Subtitle" },
                        { "id": "1:4", "type": "FRAME", "name": "Buttons", "children": [
                            { "id": "1:5", "type": "INSTANCE", "name": "Primary Button" },
                            { "id": "1:6", "type": "INSTANCE", "name": "Secondary Button" }
                        ]}
                    ]},
                    { "id": "1:7", "type": "FRAME", "name": "Features", "visible": false,
                      "children": [{ "id": "1:8", "type": "TEXT", "name": "ignored" }] }
                ]},
                { "id": "2:0", "type": "CANVAS", "name": "About", "children": [] }
            ]
        })
    }

    #[test]
    fn resolve_page_by_exact_name() {
        let d = doc();
        let p = resolve_page(&d, "About").unwrap();
        assert_eq!(id(p), Some("2:0"));
    }

    #[test]
    fn resolve_page_by_substring() {
        let d = doc();
        let p = resolve_page(&d, "hom").unwrap();
        assert_eq!(id(p), Some("1:0"));
    }

    #[test]
    fn resolve_frame_finds_top_level() {
        let d = doc();
        let page = resolve_page(&d, "Home").unwrap();
        let f = resolve_frame(page, "Hero").unwrap();
        assert_eq!(id(f), Some("1:1"));
    }

    #[test]
    fn resolve_descendant_walks_subtree() {
        let d = doc();
        let page = resolve_page(&d, "Home").unwrap();
        let n = resolve_descendant(page, "Primary").unwrap();
        assert_eq!(id(n), Some("1:5"));
    }

    #[test]
    fn fuzzy_search_finds_typoed_query() {
        let d = doc();
        let hits = fuzzy_search(&d, "buton", 5);
        assert!(hits.iter().any(|h| h.name.contains("Button")));
    }

    #[test]
    fn invisible_frame_is_skipped() {
        let d = doc();
        let page = resolve_page(&d, "Home").unwrap();
        // "Features" is invisible — substring match should not return it.
        assert!(resolve_frame(page, "Features").is_none());
    }

    #[test]
    fn resolve_node_id_finds_deep_node() {
        let d = doc();
        let n = resolve_node_id(&d, "1:5").unwrap();
        assert_eq!(name(n), Some("Primary Button"));
    }
}

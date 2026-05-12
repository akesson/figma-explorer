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
//!
//! Two parallel APIs live here:
//! - `&serde_json::Value`-based functions for live-data consumers (`context`,
//!   `styles`, `screenshot`, `extract_assets`) that need access to fields the
//!   cache projection drops (fills, strokes, characters, …).
//! - `_cache` suffixed functions over `&CacheNode` for the cached structural
//!   consumers (`pages`, `frames`, `tree`, `search`). These are the future
//!   direction; the Value-based path stays for now to avoid breaking live
//!   commands during this migration.

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Matcher,
};
use serde_json::Value;

use crate::cache::CacheNode;
use crate::node::{children, id, is_visible, name};

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

// ─────────────────────────────────────────────────────────────────────────────
// CacheNode-typed API (cache consumers)
// ─────────────────────────────────────────────────────────────────────────────

/// Top-level pages of the document (CANVAS nodes), in order.
pub fn pages_cache(doc: &CacheNode) -> &[CacheNode] {
    &doc.children
}

/// First page that matches `query` (exact → substring → fuzzy).
pub fn resolve_page_cache<'a>(doc: &'a CacheNode, query: &str) -> Option<&'a CacheNode> {
    pick_match_cache(&doc.children, query)
}

/// First direct child of `page` that matches `query`.
pub fn resolve_frame_cache<'a>(page: &'a CacheNode, query: &str) -> Option<&'a CacheNode> {
    pick_match_cache(&page.children, query)
}

/// First descendant of `root` that matches `query`. Walks the whole subtree.
pub fn resolve_descendant_cache<'a>(root: &'a CacheNode, query: &str) -> Option<&'a CacheNode> {
    let mut all: Vec<&'a CacheNode> = Vec::new();
    collect_visible_cache(root, &mut all);
    pick_match_cache_slice(&all, query)
}

/// Find a node by exact node id anywhere in the tree. Node ids are stable
/// identifiers; we don't filter on visibility here.
pub fn resolve_node_id_cache<'a>(doc: &'a CacheNode, node_id: &str) -> Option<&'a CacheNode> {
    fn find<'a>(n: &'a CacheNode, target: &str) -> Option<&'a CacheNode> {
        if n.id == target {
            return Some(n);
        }
        for c in &n.children {
            if let Some(hit) = find(c, target) {
                return Some(hit);
            }
        }
        None
    }
    find(doc, node_id)
}

fn pick_match_cache<'a>(candidates: &'a [CacheNode], query: &str) -> Option<&'a CacheNode> {
    let refs: Vec<&CacheNode> = candidates.iter().collect();
    pick_match_cache_slice(&refs, query)
}

fn pick_match_cache_slice<'a>(candidates: &[&'a CacheNode], query: &str) -> Option<&'a CacheNode> {
    let q = query.trim();
    if q.is_empty() {
        return candidates.iter().find(|n| n.visible).copied();
    }
    let q_lower = q.to_lowercase();

    if let Some(hit) = candidates
        .iter()
        .find(|n| n.visible && n.name.eq_ignore_ascii_case(q))
    {
        return Some(*hit);
    }
    if let Some(hit) = candidates
        .iter()
        .find(|n| n.visible && n.name.to_lowercase().contains(&q_lower))
    {
        return Some(*hit);
    }
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = Pattern::parse(q, CaseMatching::Ignore, Normalization::Smart);
    let mut buf: Vec<char> = Vec::new();
    candidates
        .iter()
        .filter_map(|n| {
            if !n.visible {
                return None;
            }
            buf.clear();
            buf.extend(n.name.chars());
            let score =
                pattern.score(nucleo_matcher::Utf32Str::new(&n.name, &mut buf), &mut matcher)?;
            Some((score, *n))
        })
        .max_by_key(|(s, _)| *s)
        .map(|(_, n)| n)
}

fn collect_visible_cache<'a>(root: &'a CacheNode, out: &mut Vec<&'a CacheNode>) {
    if !root.visible {
        return;
    }
    for c in &root.children {
        out.push(c);
        collect_visible_cache(c, out);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Multi-token ancestor-chain search
// ─────────────────────────────────────────────────────────────────────────────

/// Per-token attribution: which path index a token matched, and its raw score.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TokenMatch {
    /// The original token from the query.
    pub token: String,
    /// Index into `SearchHit::path` where the token matched best.
    pub path_index: usize,
    /// The matched ancestor's name (kept for display so callers don't have to
    /// re-walk the path).
    pub matched_name: String,
    /// Nucleo's raw fuzzy score for this token against the matched name.
    pub token_score: u32,
}

/// One result of `multi_token_search`. The path is root → … → node, so the
/// last element is the candidate itself.
#[derive(Clone, Debug)]
pub struct SearchHit<'a> {
    pub node: &'a CacheNode,
    pub path: Vec<&'a CacheNode>,
    pub score: f64,
    pub matches: Vec<TokenMatch>,
}

/// Decay per position step away from the leaf. The leaf gets weight 1.0,
/// the parent 0.7, grandparent 0.49, and so on. Tuned to "leaf token matters
/// most" without zeroing out ancestor matches.
const LEAF_WEIGHT_DECAY: f64 = 0.7;

/// Multiplicative bonus per pair of tokens that hit consecutive path
/// positions in query order. Encodes "wallchart > grid > filter > button"
/// matching as a chain.
const CONSECUTIVE_PAIR_BONUS: f64 = 0.10;

/// Search for nodes whose ancestor chain matches every token in `tokens`.
/// Returns up to `limit` hits ranked by aggregate score.
///
/// Algorithm:
/// 1. DFS through visible nodes, maintaining the root→node path.
/// 2. For each candidate, every token must fuzzy-match (nucleo) the name of
///    some ancestor in the path. If any token has no hit, drop the candidate.
/// 3. Score each token by (nucleo score) * (position weight) where the leaf
///    position gets weight 1.0 and each step earlier decays by
///    `LEAF_WEIGHT_DECAY`. Tokens that match the leaf itself rank highest.
/// 4. Add `CONSECUTIVE_PAIR_BONUS` * total for each adjacent pair of tokens
///    that hit adjacent path indices (token n+1 just below token n).
/// 5. Apply `type_filter` post-walk: if set, drop candidates whose `type_`
///    isn't in the filter (case-insensitive).
/// 6. Sort by score descending, take top `limit`.
pub fn multi_token_search<'a>(
    root: &'a CacheNode,
    tokens: &[&str],
    type_filter: Option<&[&str]>,
    limit: usize,
) -> Vec<SearchHit<'a>> {
    if tokens.is_empty() {
        return Vec::new();
    }

    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let patterns: Vec<Pattern> = tokens
        .iter()
        .map(|t| Pattern::parse(t, CaseMatching::Ignore, Normalization::Smart))
        .collect();

    let type_filter_lower: Option<Vec<String>> =
        type_filter.map(|tf| tf.iter().map(|t| t.to_lowercase()).collect());

    let mut hits: Vec<SearchHit<'a>> = Vec::new();
    let mut path: Vec<&'a CacheNode> = Vec::new();
    walk_with_path(root, &mut path, &mut |node, path| {
        // Skip nodes that can't be a meaningful target.
        if node.name.is_empty() {
            return;
        }
        if let Some(ref tfs) = type_filter_lower {
            if !tfs.iter().any(|t| t == &node.type_.to_lowercase()) {
                return;
            }
        }
        // Best (path_index, score) per token. None → token didn't hit, skip.
        let mut per_token: Vec<(usize, u32, String)> = Vec::with_capacity(tokens.len());
        let mut all_matched = true;
        let mut buf: Vec<char> = Vec::new();
        for (ti, pattern) in patterns.iter().enumerate() {
            let mut best: Option<(usize, u32, String)> = None;
            for (pi, ancestor) in path.iter().enumerate() {
                buf.clear();
                buf.extend(ancestor.name.chars());
                if let Some(s) = pattern.score(
                    nucleo_matcher::Utf32Str::new(&ancestor.name, &mut buf),
                    &mut matcher,
                ) {
                    if best.as_ref().is_none_or(|(_, bs, _)| s > *bs) {
                        best = Some((pi, s, ancestor.name.clone()));
                    }
                }
            }
            match best {
                Some(b) => per_token.push(b),
                None => {
                    all_matched = false;
                    break;
                }
            }
            let _ = ti;
        }
        if !all_matched {
            return;
        }

        let leaf_idx = path.len().saturating_sub(1);
        // Aggregate score with leaf-weighted decay.
        let mut score = 0.0f64;
        let mut matches_out = Vec::with_capacity(per_token.len());
        for (i, (pi, ts, mn)) in per_token.iter().enumerate() {
            let depth_from_leaf = leaf_idx.saturating_sub(*pi);
            let weight = LEAF_WEIGHT_DECAY.powi(depth_from_leaf as i32);
            score += (*ts as f64) * weight;
            matches_out.push(TokenMatch {
                token: tokens[i].to_string(),
                path_index: *pi,
                matched_name: mn.clone(),
                token_score: *ts,
            });
        }
        // Consecutive-pair bonus: each adjacent token pair that lines up on
        // adjacent path indices (in query order) gets a multiplicative boost.
        for w in per_token.windows(2) {
            if w[1].0 == w[0].0 + 1 {
                score *= 1.0 + CONSECUTIVE_PAIR_BONUS;
            }
        }

        hits.push(SearchHit {
            node,
            path: path.to_vec(),
            score,
            matches: matches_out,
        });
    });

    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(limit);
    hits
}

/// DFS over visible nodes, calling `f(node, path_including_node)` at each
/// node. The `path` slice handed to `f` is the full root→node chain.
fn walk_with_path<'a, F>(node: &'a CacheNode, path: &mut Vec<&'a CacheNode>, f: &mut F)
where
    F: FnMut(&'a CacheNode, &[&'a CacheNode]),
{
    if !node.visible {
        return;
    }
    path.push(node);
    f(node, path);
    for c in &node.children {
        walk_with_path(c, path, f);
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

    // ─────────────────────────────────────────────────────────────────────
    // CacheNode-typed API + multi_token_search
    // ─────────────────────────────────────────────────────────────────────

    fn cache_leaf(id: &str, name: &str, type_: &str) -> CacheNode {
        CacheNode {
            id: id.into(),
            type_: type_.into(),
            name: name.into(),
            visible: true,
            bounds: None,
            children: vec![],
        }
    }

    fn cache_node(id: &str, name: &str, type_: &str, children: Vec<CacheNode>) -> CacheNode {
        CacheNode {
            id: id.into(),
            type_: type_.into(),
            name: name.into(),
            visible: true,
            bounds: None,
            children,
        }
    }

    /// Wallchart → Grid → Filter → Button — the canonical test shape from
    /// the design discussion. Plus a few distractors so ranking is meaningful.
    fn wallchart_doc() -> CacheNode {
        cache_node(
            "0:0",
            "doc",
            "DOCUMENT",
            vec![cache_node(
                "1:0",
                "Home",
                "CANVAS",
                vec![
                    cache_node(
                        "1:1",
                        "Wallchart",
                        "FRAME",
                        vec![cache_node(
                            "1:2",
                            "Grid",
                            "FRAME",
                            vec![
                                cache_node(
                                    "1:3",
                                    "Filter",
                                    "FRAME",
                                    vec![
                                        cache_leaf("1:4", "Button", "INSTANCE"),
                                        cache_leaf("1:5", "Label", "TEXT"),
                                    ],
                                ),
                                // Distractor: a "Button" not under "Filter".
                                cache_leaf("1:6", "Button", "INSTANCE"),
                            ],
                        )],
                    ),
                    // Distractor sibling tree without "filter" anywhere.
                    cache_node(
                        "2:1",
                        "Sidebar",
                        "FRAME",
                        vec![cache_leaf("2:2", "Button", "INSTANCE")],
                    ),
                ],
            )],
        )
    }

    #[test]
    fn multi_token_chain_match_locates_leaf_under_full_chain() {
        let d = wallchart_doc();
        let hits = multi_token_search(&d, &["wallchart", "grid", "filter", "button"], None, 5);
        assert!(!hits.is_empty(), "expected at least one hit");
        // The top hit must be the button INSIDE Filter (1:4), not the
        // sibling distractors at 1:6 or 2:2.
        assert_eq!(hits[0].node.id, "1:4");
    }

    #[test]
    fn multi_token_requires_all_tokens_to_match_chain() {
        let d = wallchart_doc();
        // "filter" doesn't exist on the Sidebar branch — the Sidebar Button
        // (2:2) must NOT appear in the hits.
        let hits = multi_token_search(&d, &["wallchart", "filter", "button"], None, 10);
        for h in &hits {
            assert_ne!(h.node.id, "2:2", "Sidebar button leaked despite missing 'filter'");
        }
    }

    #[test]
    fn multi_token_leaf_weighted_higher_than_ancestor_match() {
        let d = wallchart_doc();
        // "button" matches both 1:4 (leaf "Button" under Filter) and the
        // ancestor-only-Button on 1:6 (sibling of Filter, name "Button").
        // The chain query "wallchart grid filter button" should prefer 1:4
        // because the leaf token "button" lines up with the leaf node.
        let hits = multi_token_search(&d, &["wallchart", "grid", "filter", "button"], None, 5);
        let pos_14 = hits.iter().position(|h| h.node.id == "1:4");
        let pos_16 = hits.iter().position(|h| h.node.id == "1:6");
        match (pos_14, pos_16) {
            (Some(a), Some(b)) => assert!(a < b, "1:4 should outrank 1:6 (got {a} vs {b})"),
            (Some(_), None) => { /* 1:6 was filtered: also fine — 1:4 still wins. */ }
            (None, _) => panic!("expected 1:4 in results"),
        }
    }

    #[test]
    fn multi_token_type_filter_drops_non_matching_types() {
        let d = wallchart_doc();
        // With type=TEXT, only the TEXT leaf under Filter ("Label", id 1:5)
        // can match, and only if all tokens still hit on its chain.
        let hits = multi_token_search(
            &d,
            &["wallchart", "grid", "filter"],
            Some(&["TEXT"]),
            10,
        );
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|h| h.node.type_ == "TEXT"));
        assert!(hits.iter().any(|h| h.node.id == "1:5"));
    }

    #[test]
    fn multi_token_empty_query_returns_empty() {
        let d = wallchart_doc();
        let hits = multi_token_search(&d, &[], None, 10);
        assert!(hits.is_empty());
    }

    #[test]
    fn resolve_node_id_cache_finds_deep_node() {
        let d = wallchart_doc();
        let n = resolve_node_id_cache(&d, "1:4").expect("expected to find 1:4");
        assert_eq!(n.name, "Button");
    }
}

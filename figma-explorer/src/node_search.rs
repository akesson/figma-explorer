//! Two unrelated lookups over Figma documents:
//!
//! - `resolve_node_id`: single-node lookup by native node id on a raw
//!   `serde_json::Value` document. Used by the live-data commands
//!   (`tokens`, `assets`, `context`, `node_info`) that need fields the cache
//!   projection drops (fills, strokes, characters, …).
//! - `multi_token_search`: ancestor-chain ranked search over `CacheNode`
//!   trees. Used by `find`. See the section header further down for the
//!   algorithm.

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Matcher,
};
use serde_json::Value;

use crate::cache::CacheNode;
use crate::node::{children, id};

/// Find a node by exact node id anywhere in the tree. Node ids are stable
/// identifiers; we don't filter on visibility here.
///
/// Counterpart to [`crate::resolver::find_node`], which is the same DFS over
/// the structural `CacheNode` projection and returns an owned clone. This one
/// walks the raw `serde_json::Value` document (so live-data commands can reach
/// fields the cache drops) and returns a borrow. The duplication is
/// intentional — different node type, different ownership.
pub fn resolve_node_id<'a>(doc: &'a Value, node_id: &str) -> Option<&'a Value> {
    fn find<'a>(n: &'a Value, target: &str, depth: usize) -> Option<&'a Value> {
        if id(n) == Some(target) {
            return Some(n);
        }
        if depth >= crate::MAX_NODE_DEPTH {
            eprintln!(
                "node_search: node tree exceeded max depth {}; truncating search",
                crate::MAX_NODE_DEPTH
            );
            return None;
        }
        for c in children(n) {
            if let Some(hit) = find(c, target, depth + 1) {
                return Some(hit);
            }
        }
        None
    }
    find(doc, node_id, 0)
}

// ─────────────────────────────────────────────────────────────────────────────
// Multi-token ancestor-chain search
// ─────────────────────────────────────────────────────────────────────────────

/// Per-token attribution: which path index a token matched, and its score.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TokenMatch {
    /// The original token from the query.
    pub token: String,
    /// Index into `SearchHit::path` where the token was assigned.
    pub path_index: usize,
    /// The matched ancestor's name (kept for display so callers don't have to
    /// re-walk the path).
    pub matched_name: String,
    /// Length-normalized score (nucleo raw score divided by `sqrt(name_chars)`).
    /// Short, semantically-named ancestors outscore long descriptive text.
    pub token_score: f64,
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
///    some ancestor in the path, and each token must be assigned to a
///    **different** ancestor (otherwise one verbose name can satisfy every
///    token on its own — the failure mode that motivated this rewrite).
/// 3. Length-normalize each per-(token, ancestor) score by dividing the raw
///    nucleo score by `sqrt(name_chars)` so a 130-char prose name doesn't
///    tie a 9-char semantic name on the same query word.
/// 4. Pick the best legal assignment (distinct path index per token) that
///    maximizes the weighted sum, with `LEAF_WEIGHT_DECAY` per step away
///    from the leaf and `CONSECUTIVE_PAIR_BONUS` for adjacent-token pairs
///    that landed on adjacent path indices in query order.
/// 5. If no legal assignment exists (any token unmatched, or `tokens.len() >
///    path.len()`), drop the node.
/// 6. Apply `type_filter` post-walk: if set, drop candidates whose `type_`
///    isn't in the filter (case-insensitive).
/// 7. Sort by score descending, take top `limit`.
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
        // Distinct-ancestor constraint is infeasible if there are more tokens
        // than ancestors. The assignment bitmask is also a u64, so cap depth.
        if tokens.len() > path.len() || path.len() > 64 {
            return;
        }

        // Build candidates[t] = every (path_index, normalized_score,
        // ancestor_name) where token t fuzzy-matches the ancestor. The
        // assignment search below then picks one entry per token, no path
        // index reused.
        let mut candidates: Vec<Vec<TokenCandidate>> = Vec::with_capacity(tokens.len());
        let mut buf: Vec<char> = Vec::new();
        for pattern in &patterns {
            let mut cands: Vec<TokenCandidate> = Vec::new();
            for (pi, ancestor) in path.iter().enumerate() {
                buf.clear();
                buf.extend(ancestor.name.chars());
                if let Some(s) = pattern.score(
                    nucleo_matcher::Utf32Str::new(&ancestor.name, &mut buf),
                    &mut matcher,
                ) {
                    let name_chars = (ancestor.name.chars().count() as f64).max(1.0);
                    let normalized = (s as f64) / name_chars.sqrt();
                    cands.push(TokenCandidate {
                        path_index: pi,
                        normalized,
                        ancestor_name: ancestor.name.clone(),
                    });
                }
            }
            if cands.is_empty() {
                return;
            }
            candidates.push(cands);
        }

        let leaf_idx = path.len().saturating_sub(1);
        let Some((score, assignment)) = best_assignment(&candidates, leaf_idx) else {
            return;
        };

        let matches_out: Vec<TokenMatch> = assignment
            .iter()
            .enumerate()
            .map(|(i, c)| TokenMatch {
                token: tokens[i].to_string(),
                path_index: c.path_index,
                matched_name: c.ancestor_name.clone(),
                token_score: c.normalized,
            })
            .collect();

        hits.push(SearchHit {
            node,
            path: path.to_vec(),
            score,
            matches: matches_out,
        });
    });

    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(limit);
    hits
}

/// One candidate ancestor for a token: the path index, the length-normalized
/// nucleo score, and the ancestor's name (kept for `TokenMatch` attribution).
#[derive(Clone, Debug)]
struct TokenCandidate {
    path_index: usize,
    normalized: f64,
    ancestor_name: String,
}

/// Find the best assignment of tokens to distinct path indices that
/// maximizes `sum(normalized_score * leaf_weight) * (1 + bonus)^pairs`, where
/// `pairs` counts adjacent tokens whose assigned indices are also adjacent
/// (in query order). Returns `None` if no legal assignment exists.
///
/// For typical query shapes (≤ 5 tokens, ≤ 20 path depth) the search visits
/// well under a thousand leaves per node, so a simple backtracking solver
/// with a `u64` bitmask of used path indices is plenty fast — no Hungarian
/// algorithm needed.
fn best_assignment(
    candidates: &[Vec<TokenCandidate>],
    leaf_idx: usize,
) -> Option<(f64, Vec<TokenCandidate>)> {
    let mut best_score = f64::NEG_INFINITY;
    let mut best_pick: Option<Vec<TokenCandidate>> = None;
    let mut current: Vec<TokenCandidate> = Vec::with_capacity(candidates.len());
    recurse_assignment(
        candidates,
        leaf_idx,
        0,
        0u64,
        &mut current,
        0.0,
        0,
        &mut best_score,
        &mut best_pick,
    );
    best_pick.map(|p| (best_score, p))
}

#[allow(clippy::too_many_arguments)]
fn recurse_assignment(
    candidates: &[Vec<TokenCandidate>],
    leaf_idx: usize,
    ti: usize,
    used: u64,
    current: &mut Vec<TokenCandidate>,
    base: f64,
    bonus_count: u32,
    best: &mut f64,
    best_pick: &mut Option<Vec<TokenCandidate>>,
) {
    if ti == candidates.len() {
        let final_score = base * (1.0 + CONSECUTIVE_PAIR_BONUS).powi(bonus_count as i32);
        if final_score > *best {
            *best = final_score;
            *best_pick = Some(current.clone());
        }
        return;
    }
    for cand in &candidates[ti] {
        let bit = 1u64 << cand.path_index;
        if used & bit != 0 {
            continue;
        }
        let depth_from_leaf = leaf_idx.saturating_sub(cand.path_index);
        let weight = LEAF_WEIGHT_DECAY.powi(depth_from_leaf as i32);
        let contribution = cand.normalized * weight;
        let is_consecutive = current
            .last()
            .is_some_and(|prev| cand.path_index == prev.path_index + 1);
        let new_bonus = bonus_count + if is_consecutive { 1 } else { 0 };
        current.push(cand.clone());
        recurse_assignment(
            candidates,
            leaf_idx,
            ti + 1,
            used | bit,
            current,
            base + contribution,
            new_bonus,
            best,
            best_pick,
        );
        current.pop();
    }
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
    fn resolve_node_id_finds_deep_node() {
        let d = doc();
        let n = resolve_node_id(&d, "1:5").unwrap();
        assert_eq!(
            n.get("name").and_then(|v| v.as_str()),
            Some("Primary Button")
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // multi_token_search
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
            assert_ne!(
                h.node.id, "2:2",
                "Sidebar button leaked despite missing 'filter'"
            );
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
        let hits = multi_token_search(&d, &["wallchart", "grid", "filter"], Some(&["TEXT"]), 10);
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

    /// A single leaf whose name contains every query word must not be a hit:
    /// the distinct-ancestor constraint refuses to bind all tokens to the
    /// same path index, even when nucleo would happily score each one on
    /// that one verbose name. This is the failure mode that motivated the
    /// rewrite (long designer notes that happened to contain every keyword).
    #[test]
    fn multi_token_rejects_single_name_satisfying_all_tokens() {
        let doc = cache_node(
            "0:0",
            "doc",
            "DOCUMENT",
            vec![cache_node(
                "1:0",
                "Page",
                "CANVAS",
                vec![cache_leaf(
                    "1:1",
                    "the wallchart filter and the widget label",
                    "TEXT",
                )],
            )],
        );
        // Ancestors {doc, Page} don't contain "wallchart", "filter", or
        // "widget" — those words live solely on the leaf. All three tokens
        // want the same path index, which the distinct constraint forbids.
        let hits = multi_token_search(&doc, &["wallchart", "filter", "widget"], None, 10);
        assert!(
            hits.iter().all(|h| h.node.id != "1:1"),
            "leaf 1:1 leaked despite no distinct-ancestor assignment: {:?}",
            hits.iter().map(|h| &h.node.id).collect::<Vec<_>>()
        );
    }

    /// More tokens than the path can hold → no hit possible. Without this
    /// short-circuit the assignment search would still iterate through every
    /// permutation and reject them one by one (slow on dense trees).
    #[test]
    fn multi_token_returns_empty_when_tokens_exceed_depth() {
        // doc > Page > Leaf — depth 3. Query has 5 tokens. Impossible.
        let doc = cache_node(
            "0:0",
            "doc",
            "DOCUMENT",
            vec![cache_node(
                "1:0",
                "Page",
                "CANVAS",
                vec![cache_leaf("1:1", "Leaf", "FRAME")],
            )],
        );
        let hits = multi_token_search(&doc, &["doc", "page", "leaf", "extra", "more"], None, 10);
        assert!(hits.is_empty());
    }

    /// Length normalization: a short semantic name (e.g. "Employees") must
    /// outrank a long descriptive name that happens to contain the same
    /// word as a substring (e.g. "list of employees here"). Both nucleo
    /// scores are similar; the sqrt(name_chars) divisor breaks the tie.
    #[test]
    fn multi_token_length_normalization_prefers_short_names() {
        // Two parallel branches under the same root. Both leaves match the
        // single-token query "employees", but one is the short word and one
        // is a long prose sentence containing it.
        let doc = cache_node(
            "0:0",
            "doc",
            "DOCUMENT",
            vec![cache_node(
                "1:0",
                "Page",
                "CANVAS",
                vec![
                    cache_leaf("1:1", "Employees", "FRAME"),
                    cache_leaf(
                        "1:2",
                        "the list of employees and their roles in the company",
                        "TEXT",
                    ),
                ],
            )],
        );
        let hits = multi_token_search(&doc, &["employees"], None, 5);
        assert!(!hits.is_empty(), "expected at least one hit");
        assert_eq!(
            hits[0].node.id,
            "1:1",
            "short 'Employees' frame should outrank the long prose; got {:?}",
            hits.iter()
                .map(|h| (&h.node.id, h.score))
                .collect::<Vec<_>>()
        );
    }
}

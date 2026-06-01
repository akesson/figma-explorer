//! Associate Figma comments with cached tree nodes.
//!
//! Comments come in four anchor flavors (`CommentClientMeta`):
//! - `FrameOffset` / `FrameOffsetRegion` — explicit `node_id`, resolved by index lookup.
//! - `Vector` — absolute canvas point. Resolved geometrically: deepest
//!   containing visible node → nearest within `threshold_px` → canvas-level.
//! - `Region` — absolute canvas rect. Resolved by best IoU against visible
//!   node bounds (floor 0.05); falls back to the rect's centroid as a Vector.
//!
//! Replies (`parent_id` set) inherit their parent's resolution so an entire
//! thread anchors together. Order is preserved: top-level first, then replies.
//!
//! Pure logic, no I/O. The caller owns the tree + comments and just hands them
//! in; we return a `Vec<AssociatedComment>` ready to render.

use std::collections::HashMap;

use figma_api::models::{Comment, CommentClientMeta};
use serde::{Deserialize, Serialize};

use crate::cache::CacheNode;
use crate::geometry::{area, contains_point, dist_to_rect, iou};
use crate::node::Bounds;

/// Default IoU floor for Region anchors. Below this we treat the region as
/// "didn't really overlap anything" and fall back to centroid lookup.
const REGION_IOU_FLOOR: f64 = 0.05;

/// Default nearest-neighbor threshold (canvas units) for Vector / Region
/// centroid pins. Baked at cache-write time — associations are precomputed in
/// the sidecar so no query-time tuning happens.
pub const DEFAULT_ASSOC_THRESHOLD_PX: f64 = 50.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssociatedComment {
    pub comment_id: String,
    pub message: String,
    pub author: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    pub reactions: usize,
    pub anchor: Anchor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeRef>,
    pub method: AssociationMethod,
    /// Set when the comment carries an explicit `node_id` that no longer
    /// exists in the cached tree (the node was deleted or moved).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_node_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anchor {
    pub kind: AnchorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explicit_node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canvas_point: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canvas_rect: Option<[f64; 4]>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorKind {
    Vector,
    FrameOffset,
    Region,
    FrameOffsetRegion,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRef {
    pub node_id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub name: String,
    pub path: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssociationMethod {
    Explicit,
    Containing,
    Nearest { distance_px: f64 },
    CanvasLevel,
}

/// Resolve every comment's anchor against the tree rooted at `root`.
///
/// `threshold_px` caps how far a `Vector` pin (or a `Region`'s centroid
/// fallback) is willing to reach for a "nearest" association. Beyond it the
/// comment is left as canvas-level.
pub fn associate(
    root: &CacheNode,
    comments: &[Comment],
    threshold_px: f64,
) -> Vec<AssociatedComment> {
    let by_id = build_index(root);

    // Resolve top-level first so replies can inherit. Two-pass to avoid
    // assuming the API returns parents before replies.
    let mut resolved_by_id: HashMap<&str, ResolvedAnchor> = HashMap::new();
    for c in comments.iter().filter(|c| c.parent_id.is_none()) {
        let r = resolve_anchor(c, &by_id, threshold_px);
        resolved_by_id.insert(c.id.as_str(), r);
    }

    // Replies: inherit from parent if present, otherwise resolve the reply's
    // own anchor (defensive — Figma's API hasn't been observed to return
    // orphan replies, but we don't want to panic if it ever does).
    let mut out: Vec<AssociatedComment> = Vec::with_capacity(comments.len());
    for c in comments {
        let resolved = if let Some(parent_id) = c.parent_id.as_deref() {
            resolved_by_id
                .get(parent_id)
                .cloned()
                .unwrap_or_else(|| resolve_anchor(c, &by_id, threshold_px))
        } else {
            resolved_by_id
                .get(c.id.as_str())
                .cloned()
                .expect("top-level entries seeded above")
        };
        out.push(materialize(c, resolved));
    }
    out
}

// ─── internals ────────────────────────────────────────────────────────────

struct IndexEntry<'a> {
    node: &'a CacheNode,
    /// Ancestor names from root → this node, *excluding* this node itself.
    path: Vec<String>,
}

#[derive(Clone)]
struct ResolvedAnchor {
    node: Option<NodeRef>,
    method: AssociationMethod,
    stale_node_id: Option<String>,
}

/// Walk the tree once, recording every node by id along with the path of
/// ancestor names. Includes invisible nodes (lookup callers filter as needed)
/// so that explicit-id resolution still finds a hidden frame's pin.
fn build_index(root: &CacheNode) -> HashMap<&str, IndexEntry<'_>> {
    let mut out: HashMap<&str, IndexEntry<'_>> = HashMap::new();
    let mut path_names: Vec<String> = Vec::new();
    walk(root, &mut path_names, &mut out);
    out
}

// Unbounded recursion is safe: `CacheNode` trees come through
// `cache::project_to_cache`, which caps depth at `MAX_NODE_DEPTH`.
fn walk<'a>(
    node: &'a CacheNode,
    path_names: &mut Vec<String>,
    out: &mut HashMap<&'a str, IndexEntry<'a>>,
) {
    if !node.id.is_empty() {
        out.insert(
            node.id.as_str(),
            IndexEntry {
                node,
                path: path_names.clone(),
            },
        );
    }
    let push = !node.name.is_empty();
    if push {
        path_names.push(node.name.clone());
    }
    for child in &node.children {
        walk(child, path_names, out);
    }
    if push {
        path_names.pop();
    }
}

fn resolve_anchor(
    c: &Comment,
    by_id: &HashMap<&str, IndexEntry<'_>>,
    threshold_px: f64,
) -> ResolvedAnchor {
    match c.client_meta.as_ref() {
        CommentClientMeta::FrameOffset(fo) => resolve_explicit(&fo.node_id, by_id),
        CommentClientMeta::FrameOffsetRegion(fr) => resolve_explicit(&fr.node_id, by_id),
        CommentClientMeta::Vector(v) => resolve_point(v.x, v.y, by_id, threshold_px),
        CommentClientMeta::Region(r) => {
            let rect = Bounds {
                x: r.x,
                y: r.y,
                width: r.region_width,
                height: r.region_height,
            };
            resolve_region(&rect, by_id, threshold_px)
        }
    }
}

fn resolve_explicit(node_id: &str, by_id: &HashMap<&str, IndexEntry<'_>>) -> ResolvedAnchor {
    match by_id.get(node_id) {
        Some(entry) => ResolvedAnchor {
            node: Some(node_ref_from(entry)),
            method: AssociationMethod::Explicit,
            stale_node_id: None,
        },
        None => ResolvedAnchor {
            node: None,
            method: AssociationMethod::CanvasLevel,
            stale_node_id: Some(node_id.to_owned()),
        },
    }
}

fn resolve_point(
    x: f64,
    y: f64,
    by_id: &HashMap<&str, IndexEntry<'_>>,
    threshold_px: f64,
) -> ResolvedAnchor {
    let mut best_contain: Option<(f64, &IndexEntry<'_>)> = None;
    let mut best_nearest: Option<(f64, &IndexEntry<'_>)> = None;
    for entry in by_id.values() {
        if !entry.node.visible {
            continue;
        }
        let Some(b) = entry.node.bounds.as_ref() else {
            continue;
        };
        // Tie-breaks compare node id so the winner is stable across runs —
        // `by_id` is a HashMap and its iteration order is unspecified, so a
        // bare `>=`/`<=` would pick an arbitrary node when two candidates have
        // equal area (e.g. a frame and its full-bleed child) or equal distance.
        if contains_point(b, x, y) {
            let a = area(b); // smallest containing node wins
            let better = match best_contain {
                None => true,
                Some((ba, be)) => a < ba || (a == ba && entry.node.id < be.node.id),
            };
            if better {
                best_contain = Some((a, entry));
            }
        } else {
            let d = dist_to_rect(b, x, y); // nearest node wins
            let better = match best_nearest {
                None => true,
                Some((bd, be)) => d < bd || (d == bd && entry.node.id < be.node.id),
            };
            if better {
                best_nearest = Some((d, entry));
            }
        }
    }
    if let Some((_, entry)) = best_contain {
        return ResolvedAnchor {
            node: Some(node_ref_from(entry)),
            method: AssociationMethod::Containing,
            stale_node_id: None,
        };
    }
    if let Some((d, entry)) = best_nearest {
        if d <= threshold_px {
            return ResolvedAnchor {
                node: Some(node_ref_from(entry)),
                method: AssociationMethod::Nearest { distance_px: d },
                stale_node_id: None,
            };
        }
    }
    ResolvedAnchor {
        node: None,
        method: AssociationMethod::CanvasLevel,
        stale_node_id: None,
    }
}

fn resolve_region(
    rect: &Bounds,
    by_id: &HashMap<&str, IndexEntry<'_>>,
    threshold_px: f64,
) -> ResolvedAnchor {
    let mut best: Option<(f64, &IndexEntry<'_>)> = None;
    for entry in by_id.values() {
        if !entry.node.visible {
            continue;
        }
        let Some(b) = entry.node.bounds.as_ref() else {
            continue;
        };
        let score = iou(b, rect);
        match best {
            Some((bs, _)) if score <= bs => {}
            _ => best = Some((score, entry)),
        }
    }
    if let Some((score, entry)) = best {
        if score >= REGION_IOU_FLOOR {
            return ResolvedAnchor {
                node: Some(node_ref_from(entry)),
                method: AssociationMethod::Containing,
                stale_node_id: None,
            };
        }
    }
    // Centroid fallback — Region effectively becomes a Vector pin.
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    resolve_point(cx, cy, by_id, threshold_px)
}

fn node_ref_from(entry: &IndexEntry<'_>) -> NodeRef {
    NodeRef {
        node_id: entry.node.id.clone(),
        type_: entry.node.type_.clone(),
        name: entry.node.name.clone(),
        path: entry.path.clone(),
    }
}

fn materialize(c: &Comment, resolved: ResolvedAnchor) -> AssociatedComment {
    AssociatedComment {
        comment_id: c.id.clone(),
        message: c.message.clone(),
        author: c.user.handle.clone(),
        created_at: c.created_at.clone(),
        resolved_at: c.resolved_at.clone().and_then(|outer| outer),
        parent_id: c.parent_id.clone(),
        order_id: c.order_id.clone(),
        reactions: c.reactions.len(),
        anchor: anchor_of(c),
        node: resolved.node,
        method: resolved.method,
        stale_node_id: resolved.stale_node_id,
    }
}

fn anchor_of(c: &Comment) -> Anchor {
    match c.client_meta.as_ref() {
        CommentClientMeta::Vector(v) => Anchor {
            kind: AnchorKind::Vector,
            explicit_node_id: None,
            canvas_point: Some([v.x, v.y]),
            canvas_rect: None,
        },
        CommentClientMeta::FrameOffset(fo) => Anchor {
            kind: AnchorKind::FrameOffset,
            explicit_node_id: Some(fo.node_id.clone()),
            canvas_point: None,
            canvas_rect: None,
        },
        CommentClientMeta::Region(r) => Anchor {
            kind: AnchorKind::Region,
            explicit_node_id: None,
            canvas_point: None,
            canvas_rect: Some([r.x, r.y, r.region_width, r.region_height]),
        },
        CommentClientMeta::FrameOffsetRegion(fr) => Anchor {
            kind: AnchorKind::FrameOffsetRegion,
            explicit_node_id: Some(fr.node_id.clone()),
            canvas_point: None,
            canvas_rect: Some([0.0, 0.0, fr.region_width, fr.region_height]),
        },
    }
}

// ─── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use figma_api::models::{
        FrameOffset as ApiFrameOffset, Reaction, Region as ApiRegion, User, Vector as ApiVector,
    };

    fn node(
        id: &str,
        type_: &str,
        name: &str,
        bounds: Option<Bounds>,
        children: Vec<CacheNode>,
    ) -> CacheNode {
        CacheNode {
            id: id.to_owned(),
            type_: type_.to_owned(),
            name: name.to_owned(),
            visible: true,
            bounds,
            children,
        }
    }

    fn b(x: f64, y: f64, w: f64, h: f64) -> Bounds {
        Bounds {
            x,
            y,
            width: w,
            height: h,
        }
    }

    fn user() -> Box<User> {
        Box::new(User::new("u1".into(), "Alice".into(), String::new()))
    }

    fn make_comment(
        id: &str,
        client_meta: CommentClientMeta,
        parent_id: Option<&str>,
        message: &str,
    ) -> Comment {
        Comment {
            id: id.to_owned(),
            client_meta: Box::new(client_meta),
            file_key: "f".into(),
            parent_id: parent_id.map(str::to_owned),
            user: user(),
            created_at: "2026-01-01T00:00:00Z".into(),
            resolved_at: None,
            message: message.to_owned(),
            order_id: Some("1".into()),
            reactions: Vec::<Reaction>::new(),
        }
    }

    /// Tree:
    ///   root (FRAME, 0..0 — no bounds intentionally to mimic DOCUMENT)
    ///     page (CANVAS, 0..0 — no bounds)
    ///       outer (FRAME 0,0 200x200)
    ///         inner (FRAME 50,50 100x100)
    ///           leaf (RECTANGLE 60,60 30x30)
    fn fixture_tree() -> CacheNode {
        let leaf = node(
            "1:4",
            "RECTANGLE",
            "leaf",
            Some(b(60.0, 60.0, 30.0, 30.0)),
            vec![],
        );
        let inner = node(
            "1:3",
            "FRAME",
            "inner",
            Some(b(50.0, 50.0, 100.0, 100.0)),
            vec![leaf],
        );
        let outer = node(
            "1:2",
            "FRAME",
            "outer",
            Some(b(0.0, 0.0, 200.0, 200.0)),
            vec![inner],
        );
        let page = node("0:1", "CANVAS", "Page 1", None, vec![outer]);
        node("0:0", "DOCUMENT", "doc", None, vec![page])
    }

    #[test]
    fn frame_offset_uses_explicit_node_id() {
        let tree = fixture_tree();
        let c = make_comment(
            "c1",
            CommentClientMeta::FrameOffset(Box::new(ApiFrameOffset::new(
                "1:3".into(),
                ApiVector::new(5.0, 5.0),
            ))),
            None,
            "explicit pin",
        );
        let out = associate(&tree, &[c], 50.0);
        assert_eq!(out.len(), 1);
        let n = out[0].node.as_ref().expect("explicit hit");
        assert_eq!(n.node_id, "1:3");
        assert!(matches!(out[0].method, AssociationMethod::Explicit));
    }

    #[test]
    fn frame_offset_with_missing_node_falls_to_canvas_level_with_stale_id() {
        let tree = fixture_tree();
        let c = make_comment(
            "c1",
            CommentClientMeta::FrameOffset(Box::new(ApiFrameOffset::new(
                "deleted-node".into(),
                ApiVector::new(0.0, 0.0),
            ))),
            None,
            "stale",
        );
        let out = associate(&tree, &[c], 50.0);
        assert!(out[0].node.is_none());
        assert!(matches!(out[0].method, AssociationMethod::CanvasLevel));
        assert_eq!(out[0].stale_node_id.as_deref(), Some("deleted-node"));
    }

    #[test]
    fn equal_area_containers_resolve_deterministically_by_id() {
        // Two sibling frames with identical bounds both contain the pin. The
        // winner must be stable: `build_index` uses a HashMap whose iteration
        // order varies per instance, so without an id tie-break the winner
        // would flicker. Each iteration builds a fresh tree (fresh map seed),
        // so 25 runs exercise different iteration orders.
        for _ in 0..25 {
            let a = node("1:9", "FRAME", "a", Some(b(0.0, 0.0, 100.0, 100.0)), vec![]);
            let z = node("1:2", "FRAME", "z", Some(b(0.0, 0.0, 100.0, 100.0)), vec![]);
            let page = node("0:1", "CANVAS", "Page 1", None, vec![a, z]);
            let tree = node("0:0", "DOCUMENT", "doc", None, vec![page]);
            let c = make_comment(
                "c1",
                CommentClientMeta::Vector(Box::new(ApiVector::new(50.0, 50.0))),
                None,
                "pin",
            );
            let out = associate(&tree, &[c], 50.0);
            let n = out[0].node.as_ref().expect("containing hit");
            assert_eq!(n.node_id, "1:2", "tie must resolve to the smaller node id");
            assert!(matches!(out[0].method, AssociationMethod::Containing));
        }
    }

    #[test]
    fn vector_picks_deepest_containing_node() {
        let tree = fixture_tree();
        // (75, 75) is inside outer, inner, and leaf — leaf is smallest.
        let c = make_comment(
            "c1",
            CommentClientMeta::Vector(Box::new(ApiVector::new(75.0, 75.0))),
            None,
            "pin",
        );
        let out = associate(&tree, &[c], 50.0);
        let n = out[0].node.as_ref().expect("contained");
        assert_eq!(n.node_id, "1:4", "expected leaf, got {}", n.node_id);
        assert!(matches!(out[0].method, AssociationMethod::Containing));
        assert_eq!(n.path, vec!["doc", "Page 1", "outer", "inner"]);
    }

    #[test]
    fn vector_falls_back_to_nearest_inside_threshold() {
        let tree = fixture_tree();
        // outer ends at x=200. (240, 100) is 40px right — within 50px threshold.
        let c = make_comment(
            "c1",
            CommentClientMeta::Vector(Box::new(ApiVector::new(240.0, 100.0))),
            None,
            "pin",
        );
        let out = associate(&tree, &[c], 50.0);
        let n = out[0].node.as_ref().expect("nearest hit");
        // outer (200x200) is the closest visible node with bounds.
        assert_eq!(n.node_id, "1:2");
        match &out[0].method {
            AssociationMethod::Nearest { distance_px } => {
                assert!((distance_px - 40.0).abs() < 1e-9, "got {distance_px}");
            }
            other => panic!("expected Nearest, got {other:?}"),
        }
    }

    #[test]
    fn vector_beyond_threshold_is_canvas_level() {
        let tree = fixture_tree();
        // 60px right of outer's edge with threshold 50 — should go canvas-level.
        let c = make_comment(
            "c1",
            CommentClientMeta::Vector(Box::new(ApiVector::new(260.0, 100.0))),
            None,
            "far",
        );
        let out = associate(&tree, &[c], 50.0);
        assert!(out[0].node.is_none());
        assert!(matches!(out[0].method, AssociationMethod::CanvasLevel));
    }

    #[test]
    fn nearest_threshold_boundary_inclusive() {
        let tree = fixture_tree();
        // exactly 49 px from outer → Nearest
        let c1 = make_comment(
            "c1",
            CommentClientMeta::Vector(Box::new(ApiVector::new(249.0, 100.0))),
            None,
            "49",
        );
        // exactly 51 px from outer → CanvasLevel
        let c2 = make_comment(
            "c2",
            CommentClientMeta::Vector(Box::new(ApiVector::new(251.0, 100.0))),
            None,
            "51",
        );
        let out = associate(&tree, &[c1, c2], 50.0);
        assert!(matches!(out[0].method, AssociationMethod::Nearest { .. }));
        assert!(matches!(out[1].method, AssociationMethod::CanvasLevel));
    }

    #[test]
    fn region_uses_best_iou() {
        let tree = fixture_tree();
        // Region exactly matches inner's bounds (100x100 @ 50,50) → IoU = 1.0
        // against inner. outer's IoU is 100²/200²=0.25, leaf's is 900/10000=0.09.
        // inner must win.
        let c = make_comment(
            "c1",
            CommentClientMeta::Region(Box::new(ApiRegion::new(50.0, 50.0, 100.0, 100.0))),
            None,
            "region",
        );
        let out = associate(&tree, &[c], 50.0);
        let n = out[0].node.as_ref().expect("iou hit");
        assert_eq!(n.node_id, "1:3", "expected inner, got {}", n.node_id);
        assert!(matches!(out[0].method, AssociationMethod::Containing));
    }

    #[test]
    fn region_below_floor_falls_back_to_centroid() {
        let tree = fixture_tree();
        // A tiny rect off to the side at (300, 300) won't hit the IoU floor against
        // any node. Centroid (305, 305) is well past outer (200x200) and beyond the
        // 50px threshold → canvas-level.
        let c = make_comment(
            "c1",
            CommentClientMeta::Region(Box::new(ApiRegion::new(300.0, 300.0, 10.0, 10.0))),
            None,
            "tiny",
        );
        let out = associate(&tree, &[c], 50.0);
        assert!(out[0].node.is_none());
        assert!(matches!(out[0].method, AssociationMethod::CanvasLevel));
    }

    #[test]
    fn replies_inherit_parent_association() {
        let tree = fixture_tree();
        let parent = make_comment(
            "p1",
            CommentClientMeta::FrameOffset(Box::new(ApiFrameOffset::new(
                "1:3".into(),
                ApiVector::new(0.0, 0.0),
            ))),
            None,
            "thread head",
        );
        // Reply with no anchor info that matches: still inherits parent's node.
        let reply = make_comment(
            "r1",
            CommentClientMeta::Vector(Box::new(ApiVector::new(9999.0, 9999.0))),
            Some("p1"),
            "reply",
        );
        let out = associate(&tree, &[parent, reply], 50.0);
        let reply_node = out[1].node.as_ref().expect("inherited");
        assert_eq!(reply_node.node_id, "1:3");
        assert!(matches!(out[1].method, AssociationMethod::Explicit));
    }

    #[test]
    fn invisible_nodes_are_skipped_for_geometric_lookups() {
        // Tree where the leaf would normally win, but it's hidden.
        let leaf = CacheNode {
            id: "leaf".into(),
            type_: "RECTANGLE".into(),
            name: "leaf".into(),
            visible: false,
            bounds: Some(b(60.0, 60.0, 30.0, 30.0)),
            children: vec![],
        };
        let inner = node(
            "inner",
            "FRAME",
            "inner",
            Some(b(50.0, 50.0, 100.0, 100.0)),
            vec![leaf],
        );
        let root = node(
            "root",
            "FRAME",
            "root",
            Some(b(0.0, 0.0, 200.0, 200.0)),
            vec![inner],
        );

        let c = make_comment(
            "c1",
            CommentClientMeta::Vector(Box::new(ApiVector::new(75.0, 75.0))),
            None,
            "pin",
        );
        let out = associate(&root, &[c], 50.0);
        let n = out[0].node.as_ref().expect("contained");
        assert_eq!(n.node_id, "inner", "leaf is hidden, inner should win");
    }
}

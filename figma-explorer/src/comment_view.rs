//! Shared comment rendering: `.comments.json` sidecar entries → output
//! Values. Consumed by `node-info` (comment target, recent/anchored
//! comments) and `comments` (thread listings).

use serde_json::{json, Value};

use crate::comment_assoc::AssociatedComment;
use crate::synth::SynthState;

/// Serialize one comment, injecting a paste-ready `comm_id`
/// (`file:N:comm:M`) and promoting the associated node's id to qualified
/// `file:N:<node>` form.
pub fn comment_value(file_synth: u32, synth: &SynthState, c: &AssociatedComment) -> Value {
    let mut v = serde_json::to_value(c).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut v {
        // Surface a paste-ready `file:N:comm:M` id; fall back to the Figma
        // id when we have no synth (interning failed earlier — best-effort).
        if let Some(comm) = synth.comment_synth(file_synth, &c.comment_id) {
            map.insert(
                "comm_id".into(),
                json!(format!("file:{file_synth}:comm:{comm}")),
            );
        }
        // Promote the associated node's id to qualified form.
        if let Some(node) = c.node.as_ref() {
            if let Some(node_obj) = map.get_mut("node").and_then(|n| n.as_object_mut()) {
                node_obj.insert(
                    "id".into(),
                    json!(format!("file:{file_synth}:{}", node.node_id)),
                );
            }
        }
    }
    v
}

/// The `requested` comment with its thread context attached: replies (when
/// it's a thread head) or parent + sibling replies (when it's a reply).
/// `all` is the file's full sidecar — threads are reassembled by `parent_id`.
pub fn thread_value(
    file_synth: u32,
    synth: &SynthState,
    all: &[AssociatedComment],
    requested: &AssociatedComment,
) -> Value {
    // The "thread root" is the head — either the requested entry itself
    // (when no parent) or the parent it points to.
    let head_id = requested
        .parent_id
        .clone()
        .unwrap_or_else(|| requested.comment_id.clone());

    let mut comment_obj = comment_value(file_synth, synth, requested);
    if let Value::Object(map) = &mut comment_obj {
        if requested.parent_id.is_some() {
            // Requested is a reply — surface its parent so the LLM has the
            // full context without a second lookup. Also list sibling
            // replies (same parent, but not the requested entry).
            if let Some(parent) = all.iter().find(|c| c.comment_id == head_id) {
                map.insert("parent".into(), comment_value(file_synth, synth, parent));
            }
            let siblings: Vec<Value> = all
                .iter()
                .filter(|c| {
                    c.parent_id.as_deref() == Some(head_id.as_str())
                        && c.comment_id != requested.comment_id
                })
                .map(|c| comment_value(file_synth, synth, c))
                .collect();
            if !siblings.is_empty() {
                map.insert("siblings".into(), Value::Array(siblings));
            }
        } else {
            // Requested is a thread head — surface direct replies.
            let replies: Vec<Value> = all
                .iter()
                .filter(|c| c.parent_id.as_deref() == Some(head_id.as_str()))
                .map(|c| comment_value(file_synth, synth, c))
                .collect();
            if !replies.is_empty() {
                map.insert("replies".into(), Value::Array(replies));
            }
        }
    }
    comment_obj
}

/// One comment thread: a head plus its replies, all borrowed from the
/// sidecar slice.
pub struct Thread<'a> {
    pub head: &'a AssociatedComment,
    pub replies: Vec<&'a AssociatedComment>,
}

/// Group sidecar entries into threads. Heads keep sidecar order; replies
/// attach to their head by `parent_id` (a reply appearing before its head
/// still attaches — heads are collected in a first pass) and are sorted
/// oldest-first so a thread reads in conversation order. Orphan replies
/// (`parent_id` set but absent from the sidecar — defensive, mirrors
/// `comment_assoc`'s tolerance) become standalone single-entry threads
/// appended after the real heads.
pub fn group_threads(comments: &[AssociatedComment]) -> Vec<Thread<'_>> {
    let mut threads: Vec<Thread<'_>> = Vec::new();
    let mut index_by_head_id = std::collections::HashMap::new();
    for c in comments {
        if c.parent_id.is_none() {
            index_by_head_id.insert(c.comment_id.as_str(), threads.len());
            threads.push(Thread {
                head: c,
                replies: Vec::new(),
            });
        }
    }
    for c in comments {
        if let Some(parent_id) = c.parent_id.as_deref() {
            match index_by_head_id.get(parent_id) {
                Some(&i) => threads[i].replies.push(c),
                None => threads.push(Thread {
                    head: c,
                    replies: Vec::new(),
                }),
            }
        }
    }
    for t in &mut threads {
        t.replies.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    }
    threads
}

/// The thread's "activity" timestamp: the lexicographic max `created_at`
/// across head and replies. Z-suffixed ISO-8601 sorts lexically, so no date
/// parsing is needed.
pub fn last_activity<'a>(t: &Thread<'a>) -> &'a str {
    t.replies
        .iter()
        .map(|r| r.created_at.as_str())
        .chain(std::iter::once(t.head.created_at.as_str()))
        .max()
        .unwrap_or_default()
}

/// Thread heads, newest `created_at` first (ties broken by `comment_id` for
/// determinism), capped at `n`. Used by `node-info`'s file summary.
pub fn recent_thread_heads(comments: &[AssociatedComment], n: usize) -> Vec<&AssociatedComment> {
    let mut heads: Vec<&AssociatedComment> =
        comments.iter().filter(|c| c.parent_id.is_none()).collect();
    heads.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.comment_id.cmp(&b.comment_id))
    });
    heads.truncate(n);
    heads
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comment_assoc::{Anchor, AnchorKind, AssociationMethod, NodeRef};

    fn comment(id: &str, parent: Option<&str>, created_at: &str) -> AssociatedComment {
        AssociatedComment {
            comment_id: id.into(),
            message: format!("msg {id}"),
            author: "tester".into(),
            created_at: created_at.into(),
            resolved_at: None,
            parent_id: parent.map(str::to_owned),
            order_id: None,
            reactions: 0,
            anchor: Anchor {
                kind: AnchorKind::Vector,
                explicit_node_id: None,
                canvas_point: Some([0.0, 0.0]),
                canvas_rect: None,
            },
            node: None,
            method: AssociationMethod::CanvasLevel,
            stale_node_id: None,
        }
    }

    fn anchored(id: &str, created_at: &str, node_id: &str) -> AssociatedComment {
        let mut c = comment(id, None, created_at);
        c.node = Some(NodeRef {
            node_id: node_id.into(),
            type_: "FRAME".into(),
            name: "Header".into(),
            path: vec![],
        });
        c.method = AssociationMethod::Explicit;
        c
    }

    #[test]
    fn group_threads_attaches_replies_to_heads() {
        // Replies appear *before* their head in sidecar order (Figma returns
        // newest-first) — they must still attach, sorted oldest-first.
        let all = vec![
            comment("r2", Some("h1"), "2026-01-05T00:00:00Z"),
            comment("r1", Some("h1"), "2026-01-02T00:00:00Z"),
            comment("h1", None, "2026-01-01T00:00:00Z"),
            comment("h2", None, "2026-01-03T00:00:00Z"),
        ];
        let threads = group_threads(&all);
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].head.comment_id, "h1");
        let reply_ids: Vec<&str> = threads[0]
            .replies
            .iter()
            .map(|r| r.comment_id.as_str())
            .collect();
        assert_eq!(reply_ids, ["r1", "r2"], "replies in conversation order");
        assert_eq!(threads[1].head.comment_id, "h2");
        assert!(threads[1].replies.is_empty());
    }

    #[test]
    fn group_threads_orphan_reply_becomes_standalone_thread() {
        let all = vec![
            comment("h1", None, "2026-01-01T00:00:00Z"),
            comment("r-orphan", Some("gone"), "2026-01-02T00:00:00Z"),
        ];
        let threads = group_threads(&all);
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[1].head.comment_id, "r-orphan");
    }

    #[test]
    fn last_activity_is_lexicographic_max_of_head_and_replies() {
        let all = vec![
            comment("h1", None, "2026-01-01T00:00:00Z"),
            comment("r1", Some("h1"), "2026-03-05T12:00:00Z"),
            comment("r2", Some("h1"), "2026-02-01T00:00:00Z"),
        ];
        let threads = group_threads(&all);
        assert_eq!(last_activity(&threads[0]), "2026-03-05T12:00:00Z");
        // No replies → head's own timestamp.
        let solo = group_threads(&all[..1]);
        assert_eq!(last_activity(&solo[0]), "2026-01-01T00:00:00Z");
    }

    #[test]
    fn recent_thread_heads_sorts_newest_first_and_caps() {
        let mut all = vec![
            comment("a", None, "2026-01-03T00:00:00Z"),
            comment("b", None, "2026-01-01T00:00:00Z"),
            comment("c", None, "2026-01-05T00:00:00Z"),
            comment("reply", Some("a"), "2026-01-09T00:00:00Z"), // not a head
        ];
        // Tie on timestamp — comment_id breaks it deterministically.
        all.push(comment("d", None, "2026-01-03T00:00:00Z"));
        let heads = recent_thread_heads(&all, 3);
        let ids: Vec<&str> = heads.iter().map(|c| c.comment_id.as_str()).collect();
        assert_eq!(ids, vec!["c", "a", "d"]);
    }

    #[test]
    fn comment_value_injects_comm_id_and_qualified_node_id() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = crate::cache::CacheDir::new(tmp.path());
        cache.ensure().unwrap();
        crate::synth::with_lock(&cache, |s| {
            s.intern_file("file-a");
            s.intern_comment(1, "42");
        })
        .unwrap();
        let synth = SynthState::load(&cache).unwrap();

        let c = {
            let mut c = anchored("42", "2026-01-01T00:00:00Z", "1:2");
            c.parent_id = None;
            c
        };
        let v = comment_value(1, &synth, &c);
        assert_eq!(v["comm_id"], "file:1:comm:1");
        assert_eq!(v["node"]["id"], "file:1:1:2");
        // No synth interned → comm_id simply absent, not an error.
        let unknown = anchored("777", "2026-01-01T00:00:00Z", "1:2");
        let v = comment_value(1, &synth, &unknown);
        assert!(v.get("comm_id").is_none());
    }

    #[test]
    fn thread_value_head_attaches_replies() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = crate::cache::CacheDir::new(tmp.path());
        cache.ensure().unwrap();
        let synth = SynthState::load(&cache).unwrap();

        let all = vec![
            comment("h1", None, "2026-01-01T00:00:00Z"),
            comment("r1", Some("h1"), "2026-01-02T00:00:00Z"),
            comment("h2", None, "2026-01-03T00:00:00Z"),
        ];
        let v = thread_value(1, &synth, &all, &all[0]);
        let replies = v["replies"].as_array().unwrap();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["comment_id"], "r1");
        assert!(v.get("parent").is_none());
    }

    #[test]
    fn thread_value_reply_attaches_parent_and_siblings() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = crate::cache::CacheDir::new(tmp.path());
        cache.ensure().unwrap();
        let synth = SynthState::load(&cache).unwrap();

        let all = vec![
            comment("h1", None, "2026-01-01T00:00:00Z"),
            comment("r1", Some("h1"), "2026-01-02T00:00:00Z"),
            comment("r2", Some("h1"), "2026-01-03T00:00:00Z"),
        ];
        let v = thread_value(1, &synth, &all, &all[1]);
        assert_eq!(v["parent"]["comment_id"], "h1");
        let siblings = v["siblings"].as_array().unwrap();
        assert_eq!(siblings.len(), 1);
        assert_eq!(siblings[0]["comment_id"], "r2");
        assert!(v.get("replies").is_none());
    }
}

//! `comments` — list comment threads in a file or under a node, or dump one
//! thread.
//!
//! Lane (per CLAUDE.md): *resolve, then read sidecar (with live fetch as
//! fallback/refresh)* — the same lane `node-info` uses. The listing reads the
//! `.comments.json` sidecar; `--refresh` (or a missing sidecar without
//! `--cache-only`) re-fetches just this file's comments via
//! [`cache::refresh_file_comments`] — no full `cache prefetch` needed.
//!
//! Targets:
//! - `file:N` / file URL → every thread in the file, replies inlined.
//! - `file:N:x:y`, bare node id, node URL → threads anchored in that subtree.
//! - `file:N:comm:M` → a single thread (same shape as `node-info`'s comment
//!   target).

use std::collections::BTreeSet;

use anyhow::{anyhow, bail, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::{json, Map, Value};

use crate::cache::{self, CacheDir, CacheNode, FileMeta};
use crate::comment_assoc::AssociatedComment;
use crate::comment_view::{comment_value, group_threads, last_activity, thread_value, Thread};
use crate::resolver::{parse_id, render_resolve_error, ResolvedTarget, Resolver};
use crate::synth::SynthState;
use crate::{print, Globals, Output};

/// List comment threads in a file or under a node, or dump one thread.
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Tagged or native ID, or a Figma URL. `file:N` lists every comment
    /// thread in the file (replies inline); `file:N:x:y`, a bare node id, or
    /// a node URL restricts to threads anchored in that subtree;
    /// `file:N:comm:M` dumps a single thread.
    pub id: String,

    /// Only threads that are still open (the head has no resolved_at).
    #[arg(long)]
    pub unresolved: bool,

    /// Only threads with activity at or after this ISO-8601 UTC instant.
    /// Prefixes work: `2026-06`, `2026-06-15`. A thread matches when its
    /// head OR any reply was created at/after the cutoff.
    #[arg(long, value_name = "ISO8601")]
    pub since: Option<String>,

    /// Re-fetch this file's comments from Figma and rewrite the cached
    /// sidecar before listing. Refreshes the whole file's comments whatever
    /// the target (the sidecar is file-granular). Errors under --cache-only.
    #[arg(long)]
    pub refresh: bool,

    /// Cap the number of threads shown (after the newest-first sort).
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        let format = globals.output;
        if self.refresh && globals.cache_only {
            bail!("--refresh needs a live fetch; drop --refresh or --cache-only");
        }
        if let Some(s) = self.since.as_deref() {
            if !s.starts_with(|c: char| c.is_ascii_digit()) {
                bail!("--since expects an ISO-8601 UTC instant or prefix, e.g. 2026-06-15 or 2026-06-15T09:00:00Z");
            }
        }

        let resolver = Resolver::new(globals.cache_only)?;
        let id = parse_id(&self.id).map_err(|e| anyhow!("{e}"))?;
        let target = resolver
            .resolve(cfg, &id)
            .await
            .map_err(|e| render_resolve_error(e, format))?;

        match target {
            ResolvedTarget::File { synth, meta, .. } => {
                self.list(cfg, &resolver, synth, &meta, None, format).await
            }
            ResolvedTarget::Node {
                file_synth,
                meta,
                node,
            } => {
                let scope = subtree_ids(&node);
                self.list(cfg, &resolver, file_synth, &meta, Some(&scope), format)
                    .await
            }
            ResolvedTarget::Comment {
                file_synth,
                comm_synth,
                meta,
                comment,
            } => {
                if self.unresolved || self.since.is_some() || self.limit.is_some() {
                    bail!(
                        "--unresolved/--since/--limit filter thread listings; file:{file_synth}:comm:{comm_synth} dumps a single thread — drop the flags or target the file"
                    );
                }
                self.single_thread(
                    cfg, &resolver, file_synth, comm_synth, &meta, comment, format,
                )
                .await
            }
            ResolvedTarget::Project { synth, .. } => {
                bail!("comments lists threads per file — pick a file with `figma-explorer ls proj:{synth}`")
            }
            ResolvedTarget::Root => {
                bail!("comments needs a file, node, or comment target — start from `figma-explorer ls`")
            }
        }
    }

    async fn list(
        &self,
        cfg: &Configuration,
        resolver: &Resolver,
        file_synth: u32,
        meta: &FileMeta,
        scope: Option<&BTreeSet<String>>,
        format: Output,
    ) -> Result<()> {
        let cache = resolver.cache();
        let (comments, fetched_at) = load_sidecar(
            cfg,
            cache,
            meta,
            file_synth,
            self.refresh,
            resolver.cache_only(),
        )
        .await?;
        // Load after any refresh so freshly-interned comm synths are visible.
        let synth_state = SynthState::load(cache)?;

        let threads = group_threads(&comments);
        let threads_total = threads.len();
        let unresolved_total = threads
            .iter()
            .filter(|t| t.head.resolved_at.is_none())
            .count();

        let filters = Filters {
            unresolved: self.unresolved,
            since: self.since.as_deref(),
            scope,
        };
        let matched = assemble_threads(threads, &filters);
        let matched_count = matched.len();
        let shown: Vec<&Thread<'_>> = match self.limit {
            Some(n) => matched.iter().take(n).collect(),
            None => matched.iter().collect(),
        };

        let entries: Vec<Value> = shown
            .iter()
            .map(|t| thread_entry(file_synth, &synth_state, t))
            .collect();

        let target_id = match scope {
            None => format!("file:{file_synth}"),
            Some(_) => self.id.clone(),
        };
        let filters_active = self.unresolved || self.since.is_some() || scope.is_some();

        match format {
            Output::Yaml => {
                println!(
                    "# {threads_total} threads ({unresolved_total} unresolved) — comments fetched {}",
                    fetched_at
                        .map(|t| human_age(cache::now_epoch().saturating_sub(t)))
                        .unwrap_or_else(|| "at an unknown time".to_owned()),
                );
                if filters_active {
                    println!("# {matched_count} of {threads_total} threads match the filters");
                }
                if entries.len() < matched_count {
                    println!(
                        "# showing {} of {matched_count} threads — use --limit N to see more",
                        entries.len(),
                    );
                }
                if entries.is_empty() {
                    return Ok(());
                }
                print(&json!({ "threads": entries }), format)
            }
            Output::Json => print(
                &json!({
                    "target": { "kind": if scope.is_some() { "node" } else { "file" }, "id": target_id },
                    "file": {
                        "key": meta.file_key,
                        "name": meta.name,
                        "synth": file_synth,
                        "last_modified": meta.last_modified,
                    },
                    "summary": {
                        "threads_total": threads_total,
                        "unresolved_total": unresolved_total,
                        "matched": matched_count,
                        "shown": entries.len(),
                        "comments_fetched_at_epoch": fetched_at,
                    },
                    "threads": entries,
                }),
                format,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn single_thread(
        &self,
        cfg: &Configuration,
        resolver: &Resolver,
        file_synth: u32,
        comm_synth: u32,
        meta: &FileMeta,
        resolved: AssociatedComment,
        format: Output,
    ) -> Result<()> {
        let cache = resolver.cache();
        let (all, requested) = if self.refresh {
            let (all, _meta) =
                cache::refresh_file_comments(cfg, cache, &meta.file_key, file_synth).await?;
            let requested = all
                .iter()
                .find(|c| c.comment_id == resolved.comment_id)
                .cloned()
                .ok_or_else(|| {
                    anyhow!(
                        "comment file:{file_synth}:comm:{comm_synth} no longer exists upstream (deleted?)"
                    )
                })?;
            (all, requested)
        } else {
            let all = cache.read_comments(&meta.file_key)?.unwrap_or_default();
            (all, resolved)
        };
        let synth_state = SynthState::load(cache)?;
        let comment_obj = thread_value(file_synth, &synth_state, &all, &requested);
        print(
            &json!({
                "target": {
                    "kind": "comment",
                    "id": format!("file:{file_synth}:comm:{comm_synth}"),
                    "path": Value::Array(Vec::new()),
                },
                "file": {
                    "key": meta.file_key,
                    "name": meta.name,
                    "synth": file_synth,
                    "last_modified": meta.last_modified,
                },
                "comment": comment_obj,
            }),
            format,
        )
    }
}

/// Read the sidecar, refreshing it live when asked (`--refresh`) or when it
/// is absent/stale-schema (`read_comments` collapses both to `None`) and
/// `--cache-only` permits. Returns the comments plus the fetch epoch for the
/// freshness header.
async fn load_sidecar(
    cfg: &Configuration,
    cache: &CacheDir,
    meta: &FileMeta,
    file_synth: u32,
    refresh: bool,
    cache_only: bool,
) -> Result<(Vec<AssociatedComment>, Option<u64>)> {
    if refresh {
        let (comments, meta) =
            cache::refresh_file_comments(cfg, cache, &meta.file_key, file_synth).await?;
        return Ok((comments, meta.comments_fetched_at_epoch));
    }
    match cache.read_comments(&meta.file_key)? {
        Some(comments) => Ok((comments, meta.comments_fetched_at_epoch)),
        None if cache_only => bail!(
            "no comments sidecar for {} (and --cache-only is set); run `figma-explorer cache prefetch` to populate the local cache, then retry",
            meta.file_key
        ),
        None => {
            let (comments, meta) =
                cache::refresh_file_comments(cfg, cache, &meta.file_key, file_synth).await?;
            Ok((comments, meta.comments_fetched_at_epoch))
        }
    }
}

/// Every node id in the subtree rooted at `node` (inclusive) — the anchor
/// scope for node-targeted listings. Same collection `node-info` uses for
/// its inline anchored comments.
fn subtree_ids(node: &CacheNode) -> BTreeSet<String> {
    fn collect(n: &CacheNode, ids: &mut BTreeSet<String>) {
        ids.insert(n.id.clone());
        for c in &n.children {
            collect(c, ids);
        }
    }
    let mut ids = BTreeSet::new();
    collect(node, &mut ids);
    ids
}

struct Filters<'a> {
    unresolved: bool,
    since: Option<&'a str>,
    scope: Option<&'a BTreeSet<String>>,
}

/// Filter + sort threads: subtree scope (head's anchor must lie in `scope`;
/// canvas-level threads are excluded when scoped), unresolved-only, and
/// since (thread activity — head or any reply — at/after the cutoff,
/// compared lexicographically on ISO-8601 Z timestamps). Sorted
/// newest-activity-first, ties broken by comment id for determinism.
fn assemble_threads<'a>(threads: Vec<Thread<'a>>, f: &Filters<'_>) -> Vec<Thread<'a>> {
    let mut kept: Vec<Thread<'a>> = threads
        .into_iter()
        .filter(|t| {
            if let Some(scope) = f.scope {
                let anchored_in_scope = t
                    .head
                    .node
                    .as_ref()
                    .is_some_and(|n| scope.contains(&n.node_id));
                if !anchored_in_scope {
                    return false;
                }
            }
            if f.unresolved && t.head.resolved_at.is_some() {
                return false;
            }
            if let Some(since) = f.since {
                if last_activity(t) < since {
                    return false;
                }
            }
            true
        })
        .collect();
    kept.sort_by(|a, b| {
        last_activity(b)
            .cmp(last_activity(a))
            .then_with(|| a.head.comment_id.cmp(&b.head.comment_id))
    });
    kept
}

/// One listing row: the head via [`comment_value`] slimmed of anchor
/// internals (`anchor`, `method`, `stale_node_id` — available via
/// `node-info`/the comm target when needed), plus `last_activity`,
/// `reply_count`, and inlined `replies` (each slimmed further — replies
/// inherit the head's anchor by construction).
fn thread_entry(file_synth: u32, synth: &SynthState, t: &Thread<'_>) -> Value {
    const ANCHOR_INTERNALS: &[&str] = &["anchor", "method", "stale_node_id"];

    let mut head = comment_value(file_synth, synth, t.head);
    if let Value::Object(map) = &mut head {
        for k in ANCHOR_INTERNALS {
            map.remove(*k);
        }
        map.insert("last_activity".into(), json!(last_activity(t)));
        map.insert("reply_count".into(), json!(t.replies.len()));
        if !t.replies.is_empty() {
            let replies: Vec<Value> = t
                .replies
                .iter()
                .map(|r| {
                    let mut v = comment_value(file_synth, synth, r);
                    if let Value::Object(m) = &mut v {
                        for k in ANCHOR_INTERNALS {
                            m.remove(*k);
                        }
                        m.remove("node");
                        m.remove("parent_id");
                    }
                    v
                })
                .collect();
            map.insert("replies".into(), Value::Array(replies));
        }
    } else {
        head = Value::Object(Map::new());
    }
    head
}

/// Coarse human-readable age, e.g. `just now`, `12m ago`, `3h ago`, `2d ago`.
/// (Same buckets as `library search`'s catalog header.)
fn human_age(secs: u64) -> String {
    if secs < 60 {
        "just now".to_owned()
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
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

    fn anchored_to(mut c: AssociatedComment, node_id: &str) -> AssociatedComment {
        c.node = Some(NodeRef {
            node_id: node_id.into(),
            type_: "FRAME".into(),
            name: "Header".into(),
            path: vec![],
        });
        c.method = AssociationMethod::Explicit;
        c
    }

    fn resolved(mut c: AssociatedComment, at: &str) -> AssociatedComment {
        c.resolved_at = Some(at.into());
        c
    }

    fn no_filters() -> Filters<'static> {
        Filters {
            unresolved: false,
            since: None,
            scope: None,
        }
    }

    fn ids<'a>(threads: &'a [Thread<'a>]) -> Vec<&'a str> {
        threads.iter().map(|t| t.head.comment_id.as_str()).collect()
    }

    #[test]
    fn unresolved_filter_keeps_open_heads_only() {
        let all = vec![
            resolved(
                comment("closed", None, "2026-01-01T00:00:00Z"),
                "2026-01-02T00:00:00Z",
            ),
            comment("open", None, "2026-01-01T00:00:00Z"),
        ];
        let f = Filters {
            unresolved: true,
            ..no_filters()
        };
        assert_eq!(ids(&assemble_threads(group_threads(&all), &f)), ["open"]);
    }

    #[test]
    fn since_matches_head_or_reply_activity() {
        let all = vec![
            // Old head, no replies → excluded.
            comment("old", None, "2026-05-01T00:00:00Z"),
            // Old head with a June reply → included (activity).
            comment("revived", None, "2026-05-01T00:00:00Z"),
            comment("r1", Some("revived"), "2026-06-02T00:00:00Z"),
            // June head → included.
            comment("fresh", None, "2026-06-15T00:00:00Z"),
        ];
        let f = Filters {
            since: Some("2026-06"),
            ..no_filters()
        };
        assert_eq!(
            ids(&assemble_threads(group_threads(&all), &f)),
            ["fresh", "revived"]
        );
    }

    #[test]
    fn since_prefix_compares_lexicographically() {
        let all = vec![comment("t", None, "2026-06-15T00:00:01Z")];
        let f = Filters {
            since: Some("2026-06-15"),
            ..no_filters()
        };
        assert_eq!(assemble_threads(group_threads(&all), &f).len(), 1);
        let f = Filters {
            since: Some("2026-06-16"),
            ..no_filters()
        };
        assert!(assemble_threads(group_threads(&all), &f).is_empty());
    }

    #[test]
    fn threads_sorted_newest_activity_first_with_deterministic_ties() {
        let all = vec![
            comment("b", None, "2026-01-01T00:00:00Z"),
            comment("a", None, "2026-01-01T00:00:00Z"),
            comment("newest", None, "2026-02-01T00:00:00Z"),
        ];
        assert_eq!(
            ids(&assemble_threads(group_threads(&all), &no_filters())),
            ["newest", "a", "b"]
        );
    }

    #[test]
    fn scope_excludes_other_subtrees_and_canvas_level() {
        let all = vec![
            anchored_to(comment("in", None, "2026-01-01T00:00:00Z"), "1:2"),
            anchored_to(comment("out", None, "2026-01-01T00:00:00Z"), "9:9"),
            comment("canvas-level", None, "2026-01-01T00:00:00Z"),
        ];
        let scope: BTreeSet<String> = ["1:1", "1:2"].iter().map(|s| s.to_string()).collect();
        let f = Filters {
            scope: Some(&scope),
            ..no_filters()
        };
        assert_eq!(ids(&assemble_threads(group_threads(&all), &f)), ["in"]);
    }

    #[test]
    fn thread_entry_drops_anchor_internals_keeps_full_message_and_replies() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = crate::cache::CacheDir::new(tmp.path());
        cache.ensure().unwrap();
        let synth = SynthState::load(&cache).unwrap();

        let all = vec![
            anchored_to(comment("h", None, "2026-01-01T00:00:00Z"), "1:2"),
            comment("r", Some("h"), "2026-02-01T00:00:00Z"),
        ];
        let threads = group_threads(&all);
        let v = thread_entry(7, &synth, &threads[0]);
        assert!(v.get("anchor").is_none());
        assert!(v.get("method").is_none());
        assert_eq!(v["message"], "msg h");
        assert_eq!(v["node"]["id"], "file:7:1:2");
        assert_eq!(v["last_activity"], "2026-02-01T00:00:00Z");
        assert_eq!(v["reply_count"], 1);
        let reply = &v["replies"][0];
        assert_eq!(reply["comment_id"], "r");
        assert!(reply.get("node").is_none());
        assert!(reply.get("anchor").is_none());
    }

    #[tokio::test]
    async fn refresh_rejected_under_cache_only() {
        let args = Args {
            id: "file:1".into(),
            unresolved: false,
            since: None,
            refresh: true,
            limit: None,
        };
        let globals = Globals {
            output: Output::Yaml,
            cache_only: true,
            scope: None,
        };
        // Bails before touching the resolver or the network.
        let err = args
            .run(
                &figma_api::apis::configuration::Configuration::new(),
                &globals,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("--refresh"), "got: {err}");
    }

    #[tokio::test]
    async fn non_iso_since_rejected() {
        let args = Args {
            id: "file:1".into(),
            unresolved: false,
            since: Some("last tuesday".into()),
            refresh: false,
            limit: None,
        };
        let globals = Globals {
            output: Output::Yaml,
            cache_only: true,
            scope: None,
        };
        let err = args
            .run(
                &figma_api::apis::configuration::Configuration::new(),
                &globals,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ISO-8601"), "got: {err}");
    }

    #[tokio::test]
    async fn load_sidecar_cache_only_missing_errors_with_prefetch_hint() {
        use crate::cache::{build_cached_file, FileRef};
        let tmp = tempfile::tempdir().unwrap();
        let cache = crate::cache::CacheDir::new(tmp.path());
        cache.ensure().unwrap();
        let doc = serde_json::json!({ "id": "0:0", "name": "doc", "type": "DOCUMENT" });
        let fref = FileRef {
            file_key: "file-a".into(),
            name: "A".into(),
            last_modified: "2024-01-01".into(),
            project_id: "p1".into(),
            project_name: "P".into(),
        };
        let payload = build_cached_file(&fref, &doc, 0);
        let meta = FileMeta::from_success(&fref, &payload, 0, 0);

        let cfg = figma_api::apis::configuration::Configuration::new();
        let err = load_sidecar(&cfg, &cache, &meta, 1, false, true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cache prefetch"), "got: {err}");
    }

    #[test]
    fn subtree_ids_collects_inclusive() {
        let node = CacheNode {
            id: "1:1".into(),
            type_: "FRAME".into(),
            name: "root".into(),
            visible: true,
            bounds: None,
            characters: None,
            children: vec![CacheNode {
                id: "1:2".into(),
                type_: "TEXT".into(),
                name: "leaf".into(),
                visible: true,
                bounds: None,
                characters: None,
                children: vec![],
            }],
        };
        let ids = subtree_ids(&node);
        assert!(ids.contains("1:1") && ids.contains("1:2"));
        assert_eq!(ids.len(), 2);
    }
}

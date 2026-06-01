//! `node-info` — comprehensive single-target view for Claude Code agents.
//!
//! Takes any tagged id (`file:N`, `file:N:x:y`, `file:N:comm:M`), a bare
//! native node id, a project synth, or a Figma URL. The output is shaped
//! for an LLM that needs to implement the design in code:
//!
//! - **Node target** — curated visual + layout + text + component metadata,
//!   with referenced variables and named styles hoisted to top-level blocks
//!   so they aren't duplicated across descendants.
//! - **Comment target** (`file:N:comm:M`) — the comment, its anchor, the
//!   associated node (already pre-resolved in the sidecar), and replies.
//! - **File / Project / Root targets** — summary views.
//!
//! Data comes from `.full.json.gz` (raw `/v1/files/{key}` body) and
//! `.variables.json` sidecars written by `cache prefetch`. With
//! `--cache-only`, a missing sidecar errors with a "run cache prefetch"
//! hint. Without it, `node-info` opportunistically refetches and writes the
//! sidecar so the next call is offline.

use std::collections::BTreeSet;

use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::{json, Map, Value};

use crate::cache::{self, CacheDir, CacheNode, EntryStatus, FileMeta};
use crate::comment_assoc::AssociatedComment;
use crate::file_summary::build_file_summary;
use crate::full_cache;
use crate::node_search::resolve_node_id;
use crate::node_view::{
    build_node_view, build_styles_index_block, build_variables_block, Collector, ViewOptions,
};
use crate::resolver::{parse_id, render_resolve_error, ResolvedTarget, Resolver};
use crate::synth::SynthState;
use crate::{print, Globals};

/// Default `--max-nodes` cap. Picked so card grids and section overviews
/// render fully while pathological mega-frames trigger truncation rather
/// than dumping tens of thousands of nodes.
const DEFAULT_MAX_NODES: usize = 500;

/// How many recent comments to include in a file-target summary.
const FILE_RECENT_COMMENTS: usize = 10;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Tagged or native ID, or a Figma URL. Omit to list cached projects
    /// (root view), same convention `ls` uses.
    pub id: Option<String>,

    /// Subtree depth limit. Default: unlimited (capped by --max-nodes).
    /// `0` = target only, no children. Pass `--no-children` for the same.
    #[arg(long)]
    pub depth: Option<usize>,

    /// Shorthand for `--depth 0`.
    #[arg(long)]
    pub no_children: bool,

    /// Hard cap on emitted descendant nodes. When hit, the rest are listed
    /// under a top-level `truncated` block so consumers know more exists.
    #[arg(long, default_value_t = DEFAULT_MAX_NODES)]
    pub max_nodes: usize,

    /// Include `prototype` block (interactions/transitions). Off by default.
    #[arg(long)]
    pub prototype: bool,

    /// Include `dev_status`, `annotations`, `export_settings`. Off by default.
    #[arg(long)]
    pub meta: bool,

    /// Include per-text-range overrides on TEXT nodes (`characterStyleOverrides`
    /// + `styleOverrideTable`). Off by default — overrides are noise unless
    ///   you specifically need the inline link / colored span data.
    #[arg(long)]
    pub rich_text: bool,

    /// Skip the resolved top-level `variables` block.
    #[arg(long)]
    pub no_variables: bool,

    /// Skip anchored comments on the node.
    #[arg(long)]
    pub no_comments: bool,

    /// Emit the raw Figma JSON for the target node (camelCase, untouched).
    /// Useful when the curated view drops a field you need.
    #[arg(long)]
    pub raw: bool,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        let format = globals.output;
        let resolver = Resolver::new(globals.cache_only)?;

        let target = match self.id.as_deref() {
            None => ResolvedTarget::Root,
            Some(s) => {
                let id = parse_id(s).map_err(|e| anyhow!("{e}"))?;
                resolver
                    .resolve(cfg, &id)
                    .await
                    .map_err(|e| render_resolve_error(e, format))?
            }
        };

        let depth = if self.no_children {
            Some(0)
        } else {
            self.depth
        };
        let opts = ViewOptions {
            depth,
            max_nodes: self.max_nodes,
            prototype: self.prototype,
            meta: self.meta,
            rich_text: self.rich_text,
        };

        let payload = match target {
            ResolvedTarget::Root => emit_root(&resolver)?,
            ResolvedTarget::Project { synth, project_id } => {
                emit_project(&resolver, synth, &project_id)?
            }
            ResolvedTarget::File {
                synth,
                meta,
                document,
            } => emit_file(cfg, &resolver, synth, &meta, &document, globals.cache_only).await?,
            ResolvedTarget::Node {
                file_synth,
                meta,
                node,
            } => {
                emit_node(
                    cfg,
                    &resolver,
                    file_synth,
                    &meta,
                    &node,
                    &opts,
                    &self,
                    globals.cache_only,
                )
                .await?
            }
            ResolvedTarget::Comment {
                file_synth,
                comm_synth,
                meta,
                comment,
            } => emit_comment(&resolver, file_synth, comm_synth, &meta, &comment)?,
        };

        print(&payload, format)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Node target
// ───────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn emit_node(
    cfg: &Configuration,
    resolver: &Resolver,
    file_synth: u32,
    meta: &FileMeta,
    node: &CacheNode,
    opts: &ViewOptions,
    args: &Args,
    cache_only: bool,
) -> Result<Value> {
    let cache = resolver.cache();
    let file_root = load_full(cfg, cache, &meta.file_key, cache_only).await?;
    let doc = file_root
        .get("document")
        .ok_or_else(|| anyhow!("cached file {} has no `document`", meta.file_key))?;

    let raw_node = resolve_node_id(doc, &node.id).ok_or_else(|| {
        anyhow!(
            "node {} not found in full sidecar (cache out of sync — run `cache prefetch`)",
            node.id
        )
    })?;

    // Path: walk the rkyv cache (already in memory). Cheap, accurate.
    let path = ancestor_path(cache, &meta.file_key, &node.id)?;

    if args.raw {
        return Ok(raw_node.clone());
    }

    let mut collector = Collector::default();
    let mut view = build_node_view(raw_node, opts, &mut collector);

    // Anchored comments (subtree-scoped — descendants count for frame-like
    // targets). On by default, suppressible with --no-comments.
    if !args.no_comments {
        if let Some(arr) =
            build_anchored_comments(cache, &meta.file_key, file_synth, node, &collector)?
        {
            if let Value::Object(map) = &mut view {
                if !arr.is_empty() {
                    map.insert("comments".into(), Value::Array(arr));
                }
            }
        }
    }

    // Top-level blocks: variables (only referenced) and styles_index.
    let vars_root = if args.no_variables {
        None
    } else {
        full_cache::read_variables(cache, &meta.file_key)?
    };
    // Snapshot the variable refs before we hand `&mut collector` to the
    // variables block — `build_variables_block` may record warnings via the
    // collector, which would alias the immutable borrow if we passed the set
    // directly.
    let var_refs = collector.variables.clone();
    let variables = build_variables_block(vars_root.as_ref(), &var_refs, &mut collector);
    let styles_index = build_styles_index_block(&file_root, &collector.styles);

    let mut out = Map::new();
    out.insert(
        "target".into(),
        json!({
            "kind": "node",
            "id": format!("file:{file_synth}:{}", node.id),
            "path": path,
        }),
    );
    out.insert("file".into(), file_block(file_synth, meta));
    out.insert("node".into(), view);
    if variables.as_object().is_some_and(|m| !m.is_empty()) {
        out.insert("variables".into(), variables);
    } else if !args.no_variables {
        if let Some(err) = meta.variables_error.as_deref() {
            // Don't pollute on success; surface only when the user asked and
            // we have a hint as to why we don't have variables data.
            if !err.is_empty() {
                out.insert(
                    "variables_note".into(),
                    json!(format!(
                        "variables sidecar unavailable: {}",
                        if err.contains("403") {
                            "account does not have Variables REST API access"
                        } else {
                            err
                        }
                    )),
                );
            }
        }
    }
    if styles_index.as_object().is_some_and(|m| !m.is_empty()) {
        out.insert("styles_index".into(), styles_index);
    }
    if collector.truncated() {
        out.insert(
            "truncated".into(),
            json!({
                "reason": format!("max_nodes ({}) reached", opts.max_nodes),
                "omitted_count": collector.omitted_ids.len(),
                "omitted_node_ids": collector.omitted_ids,
                "hint": "call node-info on an omitted id, or re-run with --max-nodes N",
            }),
        );
    }
    if !collector.warnings.is_empty() {
        out.insert(
            "_warnings".into(),
            serde_json::to_value(&collector.warnings)?,
        );
    }

    Ok(Value::Object(out))
}

// ───────────────────────────────────────────────────────────────────────────
// Comment target
// ───────────────────────────────────────────────────────────────────────────

fn emit_comment(
    resolver: &Resolver,
    file_synth: u32,
    comm_synth: u32,
    meta: &FileMeta,
    requested: &AssociatedComment,
) -> Result<Value> {
    let cache = resolver.cache();
    // Pull every comment in the file so we can attach replies (when the
    // requested id is a thread head) or the parent (when it's a reply)
    // without a second lookup.
    let all = cache.read_comments(&meta.file_key)?.unwrap_or_default();
    let synth_state = SynthState::load(cache)?;

    // The "thread root" is the head — either the requested entry itself
    // (when no parent) or the parent it points to.
    let head_id = requested
        .parent_id
        .clone()
        .unwrap_or_else(|| requested.comment_id.clone());

    let mut comment_obj = comment_value(file_synth, &synth_state, requested);
    if let Value::Object(map) = &mut comment_obj {
        if requested.parent_id.is_some() {
            // Requested is a reply — surface its parent so the LLM has the
            // full context without a second `node-info` call. Also list
            // sibling replies (same parent, but not the requested entry).
            if let Some(parent) = all.iter().find(|c| c.comment_id == head_id) {
                map.insert(
                    "parent".into(),
                    comment_value(file_synth, &synth_state, parent),
                );
            }
            let siblings: Vec<Value> = all
                .iter()
                .filter(|c| {
                    c.parent_id.as_deref() == Some(head_id.as_str())
                        && c.comment_id != requested.comment_id
                })
                .map(|c| comment_value(file_synth, &synth_state, c))
                .collect();
            if !siblings.is_empty() {
                map.insert("siblings".into(), Value::Array(siblings));
            }
        } else {
            // Requested is a thread head — surface direct replies.
            let replies: Vec<Value> = all
                .iter()
                .filter(|c| c.parent_id.as_deref() == Some(head_id.as_str()))
                .map(|c| comment_value(file_synth, &synth_state, c))
                .collect();
            if !replies.is_empty() {
                map.insert("replies".into(), Value::Array(replies));
            }
        }
    }

    Ok(json!({
        "target": {
            "kind": "comment",
            "id": format!("file:{file_synth}:comm:{comm_synth}"),
            "path": Value::Array(Vec::new()),
        },
        "file": file_block(file_synth, meta),
        "comment": comment_obj,
    }))
}

fn comment_value(file_synth: u32, synth: &SynthState, c: &AssociatedComment) -> Value {
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

// ───────────────────────────────────────────────────────────────────────────
// File / project / root targets
// ───────────────────────────────────────────────────────────────────────────

async fn emit_file(
    cfg: &Configuration,
    resolver: &Resolver,
    synth: u32,
    meta: &FileMeta,
    document: &crate::cache::CachedFile,
    cache_only: bool,
) -> Result<Value> {
    let cache = resolver.cache();
    let file_root = load_full(cfg, cache, &meta.file_key, cache_only).await?;
    let vars_root = full_cache::read_variables(cache, &meta.file_key)?;
    let comments = cache.read_comments(&meta.file_key)?.unwrap_or_default();
    let synth_state = SynthState::load(cache)?;
    let recent_comments: Vec<Value> = comments
        .iter()
        .filter(|c| c.parent_id.is_none())
        .take(FILE_RECENT_COMMENTS)
        .map(|c| comment_value(synth, &synth_state, c))
        .collect();
    let summary = build_file_summary(
        &file_root,
        vars_root.as_ref(),
        comments.len(),
        Some(Value::Array(recent_comments)),
    );
    Ok(json!({
        "target": {
            "kind": "file",
            "id": format!("file:{synth}"),
            "path": Value::Array(Vec::new()),
        },
        "file": file_block_with_doc_info(synth, meta, document),
        "file_summary": summary,
    }))
}

fn emit_project(resolver: &Resolver, synth: u32, project_id: &str) -> Result<Value> {
    let cache = resolver.cache();
    let metas = cache.list_metas()?;
    let synth_state = SynthState::load(cache)?;
    let files: Vec<Value> = metas
        .iter()
        .filter(|m| m.project_id == project_id && m.status == EntryStatus::Ok)
        .map(|m| {
            let s = synth_state.file_synth(&m.file_key).unwrap_or(0);
            json!({
                "id": format!("file:{s}"),
                "file_key": m.file_key,
                "name": m.name,
                "last_modified": m.last_modified,
                "node_count": m.node_count,
                "bytes": m.bytes,
            })
        })
        .collect();
    let project_name = metas
        .iter()
        .find(|m| m.project_id == project_id)
        .map(|m| m.project_name.clone())
        .unwrap_or_default();
    Ok(json!({
        "target": {
            "kind": "project",
            "id": format!("proj:{synth}"),
            "path": Value::Array(Vec::new()),
        },
        "project": {
            "id": format!("proj:{synth}"),
            "project_id": project_id,
            "name": project_name,
            "file_count": files.len(),
        },
        "files": files,
    }))
}

fn emit_root(resolver: &Resolver) -> Result<Value> {
    let cache = resolver.cache();
    let metas = cache.list_metas()?;
    let synth_state = SynthState::load(cache)?;
    // Group by project_id.
    use std::collections::BTreeMap;
    let mut projects: BTreeMap<String, Vec<&FileMeta>> = BTreeMap::new();
    for m in &metas {
        projects.entry(m.project_id.clone()).or_default().push(m);
    }
    let entries: Vec<Value> = projects
        .into_iter()
        .map(|(project_id, files)| {
            let synth = synth_state.project_synth(&project_id).unwrap_or(0);
            let name = files
                .first()
                .map(|m| m.project_name.clone())
                .unwrap_or_default();
            let file_entries: Vec<Value> = files
                .iter()
                .filter(|m| m.status == EntryStatus::Ok)
                .map(|m| {
                    let s = synth_state.file_synth(&m.file_key).unwrap_or(0);
                    json!({
                        "id": format!("file:{s}"),
                        "name": m.name,
                        "last_modified": m.last_modified,
                    })
                })
                .collect();
            json!({
                "id": format!("proj:{synth}"),
                "project_id": project_id,
                "name": name,
                "file_count": file_entries.len(),
                "files": file_entries,
            })
        })
        .collect();
    Ok(json!({
        "target": { "kind": "root", "id": Value::Null, "path": Value::Array(Vec::new()) },
        "projects": entries,
    }))
}

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

/// Load the `.full.json.gz` sidecar for a file. With `cache_only=true` we
/// refuse to fall back to a live fetch; instead we error with the standard
/// "run `cache prefetch`" hint. Without it, a missing or stale sidecar
/// triggers a live fetch and we write the sidecar opportunistically so the
/// next call is offline.
async fn load_full(
    cfg: &Configuration,
    cache: &CacheDir,
    file_key: &str,
    cache_only: bool,
) -> Result<Value> {
    if let Some(v) = full_cache::read_full(cache, file_key)? {
        return Ok(v);
    }
    if cache_only {
        anyhow::bail!(
            "no full-JSON sidecar for {file_key} (and --cache-only is set); run `figma-explorer cache prefetch` to populate the local cache, then retry"
        );
    }
    // Cold path: live-fetch and write the sidecar so future calls are offline.
    let v = crate::cmd::fetch_file_json(cfg, file_key, None).await?;
    // Write the sidecar once and reuse the returned byte count for the meta —
    // an earlier version called write_full twice (once to write, once to learn
    // the size), doubling the gzip + I/O on every cold fetch.
    match full_cache::write_full(cache, file_key, &v) {
        Err(e) => eprintln!("node-info: write_full failed for {file_key}: {e:#}"),
        Ok(n) => {
            if let Ok(Some(mut meta)) = cache.read_meta(file_key) {
                meta.full_fetched_at_epoch = Some(cache::now_epoch());
                meta.full_bytes = Some(n);
                meta.full_schema_version = Some(cache::FULL_SCHEMA_VERSION);
                let _ = cache.write_meta(&meta);
            }
        }
    }
    Ok(v)
}

/// Walk the cached structural document and return `[{id, type, name}, ...]`
/// for ancestors of `node_id`, root-most first, target excluded.
fn ancestor_path(cache: &CacheDir, file_key: &str, node_id: &str) -> Result<Value> {
    let payload = cache
        .read_file(file_key)
        .map_err(|e| anyhow!("reading cached tree for {file_key}: {e}"))?
        .ok_or_else(|| anyhow!("no cached tree for {file_key}"))?;
    let mut stack: Vec<&CacheNode> = Vec::new();
    fn find<'a>(n: &'a CacheNode, target: &str, stack: &mut Vec<&'a CacheNode>) -> bool {
        if n.id == target {
            return true;
        }
        stack.push(n);
        for c in &n.children {
            if find(c, target, stack) {
                return true;
            }
        }
        stack.pop();
        false
    }
    if !find(&payload.document, node_id, &mut stack) {
        return Ok(Value::Array(Vec::new()));
    }
    // The cache rooted at "DOCUMENT" (0:0) is uninteresting — hide it for the
    // same reason `ls` does (`figma-explorer/src/cmd/ls.rs`).
    let path: Vec<Value> = stack
        .iter()
        .filter(|n| n.type_ != "DOCUMENT")
        .map(|n| {
            json!({
                "id": n.id,
                "type": n.type_,
                "name": n.name,
            })
        })
        .collect();
    Ok(Value::Array(path))
}

/// Render comments whose anchored node lies within the subtree being emitted.
/// "Within the subtree" = the node id equals `node.id` *or* the comment's
/// anchored node id was emitted as a descendant (tracked in `collector`'s
/// node-id observations... but we don't currently track those, so we use a
/// cheaper proxy: any comment whose anchor is `node.id`).
///
/// The trade-off here is conscious: we keep comments scoped to the target
/// node itself rather than walking the whole emitted subtree. Subtree-wide
/// inclusion is achievable later by tracking emitted ids on the Collector,
/// but it makes the output substantially noisier on big frames. Worth
/// revisiting if user feedback wants it.
fn build_anchored_comments(
    cache: &CacheDir,
    file_key: &str,
    file_synth: u32,
    node: &CacheNode,
    _collector: &Collector,
) -> Result<Option<Vec<Value>>> {
    let Some(comments) = cache.read_comments(file_key)? else {
        return Ok(None);
    };
    let synth_state = SynthState::load(cache)?;
    // Collect every node id under `node` for a subtree-scoped filter so a
    // frame target picks up comments anchored to its descendants.
    let mut ids: BTreeSet<String> = BTreeSet::new();
    fn collect(n: &CacheNode, ids: &mut BTreeSet<String>) {
        ids.insert(n.id.clone());
        for c in &n.children {
            collect(c, ids);
        }
    }
    collect(node, &mut ids);
    let out: Vec<Value> = comments
        .iter()
        .filter(|c| c.node.as_ref().is_some_and(|n| ids.contains(&n.node_id)))
        .map(|c| comment_value(file_synth, &synth_state, c))
        .collect();
    Ok(Some(out))
}

fn file_block(synth: u32, meta: &FileMeta) -> Value {
    json!({
        "key": meta.file_key,
        "name": meta.name,
        "project_id": meta.project_id,
        "project_name": meta.project_name,
        "synth": synth,
        "last_modified": meta.last_modified,
    })
}

fn file_block_with_doc_info(synth: u32, meta: &FileMeta, doc: &crate::cache::CachedFile) -> Value {
    json!({
        "key": meta.file_key,
        "name": meta.name,
        "project_id": meta.project_id,
        "project_name": meta.project_name,
        "synth": synth,
        "last_modified": meta.last_modified,
        "node_count": doc.node_count,
    })
}

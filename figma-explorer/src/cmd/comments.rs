//! `comments` — list every comment on a file (or filter to one node), each
//! already associated with the node it pins to. Associations are pre-computed
//! at cache-write time and stored in the `.comments.json` sidecar, so this
//! command is just "load + filter + render."
//!
//! Reads the sidecar maintained by the cache layer. The sidecar is refreshed
//! on every `cache prefetch` and on every URL-driven cache refresh; tagged-id
//! reads serve whatever's on disk unless `--max-age-secs` forces a refresh.

use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::{json, Value};

use crate::cache;
use crate::comment_assoc::{AssociatedComment, AssociationMethod};
use crate::resolver::{parse_id, render_resolve_error, ResolvedTarget, Resolver};
use crate::{print, Globals, Output};

/// List comments for a file and associate each with the node it's anchored to.
///
/// `--node-level` filtering is implicit: pass a node id (`file:N:x:y`,
/// `URL`, or a bare native id) and only comments resolved to that node are
/// reported.
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Tagged or native ID, or a Figma URL. `file:N` lists all of the file's
    /// comments; a node-level id filters to that node.
    pub id: Option<String>,

    /// Only emit resolved (true) or unresolved (false) threads. Affects only
    /// top-level comments; their replies follow the thread.
    #[arg(long)]
    pub resolved: Option<bool>,

    /// Force a comments refresh regardless of cache age. Pass `0` to always
    /// refetch; values greater than 0 only refresh if the sidecar is older.
    /// Default: serve whatever's on disk.
    #[arg(long)]
    pub max_age_secs: Option<u64>,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        let format = globals.output;
        let resolver = Resolver::new(globals.cache_only)?;
        let id_str = self
            .id
            .as_deref()
            .ok_or_else(|| anyhow!("comments requires an ID (e.g. `file:N` or a Figma URL)"))?;
        let id = parse_id(id_str).map_err(|e| anyhow!("{e}"))?;
        let target = resolver
            .resolve(cfg, &id)
            .await
            .map_err(|e| render_resolve_error(e, format))?;

        let (file_synth, file_key, focus_node_id): (u32, String, Option<String>) = match &target {
            ResolvedTarget::File { synth, meta, .. } => (*synth, meta.file_key.clone(), None),
            ResolvedTarget::Node { file_synth, meta, node } => {
                (*file_synth, meta.file_key.clone(), Some(node.id.clone()))
            }
            ResolvedTarget::Project { .. } | ResolvedTarget::Root => {
                anyhow::bail!(
                    "comments requires a file or node scope; got a project/root id ({id_str})"
                );
            }
        };

        let mut comments = load_or_refresh_comments(
            cfg,
            &resolver,
            &file_key,
            self.max_age_secs,
            globals.cache_only,
        )
        .await?;
        if comments.is_empty() {
            return print(&json!({ "id": id_str, "comments": [] }), format);
        }

        // Node-level filter: keep comments anchored to the target node. For
        // simplicity we match on node_id; descendants are not included
        // (`ls` already does that on the tree side).
        if let Some(node_id) = &focus_node_id {
            comments.retain(|c| {
                c.node
                    .as_ref()
                    .is_some_and(|n| n.node_id == *node_id)
            });
        }

        // Resolution-state filter applies to top-level threads; replies follow
        // the thread state of their parent.
        if let Some(want_resolved) = self.resolved {
            let parents_in: std::collections::HashSet<String> = comments
                .iter()
                .filter(|c| c.parent_id.is_none())
                .filter(|c| c.resolved_at.is_some() == want_resolved)
                .map(|c| c.comment_id.clone())
                .collect();
            comments.retain(|c| match c.parent_id.as_deref() {
                None => parents_in.contains(c.comment_id.as_str()),
                Some(parent) => parents_in.contains(parent),
            });
        }

        render(file_synth, &file_key, &comments, format)
    }
}

// ─── helpers ──────────────────────────────────────────────────────────────

/// Decide whether to serve the on-disk sidecar or force a refetch. The
/// sidecar is treated as stale when `--max-age-secs` exceeds the recorded
/// fetch age, when no fetch has ever happened, or when the on-disk format
/// predates pre-association (no schema version stamped).
async fn load_or_refresh_comments(
    cfg: &Configuration,
    resolver: &Resolver,
    file_key: &str,
    max_age_secs: Option<u64>,
    cache_only: bool,
) -> Result<Vec<AssociatedComment>> {
    let cache = resolver.cache();
    let meta = cache
        .read_meta(file_key)?
        .ok_or_else(|| anyhow!("no cached metadata for file_key {file_key}"))?;
    let now = cache::now_epoch();

    let stale_age = match max_age_secs {
        None => false,
        Some(max) => match meta.comments_fetched_at_epoch {
            Some(fetched_at) => now.saturating_sub(fetched_at) >= max,
            None => true,
        },
    };
    let stale_schema = meta
        .comments_schema_version
        .map_or(true, |v| v < cache::COMMENTS_SCHEMA_VERSION);

    let sidecar = cache.read_comments(file_key)?;
    let need_refresh = stale_age || stale_schema || sidecar.is_none();

    if need_refresh && !cache_only {
        return cache::refresh_comments(cfg, file_key).await;
    }
    if need_refresh && cache_only {
        anyhow::bail!(
            "comments not cached for {file_key} (and --cache-only is set); run without --cache-only or `cache prefetch`"
        );
    }
    Ok(sidecar.unwrap_or_default())
}

fn render(file_synth: u32, file_key: &str, comments: &[AssociatedComment], format: Output) -> Result<()> {
    match format {
        Output::Yaml => render_yaml(file_synth, comments),
        Output::Json => render_json(file_synth, file_key, comments, format),
    }
}

fn render_yaml(file_synth: u32, comments: &[AssociatedComment]) -> Result<()> {
    if comments.is_empty() {
        return Ok(());
    }
    // Pre-measure column widths so the pipe rail aligns across rows.
    let rows: Vec<YamlRow> = comments.iter().map(|c| YamlRow::from(c, file_synth)).collect();
    let id_w = rows.iter().map(|r| r.id.len()).max().unwrap_or(1);
    let b_w = rows.iter().map(|r| r.bounds.len()).max().unwrap_or(1).max(1);
    let mut out = String::new();
    for r in &rows {
        out.push_str(&format!(
            "{id:<id_w$}  {b:<b_w$}  | COMMENT  {method:<14}  \"{author}\"  \"{msg}\"  {tail}\n",
            id = r.id,
            b = r.bounds,
            method = r.method,
            author = truncate_inline(&r.author, 24),
            msg = truncate_inline(&r.message, 60),
            tail = r.tail,
            id_w = id_w,
            b_w = b_w,
        ));
    }
    print!("{out}");
    Ok(())
}

fn render_json(file_synth: u32, file_key: &str, comments: &[AssociatedComment], format: Output) -> Result<()> {
    let items: Vec<Value> = comments
        .iter()
        .map(|c| {
            let mut v = serde_json::to_value(c).unwrap_or(Value::Null);
            // Promote the node id to a qualified `file:N:x:y` form for paste-readiness.
            if let Some(node) = c.node.as_ref() {
                if let Value::Object(map) = &mut v {
                    let qualified = format!("file:{file_synth}:{}", node.node_id);
                    if let Some(node_obj) = map.get_mut("node").and_then(|n| n.as_object_mut()) {
                        node_obj.insert("id".into(), Value::String(qualified));
                    }
                }
            }
            v
        })
        .collect();
    print(
        &json!({
            "id": format!("file:{file_synth}"),
            "file_key": file_key,
            "comments": items,
        }),
        format,
    )
}

struct YamlRow {
    id: String,
    bounds: String,
    method: String,
    author: String,
    message: String,
    tail: String,
}

impl YamlRow {
    fn from(c: &AssociatedComment, file_synth: u32) -> Self {
        let id = c.comment_id.clone();
        let (bounds, tail) = match c.node.as_ref() {
            Some(node) => {
                let qualified = format!("file:{file_synth}:{}", node.node_id);
                (
                    "-".to_owned(),
                    format!("({qualified} \"{}\")", truncate_inline(&node.name, 30)),
                )
            }
            None => ("-".to_owned(), String::new()),
        };
        let method = match &c.method {
            AssociationMethod::Explicit => "explicit".to_owned(),
            AssociationMethod::Containing => "containing".to_owned(),
            AssociationMethod::Nearest { distance_px } => {
                format!("nearest:{:.0}px", distance_px)
            }
            AssociationMethod::CanvasLevel => {
                if c.stale_node_id.is_some() {
                    "stale-ref".to_owned()
                } else {
                    "canvas".to_owned()
                }
            }
        };
        YamlRow {
            id,
            bounds,
            method,
            author: c.author.clone(),
            message: c.message.replace(['\n', '\r'], " "),
            tail,
        }
    }
}

/// Truncate to fit a fixed column. Adds a trailing ellipsis if cut.
fn truncate_inline(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

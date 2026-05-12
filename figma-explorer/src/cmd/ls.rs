//! `ls` — list anything at any level. Replaces `files`/`pages`/`frames`/`tree`.
//!
//! Behavior depends on what the ID resolves to:
//! - No ID → list every cached project, with its files grouped underneath,
//!   recursing into each file's canvases/frames up to `--depth`.
//! - `proj:N` → header + files in that project, recursing as above.
//! - `file:N` → synthesized `file:N FILE "name"` header + descendants. The
//!   DOCUMENT node at `0:0` is hidden to keep ambiguity at bay.
//! - `file:N:x:y` or `URL` or bare `x:y` → that node + descendants.
//!
//! `--depth` is honored at every level (default 3, counting levels below
//! "self" — same convention `render_flat` uses).
//!
//! Output is the new pipe-rail flat format from `tree::render_flat` so each
//! line is grep-friendly and paste-safe across commands.

use anyhow::Result;
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::{json, Value};
use std::collections::BTreeSet;

use crate::cache::{CacheNode, EntryStatus, FileMeta};
use crate::comment_assoc::AssociatedComment;
use crate::id::Id;
use crate::resolver::{parse_id, render_resolve_error, ResolvedTarget, Resolver};
use crate::tree::{
    collect_visible, render_flat_with_comments, truncate_display, CommentRow, NAME_DISPLAY_MAX,
};
use crate::{print, Globals, Output};

/// Default descent depth. Depth counts levels below "self" (the same
/// convention `render_flat` uses). At the root this means projects (depth 0),
/// files (depth 1), canvases (depth 2), frames (depth 3). At `file:N` it
/// means file (depth 0), canvases (depth 1), and so on.
const DEFAULT_DEPTH: usize = 3;

/// Default canvas names hidden from listings unless `--no-ignore` is passed.
/// Case-insensitive match against CANVAS-type nodes only. These names are
/// designer conventions for cover pages, in-progress work, and archived
/// versions — noise when browsing for real product surfaces. The cache still
/// stores them, so drilling in (`ls file:N:0:5`) and other commands work.
const DEFAULT_IGNORED_CANVASES: &[&str] = &["Cover", "WIP", "Archive"];

/// Returns the canonical (cased) ignore-list name for `node` when it matches,
/// or `None` otherwise. The returned name comes from `DEFAULT_IGNORED_CANVASES`
/// rather than the node, so reports display "WIP" consistently regardless of
/// whether the designer wrote "wip", "WIP", or "Wip".
fn ignored_canvas_label(node: &CacheNode) -> Option<&'static str> {
    if node.type_ != "CANVAS" {
        return None;
    }
    DEFAULT_IGNORED_CANVASES
        .iter()
        .copied()
        .find(|n| n.eq_ignore_ascii_case(&node.name))
}

/// Emit the one-line YAML/text transparency note when the filter actually
/// fired. Stays silent when nothing was hidden (clean listings) or when
/// `--no-ignore` disabled the filter (no names ever accumulated).
fn print_hidden_comment(hidden: &BTreeSet<&'static str>) {
    if hidden.is_empty() {
        return;
    }
    let names = hidden.iter().copied().collect::<Vec<_>>().join(", ");
    let label = if hidden.len() == 1 {
        "canvas"
    } else {
        "canvases"
    };
    println!("# hidden {label}: {names} — use --no-ignore to show");
}

/// Build the JSON shape for the top-level `ignored` field. Always emitted by
/// JSON callers (stable schema for consumers) — possibly with an empty array.
fn ignored_json(hidden: &BTreeSet<&'static str>) -> Value {
    let canvases: Vec<Value> = hidden.iter().map(|n| json!(n)).collect();
    json!({ "canvases": canvases })
}

/// Max characters of a comment's head message rendered in `ls` output.
/// Long-form discussion gets truncated to keep rows scannable; the full
/// thread is paste-ready behind the `file:N:comm:M` id for future tooling.
const COMMENT_MSG_DISPLAY_MAX: usize = 120;

/// List a node and its descendants. Honors `--depth` (default 3) at every
/// level — root, project, file, and node alike. Comments anchored to nodes
/// in the rendered tree always appear inline; there's no `--comments` flag.
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Tagged or native ID, or a Figma URL. Omit to list cached projects.
    pub id: Option<String>,

    /// Override the default descent depth.
    #[arg(long)]
    pub depth: Option<usize>,

    /// Disable the default canvas-name ignore filter — show Cover/WIP/Archive
    /// canvases that are hidden by default. The cache always stores them;
    /// this flag only affects display.
    #[arg(long)]
    pub no_ignore: bool,

    /// Only show resolved (`true`) or unresolved (`false`) comment threads.
    /// Filter applies to thread heads; replies aren't rendered as separate
    /// rows in `ls` regardless. Default: show every thread.
    #[arg(long)]
    pub resolved: Option<bool>,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        let resolver = Resolver::new(globals.cache_only)?;
        let format = globals.output;
        let depth = self.depth.unwrap_or(DEFAULT_DEPTH);

        let show_all = self.no_ignore;
        let resolved = self.resolved;
        match self.id.as_deref() {
            None => render_root(&resolver, depth, format, show_all, resolved),
            Some(s) => {
                let id = parse_id(s).map_err(|e| anyhow::anyhow!("{e}"))?;
                // Promote a bare native id to a qualified one when --in
                // names a file scope. Means `figma-explorer --in file:28 ls 0:0`
                // resolves cleanly instead of returning a 50-way ambiguity.
                let id = apply_scope(id, globals.scope.as_deref())
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                let target = resolver
                    .resolve(cfg, &id)
                    .await
                    .map_err(|e| render_resolve_error(e, format))?;
                match target {
                    ResolvedTarget::Root => {
                        render_root(&resolver, depth, format, show_all, resolved)
                    }
                    ResolvedTarget::Project { synth, project_id } => render_project(
                        &resolver,
                        synth,
                        &project_id,
                        depth,
                        format,
                        show_all,
                        resolved,
                    ),
                    ResolvedTarget::File {
                        synth,
                        meta,
                        document,
                    } => render_file(
                        &resolver,
                        synth,
                        &meta,
                        &document.document,
                        depth,
                        format,
                        show_all,
                        resolved,
                    ),
                    ResolvedTarget::Node {
                        file_synth,
                        meta,
                        node,
                    } => {
                        // Filter intentionally does not apply when the user has
                        // already drilled into a specific node — drilling in is
                        // explicit, the filter is for browsing.
                        render_node_subtree(
                            &resolver, file_synth, &meta, &node, depth, format, resolved,
                        )
                    }
                }
            }
        }
    }
}

/// Root listing — projects + their files, recursing into each file's
/// canvases/frames when `depth >= 2`. Reads structural data directly from
/// the cache; files whose payload is missing or fails to decode are emitted
/// as a file row only (no descent), so a partially populated cache still
/// produces useful output.
fn render_root(
    resolver: &Resolver,
    depth: usize,
    format: Output,
    show_all: bool,
    resolved_filter: Option<bool>,
) -> Result<()> {
    let synth = resolver.synth();
    let metas = resolver.cache().list_metas()?;

    // Group OK files by project synth.
    let mut groups: Vec<(u32, String, String, Vec<FileMeta>)> = Vec::new();
    for (project_id, &project_synth) in &synth.projects {
        let mut files: Vec<FileMeta> = metas
            .iter()
            .filter(|m| {
                m.status == EntryStatus::Ok
                    && m.project_id == *project_id
                    && synth.file_synth(&m.file_key).is_some()
            })
            .cloned()
            .collect();
        files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        let project_name = derive_project_name(&metas, project_id);
        groups.push((project_synth, project_id.clone(), project_name, files));
    }
    groups.sort_by_key(|(s, _, _, _)| *s);

    let mut hidden: BTreeSet<&'static str> = BTreeSet::new();

    match format {
        Output::Yaml => {
            let mut rows: Vec<Row> = Vec::new();
            for (psynth, _pid, pname, files) in &groups {
                rows.push(Row::header(
                    format!("proj:{psynth}"),
                    0,
                    "PROJECT",
                    pname.clone(),
                ));
                if depth >= 1 {
                    for fm in files {
                        let fsynth = synth.file_synth(&fm.file_key).expect("filtered above");
                        rows.push(Row::header(
                            format!("file:{fsynth}"),
                            1,
                            "FILE",
                            fm.name.clone(),
                        ));
                        if depth >= 2 {
                            append_descent_rows(
                                resolver,
                                fsynth,
                                fm,
                                depth,
                                1,
                                &mut rows,
                                show_all,
                                &mut hidden,
                                resolved_filter,
                            );
                        }
                    }
                }
            }
            print_hidden_comment(&hidden);
            print!("{}", format_rows(&rows));
            Ok(())
        }
        Output::Json => {
            let projects: Vec<Value> = groups
                .iter()
                .map(|(ps, pid, pname, files)| {
                    let file_jsons: Vec<Value> = if depth >= 1 {
                        files
                            .iter()
                            .map(|fm| {
                                let fs = synth.file_synth(&fm.file_key).expect("filtered above");
                                build_file_json(
                                    resolver,
                                    fs,
                                    fm,
                                    depth,
                                    show_all,
                                    &mut hidden,
                                    resolved_filter,
                                )
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    json!({
                        "id": format!("proj:{ps}"),
                        "project_id": pid,
                        "name": pname,
                        "files": file_jsons,
                    })
                })
                .collect();
            print(
                &json!({ "ignored": ignored_json(&hidden), "projects": projects }),
                format,
            )
        }
    }
}

/// Unified row used by the YAML printers in `render_root` and `render_project`.
/// `id`, `bounds`, and `name` are pre-formatted; `format_rows` only handles
/// column alignment and indentation.
struct Row {
    id: String,
    bounds: String,
    depth: usize,
    kind: String,
    name: String,
    truncated: Option<usize>,
    /// When `Some`, replaces the quoted-name portion in `format_rows` with
    /// this payload verbatim. Used by comment rows whose trailing data
    /// (author, reply count) doesn't fit the standard `TYPE "name"` shape.
    raw_payload: Option<String>,
}

impl Row {
    /// Project- or file-header row. No bounds, no truncation marker. Header
    /// names are kept verbatim — they come from cache metadata, not user
    /// node names, so the 200-char node-name cap doesn't apply.
    fn header(id: String, depth: usize, kind: &str, name: String) -> Self {
        Self {
            id,
            bounds: "-".to_owned(),
            depth,
            kind: kind.to_owned(),
            name,
            truncated: None,
            raw_payload: None,
        }
    }

    /// Comment thread-head row. Spliced under its anchor node in
    /// `append_descent_rows`. `display` is a pre-formatted payload (quoted
    /// message + author + reply count); the standard `"{name}"` slot is
    /// bypassed via `raw_payload`.
    fn comment(id: String, depth: usize, display: String) -> Self {
        Self {
            id,
            bounds: "-".to_owned(),
            depth,
            kind: "COMMENT".to_owned(),
            name: String::new(),
            truncated: None,
            raw_payload: Some(display),
        }
    }
}

/// Two-pass YAML printer: measure id/bounds column widths, then emit lines.
/// Format matches `tree::format_cache_line` so root/project rows stack
/// visually with descendant rows pulled from `CacheNode`s.
fn format_rows(rows: &[Row]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let max_id = rows.iter().map(|r| r.id.len()).max().unwrap_or(0);
    let max_bounds = rows
        .iter()
        .map(|r| r.bounds.len())
        .max()
        .unwrap_or(1)
        .max(1);
    let mut out = String::new();
    for r in rows {
        let indent = "  ".repeat(r.depth);
        let payload = match &r.raw_payload {
            Some(p) => p.clone(),
            None => format!("\"{}\"", r.name),
        };
        out.push_str(&format!(
            "{id:<id_w$}  {b:<b_w$}  | {indent}{kind}  {payload}",
            id = r.id,
            b = r.bounds,
            kind = r.kind,
            payload = payload,
            id_w = max_id,
            b_w = max_bounds,
            indent = indent,
        ));
        if let Some(n) = r.truncated {
            out.push_str(&format!("  [+{n} children]"));
        }
        out.push('\n');
    }
    out
}

/// Load `fm`'s cached document and append descendant rows under the (already
/// emitted) FILE header. `file_depth` is the depth at which the FILE header
/// sits in the surrounding listing (1 under both root and project headers).
/// Silent no-op when the payload is missing or fails to decode — callers
/// have already emitted the file row.
///
/// Comment rows are spliced in immediately after their anchor node's row.
/// Canvas-level (unanchored) comments are appended in a trailing block at
/// `file_depth + 1`, so they appear as direct children of the FILE header.
fn append_descent_rows(
    resolver: &Resolver,
    file_synth: u32,
    fm: &FileMeta,
    depth: usize,
    file_depth: usize,
    rows: &mut Vec<Row>,
    show_all: bool,
    hidden: &mut BTreeSet<&'static str>,
    resolved_filter: Option<bool>,
) {
    let cached = match resolver.cache().read_file(&fm.file_key) {
        Ok(Some(c)) => c,
        _ => return,
    };
    let synthetic = synthesize_file_root(fm, &cached.document, show_all, hidden);
    let comment_rows = load_comment_rows(resolver, &fm.file_key, file_synth, resolved_filter);
    let mut by_node: std::collections::HashMap<&str, Vec<&CommentRow>> =
        std::collections::HashMap::new();
    let mut canvas: Vec<&CommentRow> = Vec::new();
    for row in &comment_rows {
        match row.anchor_node_id.as_deref() {
            Some(nid) => by_node.entry(nid).or_default().push(row),
            None => canvas.push(row),
        }
    }

    // The synthesized root itself represents the FILE row, already emitted
    // by the caller; descend its children up to `depth - file_depth` levels.
    let max_sub_depth = depth.saturating_sub(file_depth);
    let mut tuples: Vec<(&CacheNode, usize, Option<usize>)> = Vec::new();
    collect_visible(&synthetic, 0, max_sub_depth, &mut tuples);
    for (node, sub_depth, truncated) in tuples.into_iter().skip(1) {
        let kind = if node.type_.is_empty() {
            "?".to_owned()
        } else {
            node.type_.clone()
        };
        let row_depth = file_depth + sub_depth;
        rows.push(Row {
            id: format!("file:{}:{}", file_synth, node.id),
            bounds: node
                .bounds
                .map(|b| b.compact())
                .unwrap_or_else(|| "-".to_owned()),
            depth: row_depth,
            kind,
            name: truncate_display(&node.name, NAME_DISPLAY_MAX).into_owned(),
            truncated,
            raw_payload: None,
        });
        if let Some(anchored) = by_node.get(node.id.as_str()) {
            for cr in anchored {
                rows.push(Row::comment(
                    cr.id.clone(),
                    row_depth + 1,
                    cr.display.clone(),
                ));
            }
        }
    }
    // Canvas-level threads fall under the FILE row at file_depth + 1.
    for cr in canvas {
        rows.push(Row::comment(
            cr.id.clone(),
            file_depth + 1,
            cr.display.clone(),
        ));
    }
}

/// Load the pre-associated comments sidecar for `file_key` and convert each
/// thread head into a [`CommentRow`] ready for splicing. Returns an empty
/// vector when the sidecar is absent, when reading it fails, or when no
/// thread heads survive the `--resolved` filter — callers don't have to
/// distinguish "no comments" from "couldn't read."
fn load_comment_rows(
    resolver: &Resolver,
    file_key: &str,
    file_synth: u32,
    resolved_filter: Option<bool>,
) -> Vec<CommentRow> {
    let comments = match resolver.cache().read_comments(file_key) {
        Ok(Some(c)) if !c.is_empty() => c,
        _ => return Vec::new(),
    };
    build_comment_rows(&comments, resolver, file_synth, resolved_filter)
}

/// Build `CommentRow`s from a slice of already-loaded `AssociatedComment`s.
/// Factored out so tests can drive it without a populated cache directory.
fn build_comment_rows(
    comments: &[AssociatedComment],
    resolver: &Resolver,
    file_synth: u32,
    resolved_filter: Option<bool>,
) -> Vec<CommentRow> {
    let synth = resolver.synth();

    // Reply count per parent thread head id.
    let mut reply_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for c in comments {
        if let Some(pid) = c.parent_id.as_deref() {
            *reply_counts.entry(pid).or_insert(0) += 1;
        }
    }

    let mut out: Vec<CommentRow> = Vec::new();
    for c in comments {
        if c.parent_id.is_some() {
            // Replies collapse into their parent's `(+N replies)` suffix —
            // they don't get their own row in `ls` output.
            continue;
        }
        if let Some(want_resolved) = resolved_filter {
            if c.resolved_at.is_some() != want_resolved {
                continue;
            }
        }
        let id = match synth.comment_synth(file_synth, &c.comment_id) {
            Some(n) => format!("file:{file_synth}:comm:{n}"),
            // Fallback: synth not yet interned (sidecar exists, prefetch
            // not yet run). Surfaces the raw API id so the row is still
            // grep-friendly. Next `cache prefetch` mints a proper synth.
            None => format!("file:{file_synth}:comm:?({})", c.comment_id),
        };
        let single_line = c.message.replace(['\n', '\r'], " ");
        let truncated = truncate_display(&single_line, COMMENT_MSG_DISPLAY_MAX);
        let reply_count = reply_counts
            .get(c.comment_id.as_str())
            .copied()
            .unwrap_or(0);
        let mut display = format!("\"{}\"  by @{}", truncated, c.author);
        if reply_count > 0 {
            display.push_str(&format!("  +{reply_count}"));
        }
        out.push(CommentRow {
            id,
            anchor_node_id: c.node.as_ref().map(|n| n.node_id.clone()),
            display,
        });
    }
    out
}

/// Build the JSON object for one file row, attaching a recursive `children`
/// array (or `truncated` marker) when `depth >= 2`. Mirrors the YAML descent
/// in `append_descent_rows`. Pre-associated comments are attached on each
/// node parallel to `children`; canvas-level threads sit on the file root.
fn build_file_json(
    resolver: &Resolver,
    file_synth: u32,
    fm: &FileMeta,
    depth: usize,
    show_all: bool,
    hidden: &mut BTreeSet<&'static str>,
    resolved_filter: Option<bool>,
) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(format!("file:{file_synth}")));
    obj.insert("file_key".into(), json!(fm.file_key));
    obj.insert("name".into(), json!(fm.name));
    obj.insert("last_modified".into(), json!(fm.last_modified));
    let comment_rows = load_comment_rows(resolver, &fm.file_key, file_synth, resolved_filter);
    if depth >= 2 {
        if let Ok(Some(cached)) = resolver.cache().read_file(&fm.file_key) {
            let synthetic = synthesize_file_root(fm, &cached.document, show_all, hidden);
            let rendered =
                render_subtree_json_with_comments(file_synth, &synthetic, depth - 1, &comment_rows);
            if let Value::Object(rendered_obj) = rendered {
                if let Some(kids) = rendered_obj.get("children") {
                    obj.insert("children".into(), kids.clone());
                }
                if let Some(trunc) = rendered_obj.get("truncated") {
                    obj.insert("truncated".into(), trunc.clone());
                }
            }
        }
    }
    // Canvas-level threads attach to the file root regardless of depth.
    let canvas_json: Vec<Value> = comment_rows
        .iter()
        .filter(|r| r.anchor_node_id.is_none())
        .map(|r| comment_row_json(r))
        .collect();
    if !canvas_json.is_empty() {
        obj.insert("canvas_comments".into(), Value::Array(canvas_json));
    }
    Value::Object(obj)
}

/// Best-effort lookup of the human-readable project name for `project_id` by
/// scanning file metas. Falls back to `project_id` when no file in the project
/// carries a non-empty name (project never listed, or listing predated the
/// project_name field).
fn derive_project_name(metas: &[FileMeta], project_id: &str) -> String {
    metas
        .iter()
        .find(|m| m.project_id == project_id && !m.project_name.is_empty())
        .map(|m| m.project_name.clone())
        .unwrap_or_else(|| project_id.to_owned())
}

/// Project listing — header + files in that project, recursing into each
/// file's structural tree when `depth >= 2`. Reads from cache state.
fn render_project(
    resolver: &Resolver,
    project_synth: u32,
    project_id: &str,
    depth: usize,
    format: Output,
    show_all: bool,
    resolved_filter: Option<bool>,
) -> Result<()> {
    let synth = resolver.synth();
    let metas = resolver.cache().list_metas()?;
    let project_name = derive_project_name(&metas, project_id);
    let mut files: Vec<(u32, FileMeta)> = metas
        .iter()
        .filter(|m| m.status == EntryStatus::Ok && m.project_id == project_id)
        .filter_map(|m| synth.file_synth(&m.file_key).map(|s| (s, m.clone())))
        .collect();
    files.sort_by(|(_, a), (_, b)| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    let mut hidden: BTreeSet<&'static str> = BTreeSet::new();

    match format {
        Output::Yaml => {
            let mut rows: Vec<Row> = Vec::new();
            rows.push(Row::header(
                format!("proj:{project_synth}"),
                0,
                "PROJECT",
                project_name.clone(),
            ));
            if depth >= 1 {
                for (fs, fm) in &files {
                    rows.push(Row::header(
                        format!("file:{fs}"),
                        1,
                        "FILE",
                        fm.name.clone(),
                    ));
                    if depth >= 2 {
                        append_descent_rows(
                            resolver,
                            *fs,
                            fm,
                            depth,
                            1,
                            &mut rows,
                            show_all,
                            &mut hidden,
                            resolved_filter,
                        );
                    }
                }
            }
            print_hidden_comment(&hidden);
            print!("{}", format_rows(&rows));
            Ok(())
        }
        Output::Json => {
            let mut file_jsons: Vec<Value> = Vec::new();
            if depth >= 1 {
                for (fs, fm) in &files {
                    file_jsons.push(build_file_json(
                        resolver,
                        *fs,
                        fm,
                        depth,
                        show_all,
                        &mut hidden,
                        resolved_filter,
                    ));
                }
            }
            print(
                &json!({
                    "id": format!("proj:{project_synth}"),
                    "project_id": project_id,
                    "name": project_name,
                    "ignored": ignored_json(&hidden),
                    "files": file_jsons,
                }),
                format,
            )
        }
    }
}

/// File-level listing. We synthesize a fake root with the file's name and the
/// DOCUMENT's children, so the user sees `file:N FILE "name"` at the top and
/// the actual `0:0` DOCUMENT node stays hidden — in both YAML and JSON paths.
fn render_file(
    resolver: &Resolver,
    file_synth: u32,
    meta: &FileMeta,
    document: &CacheNode,
    depth: usize,
    format: Output,
    show_all: bool,
    resolved_filter: Option<bool>,
) -> Result<()> {
    let mut hidden: BTreeSet<&'static str> = BTreeSet::new();
    let synthetic_root = synthesize_file_root(meta, document, show_all, &mut hidden);
    let comment_rows = load_comment_rows(resolver, &meta.file_key, file_synth, resolved_filter);
    match format {
        Output::Yaml => {
            print_hidden_comment(&hidden);
            let lines =
                render_flat_with_comments(&synthetic_root, file_synth, depth, &comment_rows);
            print!("{}\n", lines.join("\n"));
            Ok(())
        }
        Output::Json => {
            let items = render_subtree_json_with_comments(
                file_synth,
                &synthetic_root,
                depth,
                &comment_rows,
            );
            let canvas_json: Vec<Value> = comment_rows
                .iter()
                .filter(|r| r.anchor_node_id.is_none())
                .map(comment_row_json)
                .collect();
            let mut payload = serde_json::Map::new();
            payload.insert("id".into(), json!(format!("file:{file_synth}")));
            payload.insert("file_key".into(), json!(meta.file_key));
            payload.insert("name".into(), json!(meta.name));
            payload.insert("ignored".into(), ignored_json(&hidden));
            payload.insert("items".into(), items);
            if !canvas_json.is_empty() {
                payload.insert("canvas_comments".into(), Value::Array(canvas_json));
            }
            print(&Value::Object(payload), format)
        }
    }
}

/// Node-subtree listing — straightforward delegation to `render_flat_with_comments`.
/// Anchor matching still considers the full set of comments for the file: a
/// comment whose anchor lives outside the subtree just won't render here.
fn render_node_subtree(
    resolver: &Resolver,
    file_synth: u32,
    meta: &FileMeta,
    node: &CacheNode,
    depth: usize,
    format: Output,
    resolved_filter: Option<bool>,
) -> Result<()> {
    let comment_rows = load_comment_rows(resolver, &meta.file_key, file_synth, resolved_filter);
    match format {
        Output::Yaml => {
            let lines = render_flat_with_comments(node, file_synth, depth, &comment_rows);
            print!("{}\n", lines.join("\n"));
            Ok(())
        }
        Output::Json => print(
            &json!({
                "id": format!("file:{file_synth}:{}", node.id),
                "file_key": meta.file_key,
                "items": render_subtree_json_with_comments(file_synth, node, depth, &comment_rows),
            }),
            format,
        ),
    }
}

pub fn synthesize_file_root(
    meta: &FileMeta,
    document: &CacheNode,
    show_all: bool,
    hidden: &mut BTreeSet<&'static str>,
) -> CacheNode {
    // Skip the DOCUMENT node entirely — its visible children (canvases)
    // become the top-level items under the synthesized FILE header. Then,
    // unless `show_all`, drop any CANVAS whose name matches the default
    // ignore list (Cover/WIP/Archive, case-insensitive). Names of the
    // dropped canvases are recorded in `hidden` so the caller can emit a
    // transparency line — never silent.
    let children: Vec<CacheNode> = document
        .children
        .iter()
        .filter(|c| c.visible)
        .filter(|c| {
            if show_all {
                return true;
            }
            match ignored_canvas_label(c) {
                Some(label) => {
                    hidden.insert(label);
                    false
                }
                None => true,
            }
        })
        .cloned()
        .collect();
    CacheNode {
        // Empty id makes `tree::format_cache_line` emit the bare `file:N` form
        // (no trailing `:0:0`), so the DOCUMENT node never surfaces as a row.
        id: String::new(),
        type_: "FILE".to_owned(),
        name: meta.name.clone(),
        visible: true,
        bounds: None,
        children,
    }
}

/// JSON tree builder. Attaches a `comments` array per node from the
/// pre-associated `comment_rows` slice; canvas-level threads are handled by
/// the caller and not emitted on individual nodes.
fn render_subtree_json_with_comments(
    file_synth: u32,
    node: &CacheNode,
    max_depth: usize,
    comment_rows: &[CommentRow],
) -> Value {
    let by_node: std::collections::HashMap<&str, Vec<&CommentRow>> = {
        let mut m: std::collections::HashMap<&str, Vec<&CommentRow>> =
            std::collections::HashMap::new();
        for row in comment_rows {
            if let Some(nid) = row.anchor_node_id.as_deref() {
                m.entry(nid).or_default().push(row);
            }
        }
        m
    };
    fn build(
        node: &CacheNode,
        file_synth: u32,
        depth: usize,
        max_depth: usize,
        by_node: &std::collections::HashMap<&str, Vec<&CommentRow>>,
    ) -> Value {
        let mut obj = serde_json::Map::new();
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
fn comment_row_json(row: &CommentRow) -> Value {
    json!({
        "id": row.id,
        "display": row.display,
    })
}

/// If `--in <ID>` named a file scope and the user passed a bare native id,
/// rewrite the bare id as a qualified `file:N:x:y`. All other id shapes pass
/// through unchanged (an explicit qualifier wins over `--in`).
fn apply_scope(id: Id, scope: Option<&str>) -> Result<Id> {
    let Some(scope) = scope else { return Ok(id) };
    let Id::BareNode(node) = &id else {
        return Ok(id);
    };
    let scope_id = parse_id(scope).map_err(|e| anyhow::anyhow!("--in: {e}"))?;
    let file_synth = match scope_id {
        Id::File(n) => n,
        Id::Node { file, .. } => file,
        _ => anyhow::bail!("--in must name a file or node scope (e.g. file:2); got {scope}"),
    };
    Ok(Id::Node {
        file: file_synth,
        node: node.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{build_cached_file, CacheDir, FileRef};
    use crate::tree::render_flat;
    use serde_json::json;

    /// End-to-end check of the spine: build a fixture cache + synth state,
    /// resolve `file:N` via `Resolver`, synthesize the file root, render
    /// the flat output, and verify the synthesized FILE row never exposes
    /// the underlying DOCUMENT node (`file:N:0:0`).
    #[tokio::test]
    async fn file_id_synthesizes_header_and_hides_document() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = CacheDir::new(tmp.path());
        cache.ensure().unwrap();

        // Canvas names are deliberately neutral here so the new default
        // ignore filter (Cover/WIP/Archive) doesn't interfere with this
        // test's focus — synthesis + DOCUMENT-row suppression.
        let doc = json!({
            "id": "0:0", "name": "doc", "type": "DOCUMENT",
            "children": [
                { "id": "0:1", "name": "Home", "type": "CANVAS",
                  "children": [{ "id": "1:2", "name": "Header", "type": "FRAME",
                                 "absoluteBoundingBox": { "x": 0.0, "y": 0.0, "width": 1440.0, "height": 80.0 } }] },
                { "id": "0:2", "name": "Employees", "type": "CANVAS" },
            ],
        });
        let file_ref = FileRef {
            file_key: "abc".into(),
            name: "Demo File".into(),
            last_modified: "2026-01-01".into(),
            project_id: "p1".into(),
            project_name: "P".into(),
        };
        let payload = build_cached_file(&file_ref, &doc, 0);
        cache.write_file("abc", &payload).unwrap();
        cache
            .write_meta(&FileMeta::from_success(&file_ref, &payload, 0, 0))
            .unwrap();
        crate::synth::with_lock(&cache, |s| {
            s.intern_project("p1");
            s.intern_file("abc");
        })
        .unwrap();

        let resolver = Resolver::from_cache(CacheDir::new(tmp.path()), true).unwrap();
        let id: Id = "file:1".parse().unwrap();
        let target = resolver
            .resolve(&figma_api::apis::configuration::Configuration::new(), &id)
            .await
            .unwrap();

        let (synth, meta, document) = match target {
            ResolvedTarget::File {
                synth,
                meta,
                document,
            } => (synth, meta, document),
            other => panic!("expected File target, got {other:?}"),
        };
        let mut hidden = BTreeSet::new();
        let synthetic = synthesize_file_root(&meta, &document.document, false, &mut hidden);
        let lines = render_flat(&synthetic, synth, 1);

        // First line must be the synthesized FILE row with bare `file:1` (no
        // trailing `:0:0`), so the DOCUMENT node id is hidden.
        let first = &lines[0];
        assert!(
            first.contains("file:1") && !first.contains("file:1:0:0"),
            "expected synthesized file:1 header, got: {first}"
        );
        assert!(
            first.contains("FILE"),
            "expected FILE type in header: {first}"
        );
        assert!(
            first.contains("\"Demo File\""),
            "expected file name: {first}"
        );

        // CANVAS children should appear with their qualified IDs.
        let joined = lines.join("\n");
        assert!(
            joined.contains("file:1:0:1"),
            "Home canvas missing: {joined}"
        );
        assert!(
            joined.contains("file:1:0:2"),
            "Employees canvas missing: {joined}"
        );
        // No row references `file:1:0:0` — the DOCUMENT node id is suppressed.
        assert!(
            !joined.contains("file:1:0:0"),
            "DOCUMENT row leaked into output: {joined}"
        );
    }

    #[test]
    fn apply_scope_promotes_bare_node_under_file_scope() {
        let id: Id = "1094:66591".parse().unwrap();
        let promoted = apply_scope(id, Some("file:7")).unwrap();
        assert_eq!(
            promoted,
            Id::Node {
                file: 7,
                node: "1094:66591".into()
            }
        );
    }

    #[test]
    fn apply_scope_leaves_explicit_qualifier_alone() {
        let id: Id = "file:3:1094:66591".parse().unwrap();
        // --in file:7 should be ignored — explicit qualifier wins.
        let unchanged = apply_scope(id.clone(), Some("file:7")).unwrap();
        assert_eq!(unchanged, id);
    }

    #[test]
    fn apply_scope_rejects_non_file_scope() {
        let id: Id = "0:0".parse().unwrap();
        let err = apply_scope(id, Some("proj:1")).unwrap_err();
        assert!(err.to_string().contains("must name a file"), "got: {err}");
    }

    /// Build a minimal DOCUMENT CacheNode with the given canvas children.
    /// Used by the synthesize_file_root filter tests below. The FileMeta is
    /// constructed via `FileMeta::from_success` (the same helper the rest of
    /// the codebase uses) so we don't have to track its full field set here.
    fn document_with_canvases(canvases: Vec<(&str, &str, &str)>) -> (FileMeta, CacheNode) {
        let file_ref = FileRef {
            file_key: "k".into(),
            name: "Demo".into(),
            last_modified: "2026-01-01".into(),
            project_id: "p1".into(),
            project_name: "P".into(),
        };
        let doc_json = json!({
            "id": "0:0", "name": "doc", "type": "DOCUMENT",
            "children": canvases.iter().map(|(id, type_, name)| json!({
                "id": *id, "type": *type_, "name": *name,
            })).collect::<Vec<_>>(),
        });
        let payload = build_cached_file(&file_ref, &doc_json, 0);
        let meta = FileMeta::from_success(&file_ref, &payload, 0, 0);
        let children: Vec<CacheNode> = canvases
            .into_iter()
            .map(|(id, type_, name)| CacheNode {
                id: id.into(),
                type_: type_.into(),
                name: name.into(),
                visible: true,
                bounds: None,
                children: vec![],
            })
            .collect();
        let document = CacheNode {
            id: "0:0".into(),
            type_: "DOCUMENT".into(),
            name: "doc".into(),
            visible: true,
            bounds: None,
            children,
        };
        (meta, document)
    }

    #[test]
    fn synthesize_file_root_hides_default_canvases() {
        let (meta, doc) = document_with_canvases(vec![
            ("0:1", "CANVAS", "Cover"),
            ("0:2", "CANVAS", "Home"),
            ("0:3", "CANVAS", "WIP"),
            ("0:4", "CANVAS", "Settings"),
            ("0:5", "CANVAS", "Archive"),
        ]);
        let mut hidden = BTreeSet::new();
        let root = synthesize_file_root(&meta, &doc, false, &mut hidden);
        let names: Vec<&str> = root.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Home", "Settings"]);
        assert_eq!(
            hidden.iter().copied().collect::<Vec<_>>(),
            vec!["Archive", "Cover", "WIP"],
            "expected canonical sorted names"
        );
    }

    #[test]
    fn synthesize_file_root_case_insensitive() {
        let (meta, doc) = document_with_canvases(vec![
            ("0:1", "CANVAS", "cover"),
            ("0:2", "CANVAS", "WiP"),
            ("0:3", "CANVAS", "ARCHIVE"),
        ]);
        let mut hidden = BTreeSet::new();
        let root = synthesize_file_root(&meta, &doc, false, &mut hidden);
        assert!(root.children.is_empty(), "all three should be hidden");
        // Labels come from the canonical const, not the designer's casing.
        assert_eq!(
            hidden.iter().copied().collect::<Vec<_>>(),
            vec!["Archive", "Cover", "WIP"],
        );
    }

    #[test]
    fn synthesize_file_root_only_filters_canvas_type() {
        // A FRAME named "Cover" must survive — the filter is CANVAS-only.
        let (meta, doc) =
            document_with_canvases(vec![("0:1", "FRAME", "Cover"), ("0:2", "CANVAS", "Cover")]);
        let mut hidden = BTreeSet::new();
        let root = synthesize_file_root(&meta, &doc, false, &mut hidden);
        let kept: Vec<(&str, &str)> = root
            .children
            .iter()
            .map(|c| (c.type_.as_str(), c.name.as_str()))
            .collect();
        assert_eq!(kept, vec![("FRAME", "Cover")]);
        assert_eq!(hidden.iter().copied().collect::<Vec<_>>(), vec!["Cover"]);
    }

    #[test]
    fn synthesize_file_root_show_all_disables_filter() {
        let (meta, doc) = document_with_canvases(vec![
            ("0:1", "CANVAS", "Cover"),
            ("0:2", "CANVAS", "Home"),
            ("0:3", "CANVAS", "Archive"),
        ]);
        let mut hidden = BTreeSet::new();
        let root = synthesize_file_root(&meta, &doc, true, &mut hidden);
        let names: Vec<&str> = root.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Cover", "Home", "Archive"]);
        assert!(
            hidden.is_empty(),
            "show_all=true must not accumulate hidden names"
        );
    }

    #[test]
    fn ignored_canvas_label_returns_canonical_const_reference() {
        // Sanity: the returned label is a &'static str pointing at the const
        // array, so it stays cheap to insert into BTreeSet<&'static str>.
        let n = CacheNode {
            id: "0:1".into(),
            type_: "CANVAS".into(),
            name: "cover".into(),
            visible: true,
            bounds: None,
            children: vec![],
        };
        assert_eq!(ignored_canvas_label(&n), Some("Cover"));
    }
}

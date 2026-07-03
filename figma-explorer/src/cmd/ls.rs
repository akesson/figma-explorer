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
    collect_visible, comment_count_suffix, comment_row_json, group_comments,
    render_flat_with_comment_counts, render_flat_with_comments, render_subtree_json_with_comments,
    truncate_display, CommentRow, NAME_DISPLAY_MAX,
};
use crate::{print, Globals, Output};

/// Default descent depth. Depth counts levels below "self" (the same
/// convention `render_flat` uses). At the root this means projects (depth 0),
/// files (depth 1), canvases (depth 2), frames (depth 3). At `file:N` it
/// means file (depth 0), canvases (depth 1), and so on.
const DEFAULT_DEPTH: usize = 3;

/// Root listings (no ID) default shallower than [`DEFAULT_DEPTH`]: just
/// projects + files, no descent into canvases/frames. A full workspace can be
/// dozens of files; at depth 3 that dump measured ~237KB. The drill-down
/// commands (`ls file:N`, `ls proj:N`, or `--depth 2`) are one paste away.
const ROOT_DEFAULT_DEPTH: usize = 1;

/// Resolve the descent depth: an explicit `--depth` always wins; otherwise
/// root listings default to [`ROOT_DEFAULT_DEPTH`] and every other target to
/// [`DEFAULT_DEPTH`].
fn effective_depth(explicit: Option<usize>, is_root: bool) -> usize {
    explicit.unwrap_or(if is_root {
        ROOT_DEFAULT_DEPTH
    } else {
        DEFAULT_DEPTH
    })
}

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

/// List a node and its descendants. Honors `--depth` at every level (default 3;
/// root listings default to 1 — projects + files only). By default comment
/// threads are summarized: a node with anchored threads shows a `[N comments]`
/// suffix and file targets get a one-line thread-count header. Pass
/// `--comments` to interleave the individual threads inline.
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

    /// Render individual comment thread rows inline under their anchor nodes
    /// (YAML). Off by default: anchored threads collapse to a `[N comments]`
    /// suffix plus a `#` thread-count header on file targets. JSON always
    /// carries the full comment arrays regardless of this flag.
    #[arg(long)]
    pub comments: bool,

    /// Only show resolved (`true`) or unresolved (`false`) comment threads.
    /// Filters the inline thread rows, so it requires `--comments`. Replies
    /// aren't rendered as separate rows in `ls` regardless.
    #[arg(long, requires = "comments")]
    pub resolved: Option<bool>,

    /// Only show nodes whose name contains PATTERN (case-insensitive
    /// substring). Ancestors of matching nodes are kept for tree context;
    /// other branches are pruned. Matching happens within the --depth
    /// budget. Files/projects with no matches inside are dropped from
    /// root/project listings unless their own name matches.
    #[arg(long, value_name = "PATTERN")]
    pub name: Option<String>,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        let resolver = Resolver::new(globals.cache_only)?;
        let format = globals.output;
        let explicit_depth = self.depth;

        let show_all = self.no_ignore;
        let resolved = self.resolved;
        let inline_comments = self.comments;
        // Lowercase the needle once; case-insensitive substring matching
        // downstream. (`--name ""` matches everything — a harmless no-op.)
        let name_filter = self.name.as_deref().map(str::to_lowercase);
        let name_filter = name_filter.as_deref();
        match self.id.as_deref() {
            None => render_root(
                &resolver,
                effective_depth(explicit_depth, true),
                explicit_depth.is_none(),
                format,
                show_all,
                resolved,
                inline_comments,
                name_filter,
            ),
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
                    ResolvedTarget::Root => render_root(
                        &resolver,
                        effective_depth(explicit_depth, true),
                        explicit_depth.is_none(),
                        format,
                        show_all,
                        resolved,
                        inline_comments,
                        name_filter,
                    ),
                    ResolvedTarget::Project { synth, project_id } => render_project(
                        &resolver,
                        synth,
                        &project_id,
                        effective_depth(explicit_depth, false),
                        format,
                        show_all,
                        resolved,
                        inline_comments,
                        name_filter,
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
                        effective_depth(explicit_depth, false),
                        format,
                        show_all,
                        resolved,
                        inline_comments,
                        name_filter,
                    ),
                    ResolvedTarget::Node {
                        file_synth,
                        meta,
                        node,
                    } => {
                        // The canvas-ignore filter intentionally does not apply
                        // when the user has already drilled into a specific node
                        // — drilling in is explicit, that filter is for
                        // browsing. `--name` DOES apply here (pruning inside a
                        // subtree is its point).
                        render_node_subtree(
                            &resolver,
                            file_synth,
                            &meta,
                            &node,
                            effective_depth(explicit_depth, false),
                            format,
                            resolved,
                            inline_comments,
                            name_filter,
                        )
                    }
                    ResolvedTarget::Comment { .. } => {
                        anyhow::bail!(
                            "ls does not accept comment ids; use `node-info` for a comment"
                        );
                    }
                }
            }
        }
    }
}

/// One project's listing data: synth, project id, display name, and its files
/// paired with their file synths. Named to keep the `render_root` grouping
/// vector readable (and clippy happy about nested generics).
type ProjectGroup = (u32, String, String, Vec<(u32, FileMeta)>);

/// Build the YAML rows for one project group: a PROJECT header, then a FILE
/// header per file, descending into each file's tree when `depth >= 2`.
/// Shared by `render_root` (called per project) and `render_project` (once).
///
/// With a `name_filter`, file sections whose pruned tree came up empty are
/// dropped unless the file's own name matches, and the whole project block
/// is dropped (empty vec) when no file survived and the project name doesn't
/// match either.
#[allow(clippy::too_many_arguments)]
fn project_rows(
    resolver: &Resolver,
    project_synth: u32,
    project_name: &str,
    files: &[(u32, FileMeta)],
    depth: usize,
    show_all: bool,
    hidden: &mut BTreeSet<&'static str>,
    resolved_filter: Option<bool>,
    inline_comments: bool,
    name_filter: Option<&str>,
    match_count: &mut usize,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let project_name_matches = name_filter
        .map(|needle| project_name.to_lowercase().contains(needle))
        .unwrap_or(false);
    if project_name_matches {
        *match_count += 1;
    }
    rows.push(Row::header(
        format!("proj:{project_synth}"),
        0,
        "PROJECT",
        project_name.to_owned(),
    ));
    let mut any_file_kept = false;
    if depth >= 1 {
        for (fs, fm) in files {
            let file_name_matches = name_filter
                .map(|needle| fm.name.to_lowercase().contains(needle))
                .unwrap_or(false);
            let mut section = vec![Row::header(
                format!("file:{fs}"),
                1,
                "FILE",
                fm.name.clone(),
            )];
            if depth >= 2 {
                append_descent_rows(
                    resolver,
                    *fs,
                    fm,
                    depth,
                    1,
                    &mut section,
                    show_all,
                    hidden,
                    resolved_filter,
                    inline_comments,
                    name_filter,
                    match_count,
                );
            }
            if name_filter.is_some() && section.len() == 1 && !file_name_matches {
                continue;
            }
            if file_name_matches {
                *match_count += 1;
            }
            any_file_kept = true;
            rows.extend(section);
        }
    }
    if name_filter.is_some() && !any_file_kept && !project_name_matches {
        return Vec::new();
    }
    rows
}

/// Build the JSON `files` array for one project group. Empty when `depth < 1`
/// (listing projects only). Shared by `render_root` and `render_project`.
/// With a `name_filter`, files whose pruned tree came up empty (and whose
/// own name doesn't match) are omitted.
#[allow(clippy::too_many_arguments)]
fn project_files_json(
    resolver: &Resolver,
    files: &[(u32, FileMeta)],
    depth: usize,
    show_all: bool,
    hidden: &mut BTreeSet<&'static str>,
    resolved_filter: Option<bool>,
    name_filter: Option<&str>,
    match_count: &mut usize,
) -> Vec<Value> {
    if depth < 1 {
        return Vec::new();
    }
    files
        .iter()
        .filter_map(|(fs, fm)| {
            build_file_json(
                resolver,
                *fs,
                fm,
                depth,
                show_all,
                hidden,
                resolved_filter,
                name_filter,
                match_count,
            )
        })
        .collect()
}

/// Root listing — projects + their files, recursing into each file's
/// canvases/frames when `depth >= 2`. Reads structural data directly from
/// the cache; files whose payload is missing or fails to decode are emitted
/// as a file row only (no descent), so a partially populated cache still
/// produces useful output.
#[allow(clippy::too_many_arguments)]
fn render_root(
    resolver: &Resolver,
    depth: usize,
    depth_hint: bool,
    format: Output,
    show_all: bool,
    resolved_filter: Option<bool>,
    inline_comments: bool,
    name_filter: Option<&str>,
) -> Result<()> {
    let synth = resolver.synth();
    let metas = resolver.cache().list_metas()?;

    // Group OK files by project synth, pre-resolving each file's synth so the
    // render helpers don't have to re-look-it-up (and so synth-less files are
    // simply skipped rather than panicking).
    let mut groups: Vec<ProjectGroup> = Vec::new();
    for (project_id, &project_synth) in &synth.projects {
        let mut files: Vec<(u32, FileMeta)> = metas
            .iter()
            .filter(|m| m.status == EntryStatus::Ok && m.project_id == *project_id)
            .filter_map(|m| synth.file_synth(&m.file_key).map(|s| (s, m.clone())))
            .collect();
        files.sort_by_key(|(_, m)| m.name.to_lowercase());
        let project_name = derive_project_name(&metas, project_id);
        groups.push((project_synth, project_id.clone(), project_name, files));
    }
    groups.sort_by_key(|(s, _, _, _)| *s);

    let mut hidden: BTreeSet<&'static str> = BTreeSet::new();
    let mut matches = 0usize;

    match format {
        Output::Yaml => {
            if depth_hint {
                println!(
                    "# depth {ROOT_DEFAULT_DEPTH} (projects + files) — use `ls file:N` / `ls proj:N` or --depth 2 to descend"
                );
            }
            let mut rows: Vec<Row> = Vec::new();
            for (psynth, _pid, pname, files) in &groups {
                rows.extend(project_rows(
                    resolver,
                    *psynth,
                    pname,
                    files,
                    depth,
                    show_all,
                    &mut hidden,
                    resolved_filter,
                    inline_comments,
                    name_filter,
                    &mut matches,
                ));
            }
            print_hidden_comment(&hidden);
            if let Some(pattern) = name_filter {
                print_name_filter_comment(pattern, matches);
            }
            print!("{}", format_rows(&rows));
            Ok(())
        }
        Output::Json => {
            let projects: Vec<Value> = groups
                .iter()
                .filter_map(|(ps, pid, pname, files)| {
                    let file_jsons = project_files_json(
                        resolver,
                        files,
                        depth,
                        show_all,
                        &mut hidden,
                        resolved_filter,
                        name_filter,
                        &mut matches,
                    );
                    if let Some(needle) = name_filter {
                        let pname_matches = pname.to_lowercase().contains(needle);
                        if file_jsons.is_empty() && !pname_matches {
                            return None;
                        }
                        if pname_matches {
                            matches += 1;
                        }
                    }
                    Some(json!({
                        "id": format!("proj:{ps}"),
                        "project_id": pid,
                        "name": pname,
                        "files": file_jsons,
                    }))
                })
                .collect();
            let mut payload = serde_json::Map::new();
            payload.insert("ignored".into(), ignored_json(&hidden));
            if let Some(pattern) = name_filter {
                payload.insert("name_filter".into(), name_filter_json(pattern, matches));
            }
            payload.insert("projects".into(), Value::Array(projects));
            print(&Value::Object(payload), format)
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
    /// Counts-mode `[N comments]` suffix count for this node's anchored
    /// threads. `None` in inline mode (threads render as their own rows) and
    /// for nodes with no anchored threads.
    comment_count: Option<usize>,
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
            comment_count: None,
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
            comment_count: None,
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
        if let Some(c) = r.comment_count {
            out.push_str(&comment_count_suffix(c));
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
#[allow(clippy::too_many_arguments)]
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
    inline_comments: bool,
    name_filter: Option<&str>,
    match_count: &mut usize,
) {
    let cached = match resolver.cache().read_file(&fm.file_key) {
        Ok(Some(c)) => c,
        _ => return,
    };
    let mut synthetic = synthesize_file_root(fm, &cached.document, show_all, hidden);
    // The synthesized root itself represents the FILE row, already emitted
    // by the caller; descend its children up to `depth - file_depth` levels.
    let max_sub_depth = depth.saturating_sub(file_depth);
    if let Some(needle) = name_filter {
        synthetic = prune_root_children(&synthetic, needle, max_sub_depth, match_count);
    }
    let mut comment_rows = load_comment_rows(resolver, &fm.file_key, file_synth, resolved_filter);
    if name_filter.is_some() {
        // A filtered listing shows matched nodes + their anchored threads;
        // canvas-level (unanchored) threads are browsing noise here.
        comment_rows.retain(|r| r.anchor_node_id.is_some());
    }
    let (by_node, canvas) = group_comments(&comment_rows);
    let mut tuples: Vec<(&CacheNode, usize, Option<usize>)> = Vec::new();
    collect_visible(&synthetic, 0, max_sub_depth, &mut tuples);
    for (node, sub_depth, truncated) in tuples.into_iter().skip(1) {
        let kind = if node.type_.is_empty() {
            "?".to_owned()
        } else {
            node.type_.clone()
        };
        let row_depth = file_depth + sub_depth;
        let anchored = by_node.get(node.id.as_str());
        // Counts mode: fold the anchored-thread count into a `[N comments]`
        // suffix on the node row instead of emitting a row per thread.
        let comment_count = if inline_comments {
            None
        } else {
            anchored.map(|a| a.len())
        };
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
            comment_count,
        });
        if inline_comments {
            if let Some(anchored) = anchored {
                for cr in anchored {
                    rows.push(Row::comment(
                        cr.id.clone(),
                        row_depth + 1,
                        cr.display.clone(),
                    ));
                }
            }
        }
    }
    // Canvas-level threads fall under the FILE row at file_depth + 1 — only in
    // inline mode; in counts mode the file-level header accounts for them.
    if inline_comments {
        for cr in canvas {
            rows.push(Row::comment(
                cr.id.clone(),
                file_depth + 1,
                cr.display.clone(),
            ));
        }
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

/// Read the pre-associated comments sidecar for `file_key`, tolerating a
/// missing/unreadable/empty sidecar by returning an empty vector. Unlike
/// [`load_comment_rows`] this hands back the raw `AssociatedComment`s so a
/// caller can both derive [`CommentRow`]s and compute [`comment_stats`] from a
/// single read.
fn read_comments_lenient(resolver: &Resolver, file_key: &str) -> Vec<AssociatedComment> {
    resolver
        .cache()
        .read_comments(file_key)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Thread-count summary for a file's comment sidecar. Counts thread heads
/// (`parent_id == None`), including canvas-level (unanchored) threads.
struct CommentStats {
    total_threads: usize,
    unresolved: usize,
}

/// Tally thread heads and how many are still open. Replies are ignored.
fn comment_stats(comments: &[AssociatedComment]) -> CommentStats {
    let mut total_threads = 0usize;
    let mut unresolved = 0usize;
    for c in comments {
        if c.parent_id.is_some() {
            continue;
        }
        total_threads += 1;
        if c.resolved_at.is_none() {
            unresolved += 1;
        }
    }
    CommentStats {
        total_threads,
        unresolved,
    }
}

/// The counts-mode `#` header for a file target: how many comment threads the
/// file carries and how to read them. `None` when the file has no threads
/// (stay silent rather than print a zero).
fn comment_summary_header(file_synth: u32, stats: &CommentStats) -> Option<String> {
    if stats.total_threads == 0 {
        return None;
    }
    let label = if stats.total_threads == 1 {
        "comment thread"
    } else {
        "comment threads"
    };
    Some(format!(
        "# {} {label} ({} unresolved) — use: comments file:{file_synth} [--grep <word>]",
        stats.total_threads, stats.unresolved
    ))
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
/// Returns `None` when a `name_filter` is active, nothing under the file
/// matched, and the file's own name doesn't match either.
#[allow(clippy::too_many_arguments)]
fn build_file_json(
    resolver: &Resolver,
    file_synth: u32,
    fm: &FileMeta,
    depth: usize,
    show_all: bool,
    hidden: &mut BTreeSet<&'static str>,
    resolved_filter: Option<bool>,
    name_filter: Option<&str>,
    match_count: &mut usize,
) -> Option<Value> {
    let file_name_matches = name_filter
        .map(|needle| fm.name.to_lowercase().contains(needle))
        .unwrap_or(false);
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(format!("file:{file_synth}")));
    obj.insert("file_key".into(), json!(fm.file_key));
    obj.insert("name".into(), json!(fm.name));
    obj.insert("last_modified".into(), json!(fm.last_modified));
    let mut comment_rows = load_comment_rows(resolver, &fm.file_key, file_synth, resolved_filter);
    if name_filter.is_some() {
        comment_rows.retain(|r| r.anchor_node_id.is_some());
    }
    let mut any_node_kept = false;
    if depth >= 2 {
        if let Ok(Some(cached)) = resolver.cache().read_file(&fm.file_key) {
            let mut synthetic = synthesize_file_root(fm, &cached.document, show_all, hidden);
            if let Some(needle) = name_filter {
                synthetic = prune_root_children(&synthetic, needle, depth - 1, match_count);
            }
            any_node_kept = !synthetic.children.is_empty();
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
    if name_filter.is_some() && !any_node_kept && !file_name_matches {
        return None;
    }
    if file_name_matches {
        *match_count += 1;
    }
    // Canvas-level threads attach to the file root regardless of depth.
    let canvas_json: Vec<Value> = comment_rows
        .iter()
        .filter(|r| r.anchor_node_id.is_none())
        .map(comment_row_json)
        .collect();
    if !canvas_json.is_empty() {
        obj.insert("canvas_comments".into(), Value::Array(canvas_json));
    }
    Some(Value::Object(obj))
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
#[allow(clippy::too_many_arguments)]
fn render_project(
    resolver: &Resolver,
    project_synth: u32,
    project_id: &str,
    depth: usize,
    format: Output,
    show_all: bool,
    resolved_filter: Option<bool>,
    inline_comments: bool,
    name_filter: Option<&str>,
) -> Result<()> {
    let synth = resolver.synth();
    let metas = resolver.cache().list_metas()?;
    let project_name = derive_project_name(&metas, project_id);
    let mut files: Vec<(u32, FileMeta)> = metas
        .iter()
        .filter(|m| m.status == EntryStatus::Ok && m.project_id == project_id)
        .filter_map(|m| synth.file_synth(&m.file_key).map(|s| (s, m.clone())))
        .collect();
    files.sort_by_key(|(_, m)| m.name.to_lowercase());

    let mut hidden: BTreeSet<&'static str> = BTreeSet::new();
    let mut matches = 0usize;

    match format {
        Output::Yaml => {
            let rows = project_rows(
                resolver,
                project_synth,
                &project_name,
                &files,
                depth,
                show_all,
                &mut hidden,
                resolved_filter,
                inline_comments,
                name_filter,
                &mut matches,
            );
            print_hidden_comment(&hidden);
            if let Some(pattern) = name_filter {
                print_name_filter_comment(pattern, matches);
            }
            print!("{}", format_rows(&rows));
            Ok(())
        }
        Output::Json => {
            let file_jsons = project_files_json(
                resolver,
                &files,
                depth,
                show_all,
                &mut hidden,
                resolved_filter,
                name_filter,
                &mut matches,
            );
            let mut payload = serde_json::Map::new();
            payload.insert("id".into(), json!(format!("proj:{project_synth}")));
            payload.insert("project_id".into(), json!(project_id));
            payload.insert("name".into(), json!(project_name));
            payload.insert("ignored".into(), ignored_json(&hidden));
            if let Some(pattern) = name_filter {
                payload.insert("name_filter".into(), name_filter_json(pattern, matches));
            }
            payload.insert("files".into(), Value::Array(file_jsons));
            print(&Value::Object(payload), format)
        }
    }
}

/// File-level listing. We synthesize a fake root with the file's name and the
/// DOCUMENT's children, so the user sees `file:N FILE "name"` at the top and
/// the actual `0:0` DOCUMENT node stays hidden — in both YAML and JSON paths.
#[allow(clippy::too_many_arguments)]
fn render_file(
    resolver: &Resolver,
    file_synth: u32,
    meta: &FileMeta,
    document: &CacheNode,
    depth: usize,
    format: Output,
    show_all: bool,
    resolved_filter: Option<bool>,
    inline_comments: bool,
    name_filter: Option<&str>,
) -> Result<()> {
    let mut hidden: BTreeSet<&'static str> = BTreeSet::new();
    let mut synthetic_root = synthesize_file_root(meta, document, show_all, &mut hidden);
    let mut matches = 0usize;
    if let Some(needle) = name_filter {
        synthetic_root = prune_root_children(&synthetic_root, needle, depth, &mut matches);
    }
    // Read the raw sidecar once: the summary header needs thread/unresolved
    // counts (including canvas-level threads), while the rows need CommentRows.
    let assoc = read_comments_lenient(resolver, &meta.file_key);
    let mut comment_rows = build_comment_rows(&assoc, resolver, file_synth, resolved_filter);
    if name_filter.is_some() {
        comment_rows.retain(|r| r.anchor_node_id.is_some());
    }
    match format {
        Output::Yaml => {
            print_hidden_comment(&hidden);
            if let Some(pattern) = name_filter {
                print_name_filter_comment(pattern, matches);
            }
            if inline_comments {
                let lines =
                    render_flat_with_comments(&synthetic_root, file_synth, depth, &comment_rows);
                println!("{}", lines.join("\n"));
            } else {
                if let Some(header) = comment_summary_header(file_synth, &comment_stats(&assoc)) {
                    println!("{header}");
                }
                let lines = render_flat_with_comment_counts(
                    &synthetic_root,
                    file_synth,
                    depth,
                    &comment_rows,
                );
                println!("{}", lines.join("\n"));
            }
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
            if let Some(pattern) = name_filter {
                payload.insert("name_filter".into(), name_filter_json(pattern, matches));
            }
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
#[allow(clippy::too_many_arguments)]
fn render_node_subtree(
    resolver: &Resolver,
    file_synth: u32,
    meta: &FileMeta,
    node: &CacheNode,
    depth: usize,
    format: Output,
    resolved_filter: Option<bool>,
    inline_comments: bool,
    name_filter: Option<&str>,
) -> Result<()> {
    let mut matches = 0usize;
    let node = match name_filter {
        Some(needle) => &prune_root_children(node, needle, depth, &mut matches),
        None => node,
    };
    let mut comment_rows = load_comment_rows(resolver, &meta.file_key, file_synth, resolved_filter);
    if name_filter.is_some() {
        comment_rows.retain(|r| r.anchor_node_id.is_some());
    }
    match format {
        Output::Yaml => {
            if let Some(pattern) = name_filter {
                print_name_filter_comment(pattern, matches);
            }
            // No file-level thread-count header here — a subtree only shows a
            // slice of the file's threads, so a whole-file count would mislead.
            // Per-node `[N comments]` suffixes remain accurate.
            let lines = if inline_comments {
                render_flat_with_comments(node, file_synth, depth, &comment_rows)
            } else {
                render_flat_with_comment_counts(node, file_synth, depth, &comment_rows)
            };
            println!("{}", lines.join("\n"));
            Ok(())
        }
        Output::Json => {
            let mut payload = serde_json::Map::new();
            payload.insert("id".into(), json!(format!("file:{file_synth}:{}", node.id)));
            payload.insert("file_key".into(), json!(meta.file_key));
            if let Some(pattern) = name_filter {
                payload.insert("name_filter".into(), name_filter_json(pattern, matches));
            }
            payload.insert(
                "items".into(),
                render_subtree_json_with_comments(file_synth, node, depth, &comment_rows),
            );
            print(&Value::Object(payload), format)
        }
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
        characters: None,
        children,
    }
}

/// Prune `node`'s subtree to name matches ∪ their ancestors, within the
/// `max_depth` render budget (`depth` = node's own level below the render
/// root; nodes deeper than the budget are neither matched nor kept). Returns
/// `None` when nothing at or under `node` matches. Grep semantics: a matched
/// node does NOT keep its non-matching descendants — drilling in is one
/// `ls <id>` away — EXCEPT at the depth boundary, where the raw clone keeps
/// its (never-rendered) children so the `[+N children]` truncation marker
/// still reports the unfiltered count. Increments `match_count` per matched
/// node.
fn filter_tree_by_name(
    node: &CacheNode,
    needle_lower: &str,
    depth: usize,
    max_depth: usize,
    match_count: &mut usize,
) -> Option<CacheNode> {
    if !node.visible {
        return None;
    }
    let self_matches = node.name.to_lowercase().contains(needle_lower);
    if depth >= max_depth {
        if self_matches {
            *match_count += 1;
            return Some(node.clone());
        }
        return None;
    }
    let kept: Vec<CacheNode> = node
        .children
        .iter()
        .filter_map(|c| filter_tree_by_name(c, needle_lower, depth + 1, max_depth, match_count))
        .collect();
    if self_matches {
        *match_count += 1;
        return Some(CacheNode {
            children: kept,
            ..node.clone()
        });
    }
    if kept.is_empty() {
        return None;
    }
    Some(CacheNode {
        children: kept,
        ..node.clone()
    })
}

/// Apply [`filter_tree_by_name`] below an always-kept render root (the
/// synthesized FILE node or an explicitly-targeted node): the root row stays,
/// its descendants are pruned. The root's own name does not count as a match.
fn prune_root_children(
    root: &CacheNode,
    needle_lower: &str,
    max_depth: usize,
    match_count: &mut usize,
) -> CacheNode {
    let children: Vec<CacheNode> = root
        .children
        .iter()
        .filter_map(|c| filter_tree_by_name(c, needle_lower, 1, max_depth, match_count))
        .collect();
    CacheNode {
        children,
        ..root.clone()
    }
}

/// The `# name filter …` YAML transparency line — printed whenever the
/// filter is active, including on zero matches (a silent empty listing reads
/// as "no children" otherwise).
fn print_name_filter_comment(pattern: &str, matches: usize) {
    let label = if matches == 1 { "match" } else { "matches" };
    println!("# name filter \"{pattern}\": {matches} {label}");
}

/// The JSON counterpart of the transparency line.
fn name_filter_json(pattern: &str, matches: usize) -> Value {
    json!({ "pattern": pattern, "matches": matches })
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
    fn apply_scope_promotes_bare_instance_node_under_file_scope() {
        let id: Id = "I880:3606;2816:36646".parse().unwrap();
        let promoted = apply_scope(id, Some("file:7")).unwrap();
        assert_eq!(
            promoted,
            Id::Node {
                file: 7,
                node: "I880:3606;2816:36646".into()
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

    #[test]
    fn effective_depth_defaults_root_to_one() {
        assert_eq!(effective_depth(None, true), ROOT_DEFAULT_DEPTH);
        assert_eq!(effective_depth(None, false), DEFAULT_DEPTH);
        // Explicit --depth always wins, root or not.
        assert_eq!(effective_depth(Some(5), true), 5);
        assert_eq!(effective_depth(Some(0), false), 0);
    }

    #[test]
    fn format_rows_appends_comment_count_suffix() {
        let mut row = Row::header("file:1:1:2".into(), 0, "FRAME", "Header".into());
        row.comment_count = Some(3);
        assert!(
            format_rows(&[row]).contains("  [3 comments]"),
            "expected pluralized suffix"
        );
        let mut one = Row::header("file:1:1:3".into(), 0, "FRAME", "Footer".into());
        one.comment_count = Some(1);
        assert!(format_rows(&[one]).contains("  [1 comment]"), "singular");
        let none = Row::header("file:1:1:4".into(), 0, "FRAME", "Body".into());
        assert!(
            !format_rows(&[none]).contains("comment"),
            "no suffix when None"
        );
    }

    /// Minimal `AssociatedComment` for the stats tests: a head or reply, open
    /// or resolved, anchored to a node id or canvas-level (`None`).
    fn stat_comment(
        id: &str,
        parent: Option<&str>,
        resolved: bool,
        node_id: Option<&str>,
    ) -> AssociatedComment {
        use crate::comment_assoc::{Anchor, AnchorKind, AssociationMethod, NodeRef};
        AssociatedComment {
            comment_id: id.into(),
            message: format!("msg {id}"),
            author: "a".into(),
            created_at: "2026-01-01".into(),
            resolved_at: resolved.then(|| "2026-01-02".to_string()),
            parent_id: parent.map(str::to_string),
            order_id: None,
            reactions: 0,
            anchor: Anchor {
                kind: AnchorKind::FrameOffset,
                explicit_node_id: node_id.map(str::to_string),
                canvas_point: None,
                canvas_rect: None,
            },
            node: node_id.map(|n| NodeRef {
                node_id: n.into(),
                type_: "FRAME".into(),
                name: "N".into(),
                path: vec![],
            }),
            method: AssociationMethod::Explicit,
            stale_node_id: None,
        }
    }

    #[test]
    fn comment_stats_counts_heads_including_canvas_level() {
        let comments = vec![
            stat_comment("h1", None, false, Some("1:2")), // open, anchored
            stat_comment("h2", None, true, None),         // resolved, canvas-level
            stat_comment("r1", Some("h1"), false, Some("1:2")), // reply — ignored
        ];
        let stats = comment_stats(&comments);
        assert_eq!(stats.total_threads, 2, "two heads, reply excluded");
        assert_eq!(stats.unresolved, 1, "only h1 is open");
    }

    #[test]
    fn comment_summary_header_formats_and_hides_zero() {
        let stats = CommentStats {
            total_threads: 52,
            unresolved: 12,
        };
        let header = comment_summary_header(15, &stats).unwrap();
        assert_eq!(
            header,
            "# 52 comment threads (12 unresolved) — use: comments file:15 [--grep <word>]"
        );
        // Singular label + zero-thread suppression.
        assert!(comment_summary_header(
            15,
            &CommentStats {
                total_threads: 1,
                unresolved: 0
            }
        )
        .unwrap()
        .contains("1 comment thread ("));
        assert!(comment_summary_header(
            15,
            &CommentStats {
                total_threads: 0,
                unresolved: 0
            }
        )
        .is_none());
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
                characters: None,
                children: vec![],
            })
            .collect();
        let document = CacheNode {
            id: "0:0".into(),
            type_: "DOCUMENT".into(),
            name: "doc".into(),
            visible: true,
            bounds: None,
            characters: None,
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
            characters: None,
            children: vec![],
        };
        assert_eq!(ignored_canvas_label(&n), Some("Cover"));
    }

    fn leaf_node(id: &str, type_: &str, name: &str, children: Vec<CacheNode>) -> CacheNode {
        CacheNode {
            id: id.into(),
            type_: type_.into(),
            name: name.into(),
            visible: true,
            bounds: None,
            characters: None,
            children,
        }
    }

    /// Three-level fixture for the `--name` filter tests:
    /// Page ─ Header ─ Search  |  Page ─ Footer ─ Links
    fn name_filter_fixture() -> CacheNode {
        let search = leaf_node("2:1", "INSTANCE", "Search", vec![]);
        let header = leaf_node("1:1", "FRAME", "Header", vec![search]);
        let links = leaf_node("2:2", "TEXT", "Links", vec![]);
        let footer = leaf_node("1:2", "FRAME", "Footer", vec![links]);
        leaf_node("0:1", "CANVAS", "Page", vec![header, footer])
    }

    #[test]
    fn name_filter_keeps_match_and_ancestors_drops_siblings() {
        let root = name_filter_fixture();
        let mut count = 0;
        let pruned = filter_tree_by_name(&root, "search", 0, 5, &mut count).unwrap();
        assert_eq!(count, 1);
        assert_eq!(pruned.children.len(), 1, "Footer branch pruned");
        assert_eq!(pruned.children[0].name, "Header");
        assert_eq!(pruned.children[0].children[0].name, "Search");
    }

    #[test]
    fn name_filter_is_case_insensitive_substring() {
        let root = name_filter_fixture();
        let mut count = 0;
        // Needle is pre-lowercased by run(); "earch" ⊂ "Search".
        let pruned = filter_tree_by_name(&root, "earch", 0, 5, &mut count);
        assert!(pruned.is_some());
        assert_eq!(count, 1);
    }

    #[test]
    fn name_filter_matched_node_drops_nonmatching_children() {
        let root = name_filter_fixture();
        let mut count = 0;
        let pruned = filter_tree_by_name(&root, "header", 0, 5, &mut count).unwrap();
        assert_eq!(count, 1);
        let header = &pruned.children[0];
        assert_eq!(header.name, "Header");
        assert!(
            header.children.is_empty(),
            "matched node keeps no non-matching descendants"
        );
    }

    #[test]
    fn name_filter_respects_depth_budget() {
        let root = name_filter_fixture();
        let mut count = 0;
        // "Search" sits at depth 2 below the canvas; budget 1 → no match.
        assert!(filter_tree_by_name(&root, "search", 0, 1, &mut count).is_none());
        assert_eq!(count, 0);
    }

    #[test]
    fn name_filter_boundary_match_keeps_raw_children_for_truncation() {
        let root = name_filter_fixture();
        let mut count = 0;
        // "Header" matches exactly at the depth boundary (1) — the raw clone
        // keeps its children so `[+N children]` counts stay unfiltered.
        let pruned = filter_tree_by_name(&root, "header", 0, 1, &mut count).unwrap();
        assert_eq!(count, 1);
        assert_eq!(pruned.children[0].children.len(), 1);
    }

    #[test]
    fn name_filter_zero_matches_returns_none() {
        let root = name_filter_fixture();
        let mut count = 0;
        assert!(filter_tree_by_name(&root, "nonexistent", 0, 5, &mut count).is_none());
        assert_eq!(count, 0);
    }

    #[test]
    fn name_filter_skips_invisible_nodes() {
        let mut hidden_node = leaf_node("3:1", "FRAME", "Search hidden", vec![]);
        hidden_node.visible = false;
        let root = leaf_node("0:1", "CANVAS", "Page", vec![hidden_node]);
        let mut count = 0;
        assert!(filter_tree_by_name(&root, "search", 0, 5, &mut count).is_none());
        assert_eq!(count, 0);
    }

    #[test]
    fn prune_root_children_always_keeps_root() {
        let root = name_filter_fixture();
        let mut count = 0;
        // Root's own name matches nothing; children all pruned — root stays.
        let pruned = prune_root_children(&root, "nonexistent", 5, &mut count);
        assert_eq!(pruned.name, "Page");
        assert!(pruned.children.is_empty());
        assert_eq!(count, 0);
    }

    /// Characterization guard for the comment-aware JSON subtree renderer:
    /// id formatting, per-node comment attachment, canvas-level exclusion, and
    /// depth truncation. Locks behavior across the group_comments rewire + the
    /// relocation of this function into `tree.rs`.
    #[test]
    fn render_subtree_json_attaches_comments_and_truncates() {
        let leaf = leaf_node("1:3", "RECTANGLE", "leaf", vec![]);
        let mid = leaf_node("1:2", "FRAME", "mid", vec![leaf]);
        let root = leaf_node("0:1", "CANVAS", "Page", vec![mid]);
        let comment_rows = vec![
            CommentRow {
                id: "file:1:comm:1".into(),
                anchor_node_id: Some("1:2".into()),
                display: "\"hi\"  by @a".into(),
            },
            CommentRow {
                id: "file:1:comm:2".into(),
                anchor_node_id: None,
                display: "\"canvas\"  by @b".into(),
            },
        ];

        // depth 0 → root only, children collapsed into a truncated marker.
        let v0 = render_subtree_json_with_comments(1, &root, 0, &comment_rows);
        assert_eq!(v0["id"], "file:1:0:1");
        assert_eq!(v0["truncated"]["children"], 1);
        assert!(v0.get("children").is_none());

        // depth 5 → full tree; the anchored comment attaches to 1:2, the
        // canvas-level comment (anchor None) attaches to no node.
        let v = render_subtree_json_with_comments(1, &root, 5, &comment_rows);
        let frame = &v["children"][0];
        assert_eq!(frame["id"], "file:1:1:2");
        assert_eq!(frame["comments"][0]["id"], "file:1:comm:1");
        assert_eq!(frame["comments"].as_array().unwrap().len(), 1);
        let leaf_out = &frame["children"][0];
        assert_eq!(leaf_out["id"], "file:1:1:3");
        assert!(leaf_out.get("comments").is_none());
        // Canvas-level thread never appears in the per-node tree.
        assert!(!serde_json::to_string(&v).unwrap().contains("comm:2"));
    }

    /// Guard for the `project_rows` helper shared by render_root/render_project:
    /// PROJECT header, FILE header, and descent into the file tree.
    #[tokio::test]
    async fn project_rows_emits_headers_and_descent() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = CacheDir::new(tmp.path());
        cache.ensure().unwrap();
        let doc = json!({
            "id": "0:0", "name": "doc", "type": "DOCUMENT",
            "children": [
                { "id": "0:1", "name": "Home", "type": "CANVAS",
                  "children": [{ "id": "1:2", "name": "Header", "type": "FRAME",
                                 "absoluteBoundingBox": { "x": 0.0, "y": 0.0, "width": 100.0, "height": 40.0 } }] }
            ],
        });
        let file_ref = FileRef {
            file_key: "abc".into(),
            name: "Demo".into(),
            last_modified: "2026-01-01".into(),
            project_id: "p1".into(),
            project_name: "Proj".into(),
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
        let synth = resolver.synth();
        let psynth = *synth.projects.get("p1").unwrap();
        let fsynth = synth.file_synth("abc").unwrap();
        let meta = resolver.cache().read_meta("abc").unwrap().unwrap();

        let mut hidden = BTreeSet::new();
        let mut matches = 0usize;
        // depth 3: projects(0) -> files(1) -> canvases(2) -> frames(3).
        let rows = project_rows(
            &resolver,
            psynth,
            "Proj",
            &[(fsynth, meta)],
            3,
            false,
            &mut hidden,
            None,
            false,
            None,
            &mut matches,
        );
        let out = format_rows(&rows);
        assert!(out.contains(&format!("proj:{psynth}")), "{out}");
        assert!(out.contains("PROJECT  \"Proj\""), "{out}");
        assert!(out.contains(&format!("file:{fsynth}")), "{out}");
        assert!(out.contains("FILE  \"Demo\""), "{out}");
        assert!(out.contains("FRAME  \"Header\""), "{out}");
    }

    /// Counts mode (default): a node with anchored threads gets a
    /// `[N comments]` suffix, no COMMENT rows, no canvas-level rows.
    #[tokio::test]
    async fn project_rows_counts_mode_sets_suffix_no_comment_rows() {
        use crate::comment_assoc::{
            Anchor, AnchorKind, AssociatedComment, AssociationMethod, NodeRef,
        };

        let tmp = tempfile::tempdir().unwrap();
        let cache = CacheDir::new(tmp.path());
        cache.ensure().unwrap();
        let doc = json!({
            "id": "0:0", "name": "doc", "type": "DOCUMENT",
            "children": [
                { "id": "0:1", "name": "Home", "type": "CANVAS",
                  "children": [{ "id": "1:2", "name": "Header", "type": "FRAME",
                                 "absoluteBoundingBox": { "x": 0.0, "y": 0.0, "width": 100.0, "height": 40.0 } }] }
            ],
        });
        let file_ref = FileRef {
            file_key: "abc".into(),
            name: "Demo".into(),
            last_modified: "2026-01-01".into(),
            project_id: "p1".into(),
            project_name: "Proj".into(),
        };
        let payload = build_cached_file(&file_ref, &doc, 0);
        cache.write_file("abc", &payload).unwrap();
        cache
            .write_meta(&FileMeta::from_success(&file_ref, &payload, 0, 0))
            .unwrap();
        // One thread head anchored to the Header frame (1:2).
        let comment = AssociatedComment {
            comment_id: "c1".into(),
            message: "looks good".into(),
            author: "alice".into(),
            created_at: "2026-01-02".into(),
            resolved_at: None,
            parent_id: None,
            order_id: None,
            reactions: 0,
            anchor: Anchor {
                kind: AnchorKind::FrameOffset,
                explicit_node_id: Some("1:2".into()),
                canvas_point: None,
                canvas_rect: None,
            },
            node: Some(NodeRef {
                node_id: "1:2".into(),
                type_: "FRAME".into(),
                name: "Header".into(),
                path: vec![],
            }),
            method: AssociationMethod::Explicit,
            stale_node_id: None,
        };
        cache.write_comments("abc", &[comment]).unwrap();
        crate::synth::with_lock(&cache, |s| {
            s.intern_project("p1");
            s.intern_file("abc");
        })
        .unwrap();

        let resolver = Resolver::from_cache(CacheDir::new(tmp.path()), true).unwrap();
        let synth = resolver.synth();
        let psynth = *synth.projects.get("p1").unwrap();
        let fsynth = synth.file_synth("abc").unwrap();
        let meta = resolver.cache().read_meta("abc").unwrap().unwrap();

        let mut hidden = BTreeSet::new();
        let mut matches = 0usize;
        let rows = project_rows(
            &resolver,
            psynth,
            "Proj",
            &[(fsynth, meta)],
            3,
            false,
            &mut hidden,
            None,
            false, // counts mode
            None,
            &mut matches,
        );
        let out = format_rows(&rows);
        assert!(
            out.contains("FRAME  \"Header\"  [1 comment]"),
            "expected suffix on the anchored node: {out}"
        );
        assert!(
            !out.contains("COMMENT"),
            "counts mode must not emit COMMENT rows: {out}"
        );
    }
}

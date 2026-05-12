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

use crate::cache::{CacheNode, EntryStatus, FileMeta};
use crate::id::Id;
use crate::resolver::{parse_id, render_resolve_error, ResolvedTarget, Resolver};
use crate::tree::{collect_visible, render_flat, truncate_display, NAME_DISPLAY_MAX};
use crate::{print, Globals, Output};

/// Default descent depth. Depth counts levels below "self" (the same
/// convention `render_flat` uses). At the root this means projects (depth 0),
/// files (depth 1), canvases (depth 2), frames (depth 3). At `file:N` it
/// means file (depth 0), canvases (depth 1), and so on.
const DEFAULT_DEPTH: usize = 3;

/// List a node and its descendants. Honors `--depth` (default 3) at every
/// level — root, project, file, and node alike.
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Tagged or native ID, or a Figma URL. Omit to list cached projects.
    pub id: Option<String>,

    /// Override the default descent depth.
    #[arg(long)]
    pub depth: Option<usize>,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        let resolver = Resolver::new(globals.cache_only)?;
        let format = globals.output;
        let depth = self.depth.unwrap_or(DEFAULT_DEPTH);

        match self.id.as_deref() {
            None => render_root(&resolver, depth, format),
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
                    ResolvedTarget::Root => render_root(&resolver, depth, format),
                    ResolvedTarget::Project { synth, project_id } => {
                        render_project(&resolver, synth, &project_id, depth, format)
                    }
                    ResolvedTarget::File { synth, meta, document } => {
                        render_file(synth, &meta, &document.document, depth, format)
                    }
                    ResolvedTarget::Node { file_synth, meta, node } => {
                        render_node_subtree(file_synth, &meta, &node, depth, format)
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
fn render_root(resolver: &Resolver, depth: usize, format: Output) -> Result<()> {
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
                            append_descent_rows(resolver, fsynth, fm, depth, 1, &mut rows);
                        }
                    }
                }
            }
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
                                build_file_json(resolver, fs, fm, depth)
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
            print(&json!({ "projects": projects }), format)
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
    let max_bounds = rows.iter().map(|r| r.bounds.len()).max().unwrap_or(1).max(1);
    let mut out = String::new();
    for r in rows {
        let indent = "  ".repeat(r.depth);
        out.push_str(&format!(
            "{id:<id_w$}  {b:<b_w$}  | {indent}{kind}  \"{name}\"",
            id = r.id,
            b = r.bounds,
            kind = r.kind,
            name = r.name,
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
fn append_descent_rows(
    resolver: &Resolver,
    file_synth: u32,
    fm: &FileMeta,
    depth: usize,
    file_depth: usize,
    rows: &mut Vec<Row>,
) {
    let cached = match resolver.cache().read_file(&fm.file_key) {
        Ok(Some(c)) => c,
        _ => return,
    };
    let synthetic = synthesize_file_root(fm, &cached.document);
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
        rows.push(Row {
            id: format!("file:{}:{}", file_synth, node.id),
            bounds: node.bounds.map(|b| b.compact()).unwrap_or_else(|| "-".to_owned()),
            depth: file_depth + sub_depth,
            kind,
            name: truncate_display(&node.name, NAME_DISPLAY_MAX).into_owned(),
            truncated,
        });
    }
}

/// Build the JSON object for one file row, attaching a recursive `children`
/// array (or `truncated` marker) when `depth >= 2`. Mirrors the YAML descent
/// in `append_descent_rows`.
fn build_file_json(resolver: &Resolver, file_synth: u32, fm: &FileMeta, depth: usize) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(format!("file:{file_synth}")));
    obj.insert("file_key".into(), json!(fm.file_key));
    obj.insert("name".into(), json!(fm.name));
    obj.insert("last_modified".into(), json!(fm.last_modified));
    if depth >= 2 {
        if let Ok(Some(cached)) = resolver.cache().read_file(&fm.file_key) {
            let synthetic = synthesize_file_root(fm, &cached.document);
            let rendered = render_subtree_json(file_synth, &synthetic, depth - 1);
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
                        append_descent_rows(resolver, *fs, fm, depth, 1, &mut rows);
                    }
                }
            }
            print!("{}", format_rows(&rows));
            Ok(())
        }
        Output::Json => {
            let file_jsons: Vec<Value> = if depth >= 1 {
                files
                    .iter()
                    .map(|(fs, fm)| build_file_json(resolver, *fs, fm, depth))
                    .collect()
            } else {
                Vec::new()
            };
            print(
                &json!({
                    "id": format!("proj:{project_synth}"),
                    "project_id": project_id,
                    "name": project_name,
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
    file_synth: u32,
    meta: &FileMeta,
    document: &CacheNode,
    depth: usize,
    format: Output,
) -> Result<()> {
    let synthetic_root = synthesize_file_root(meta, document);
    match format {
        Output::Yaml => {
            let lines = render_flat(&synthetic_root, file_synth, depth);
            print!("{}\n", lines.join("\n"));
            Ok(())
        }
        Output::Json => print(
            &json!({
                "id": format!("file:{file_synth}"),
                "file_key": meta.file_key,
                "name": meta.name,
                "items": render_subtree_json(file_synth, &synthetic_root, depth),
            }),
            format,
        ),
    }
}

/// Node-subtree listing — straightforward delegation to `render_flat`.
fn render_node_subtree(
    file_synth: u32,
    meta: &FileMeta,
    node: &CacheNode,
    depth: usize,
    format: Output,
) -> Result<()> {
    match format {
        Output::Yaml => {
            let lines = render_flat(node, file_synth, depth);
            print!("{}\n", lines.join("\n"));
            Ok(())
        }
        Output::Json => print(
            &json!({
                "id": format!("file:{file_synth}:{}", node.id),
                "file_key": meta.file_key,
                "items": render_subtree_json(file_synth, node, depth),
            }),
            format,
        ),
    }
}

pub fn synthesize_file_root(meta: &FileMeta, document: &CacheNode) -> CacheNode {
    CacheNode {
        // Empty id makes `tree::format_cache_line` emit the bare `file:N` form
        // (no trailing `:0:0`), so the DOCUMENT node never surfaces as a row.
        id: String::new(),
        type_: "FILE".to_owned(),
        name: meta.name.clone(),
        visible: true,
        bounds: None,
        // Skip the DOCUMENT node entirely — its visible children (canvases)
        // become the top-level items under the synthesized FILE header.
        children: document.children.iter().filter(|c| c.visible).cloned().collect(),
    }
}

fn render_subtree_json(file_synth: u32, node: &CacheNode, max_depth: usize) -> Value {
    fn build(node: &CacheNode, file_synth: u32, depth: usize, max_depth: usize) -> Value {
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
        let kids: Vec<&CacheNode> = node.children.iter().filter(|c| c.visible).collect();
        if !kids.is_empty() {
            if depth >= max_depth {
                obj.insert("truncated".into(), json!({ "children": kids.len() }));
            } else {
                let rendered: Vec<Value> = kids
                    .iter()
                    .map(|c| build(c, file_synth, depth + 1, max_depth))
                    .collect();
                obj.insert("children".into(), Value::Array(rendered));
            }
        }
        Value::Object(obj)
    }
    build(node, file_synth, 0, max_depth)
}

/// If `--in <ID>` named a file scope and the user passed a bare native id,
/// rewrite the bare id as a qualified `file:N:x:y`. All other id shapes pass
/// through unchanged (an explicit qualifier wins over `--in`).
fn apply_scope(id: Id, scope: Option<&str>) -> Result<Id> {
    let Some(scope) = scope else { return Ok(id) };
    let Id::BareNode(node) = &id else { return Ok(id) };
    let scope_id = parse_id(scope).map_err(|e| anyhow::anyhow!("--in: {e}"))?;
    let file_synth = match scope_id {
        Id::File(n) => n,
        Id::Node { file, .. } => file,
        _ => anyhow::bail!("--in must name a file or node scope (e.g. file:2); got {scope}"),
    };
    Ok(Id::Node { file: file_synth, node: node.clone() })
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

        let doc = json!({
            "id": "0:0", "name": "doc", "type": "DOCUMENT",
            "children": [
                { "id": "0:1", "name": "Cover", "type": "CANVAS",
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
            ResolvedTarget::File { synth, meta, document } => (synth, meta, document),
            other => panic!("expected File target, got {other:?}"),
        };
        let synthetic = synthesize_file_root(&meta, &document.document);
        let lines = render_flat(&synthetic, synth, 1);

        // First line must be the synthesized FILE row with bare `file:1` (no
        // trailing `:0:0`), so the DOCUMENT node id is hidden.
        let first = &lines[0];
        assert!(
            first.contains("file:1") && !first.contains("file:1:0:0"),
            "expected synthesized file:1 header, got: {first}"
        );
        assert!(first.contains("FILE"), "expected FILE type in header: {first}");
        assert!(first.contains("\"Demo File\""), "expected file name: {first}");

        // CANVAS children should appear with their qualified IDs.
        let joined = lines.join("\n");
        assert!(joined.contains("file:1:0:1"), "Cover canvas missing: {joined}");
        assert!(joined.contains("file:1:0:2"), "Employees canvas missing: {joined}");
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
            Id::Node { file: 7, node: "1094:66591".into() }
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
}


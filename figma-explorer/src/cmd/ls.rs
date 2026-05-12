//! `ls` — list anything at any level. Replaces `files`/`pages`/`frames`/`tree`.
//!
//! Behavior depends on what the ID resolves to:
//! - No ID → list every cached project, with its files grouped underneath.
//! - `proj:N` → header + files in that project.
//! - `file:N` → synthesized `file:N FILE "name"` header + pages (default
//!   depth 1; the DOCUMENT node at `0:0` is hidden to keep ambiguity at bay).
//! - `file:N:x:y` or `URL` or bare `x:y` → that node + descendants at the
//!   requested depth.
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
use crate::tree::render_flat;
use crate::{print, Globals, Output};

/// Default depth when descending into a node (`file:N:x:y`, bare, URL).
const DEFAULT_NODE_DEPTH: usize = 3;
/// Default depth when listing the top of a file (`file:N`). Capped to 1 so a
/// bare `figma-explorer ls file:N` doesn't dump an 80k-node tree by accident.
const DEFAULT_FILE_DEPTH: usize = 1;

/// List a node and its descendants. Default depth varies by level (see
/// constants above). At the root (no ID) or `proj:N`, lists cached entities
/// rather than rendering a node tree.
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Tagged or native ID, or a Figma URL. Omit to list cached projects.
    pub id: Option<String>,

    /// Override the default descent depth. Ignored at the root listing.
    #[arg(long)]
    pub depth: Option<usize>,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        let resolver = Resolver::new(globals.cache_only)?;
        let format = globals.output;

        match self.id.as_deref() {
            None => render_root(&resolver, format),
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
                    ResolvedTarget::Root => render_root(&resolver, format),
                    ResolvedTarget::Project { synth, project_id } => {
                        render_project(&resolver, synth, &project_id, format)
                    }
                    ResolvedTarget::File { synth, meta, document } => {
                        let depth = self.depth.unwrap_or(DEFAULT_FILE_DEPTH);
                        render_file(synth, &meta, &document.document, depth, format)
                    }
                    ResolvedTarget::Node { file_synth, meta, node } => {
                        let depth = self.depth.unwrap_or(DEFAULT_NODE_DEPTH);
                        render_node_subtree(file_synth, &meta, &node, depth, format)
                    }
                }
            }
        }
    }
}

/// Root listing — projects + their files, read directly from the cache state.
fn render_root(resolver: &Resolver, format: Output) -> Result<()> {
    let synth = resolver.synth();
    let metas = resolver.cache().list_metas()?;

    // Group OK files by project synth.
    let mut groups: Vec<(u32, String, Vec<FileMeta>)> = Vec::new();
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
        groups.push((project_synth, project_id.clone(), files));
    }
    groups.sort_by_key(|(s, _, _)| *s);

    match format {
        Output::Yaml => {
            // Two-pass column-width measurement across every line (project +
            // every file) so the pipe rail stays at a fixed column.
            let mut rows: Vec<RootRow> = Vec::new();
            for (psynth, pid, files) in &groups {
                rows.push(RootRow {
                    id: format!("proj:{psynth}"),
                    kind: "PROJECT".to_owned(),
                    name: pid.clone(),
                    bounds: "-".to_owned(),
                    depth: 0,
                });
                for fm in files {
                    let fsynth = synth.file_synth(&fm.file_key).expect("filtered above");
                    rows.push(RootRow {
                        id: format!("file:{fsynth}"),
                        kind: "FILE".to_owned(),
                        name: fm.name.clone(),
                        bounds: "-".to_owned(),
                        depth: 1,
                    });
                }
            }
            let max_id = rows.iter().map(|r| r.id.len()).max().unwrap_or(0);
            let max_bounds = rows.iter().map(|r| r.bounds.len()).max().unwrap_or(1);
            let mut out = String::new();
            for r in &rows {
                let indent = "  ".repeat(r.depth);
                out.push_str(&format!(
                    "{id:<id_w$}  {b:<b_w$}  | {indent}{kind}  \"{name}\"\n",
                    id = r.id,
                    b = r.bounds,
                    kind = r.kind,
                    name = r.name,
                    id_w = max_id,
                    b_w = max_bounds,
                    indent = indent,
                ));
            }
            print!("{out}");
            Ok(())
        }
        Output::Json => {
            let projects: Vec<Value> = groups
                .iter()
                .map(|(ps, pid, files)| {
                    let file_jsons: Vec<Value> = files
                        .iter()
                        .map(|fm| {
                            let fs = synth.file_synth(&fm.file_key).expect("filtered above");
                            json!({
                                "id": format!("file:{fs}"),
                                "file_key": fm.file_key,
                                "name": fm.name,
                                "last_modified": fm.last_modified,
                            })
                        })
                        .collect();
                    json!({
                        "id": format!("proj:{ps}"),
                        "project_id": pid,
                        "files": file_jsons,
                    })
                })
                .collect();
            print(&json!({ "projects": projects }), format)
        }
    }
}

struct RootRow {
    id: String,
    kind: String,
    name: String,
    bounds: String,
    depth: usize,
}

/// Project listing — header + files in that project. Read from cache state.
fn render_project(
    resolver: &Resolver,
    project_synth: u32,
    project_id: &str,
    format: Output,
) -> Result<()> {
    let synth = resolver.synth();
    let metas = resolver.cache().list_metas()?;
    let mut files: Vec<(u32, FileMeta)> = metas
        .into_iter()
        .filter(|m| m.status == EntryStatus::Ok && m.project_id == project_id)
        .filter_map(|m| synth.file_synth(&m.file_key).map(|s| (s, m)))
        .collect();
    files.sort_by(|(_, a), (_, b)| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    match format {
        Output::Yaml => {
            // Pre-compute widths.
            let header = (format!("proj:{project_synth}"), "-".to_owned());
            let mut max_id = header.0.len();
            let mut max_b = header.1.len();
            for (fs, _) in &files {
                let id = format!("file:{fs}");
                if id.len() > max_id {
                    max_id = id.len();
                }
            }
            // bounds column is always "-" here.
            if max_b < 1 {
                max_b = 1;
            }
            let mut out = String::new();
            out.push_str(&format!(
                "{id:<id_w$}  {b:<b_w$}  | PROJECT  \"{name}\"\n",
                id = header.0,
                b = header.1,
                name = project_id,
                id_w = max_id,
                b_w = max_b,
            ));
            for (fs, fm) in &files {
                let id = format!("file:{fs}");
                out.push_str(&format!(
                    "{id:<id_w$}  {b:<b_w$}  |   FILE  \"{name}\"\n",
                    id = id,
                    b = "-",
                    name = fm.name,
                    id_w = max_id,
                    b_w = max_b,
                ));
            }
            print!("{out}");
            Ok(())
        }
        Output::Json => {
            let file_jsons: Vec<Value> = files
                .iter()
                .map(|(fs, fm)| {
                    json!({
                        "id": format!("file:{fs}"),
                        "file_key": fm.file_key,
                        "name": fm.name,
                        "last_modified": fm.last_modified,
                    })
                })
                .collect();
            print(
                &json!({
                    "id": format!("proj:{project_synth}"),
                    "project_id": project_id,
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


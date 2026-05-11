//! Local file cache.
//!
//! Stores a slimmed-down projection of each Figma file under `cache/files/`,
//! plus a `manifest.json` keyed by file_key. The projection keeps only the
//! fields the structural commands (`find`, `pages`, `frames`, `tree`) consume:
//! `id`, `name`, `type`, `visible: false` when hidden, `absoluteBoundingBox`,
//! and `children` recursively. Everything else — fills, strokes, effects,
//! type styles, characters, layout grids, vector geometry — is dropped.
//!
//! Measured reductions on this team's files: ~92% smaller than the raw API
//! JSON, with 51 files projected to ~30 MB on disk vs ~2.8 GB raw.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use figma_api::apis::configuration::Configuration;
use figma_api::apis::projects_api as api;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::into_anyhow;

pub const DEFAULT_DIR: &str = "cache";
pub const DEFAULT_TTL_SECS: u64 = 3600;

/// Trim a Figma node tree to just the structural fields the search/tree
/// commands need. Field names match the Figma REST API verbatim so the
/// existing `node::*` accessors work on cached data without changes.
pub fn strip_node(node: &Value) -> Value {
    let mut out = Map::new();
    if let Some(v) = node.get("id") {
        out.insert("id".into(), v.clone());
    }
    if let Some(v) = node.get("name") {
        out.insert("name".into(), v.clone());
    }
    if let Some(v) = node.get("type") {
        out.insert("type".into(), v.clone());
    }
    // Figma omits `visible` when the node is visible; preserve that convention.
    if matches!(node.get("visible"), Some(Value::Bool(false))) {
        out.insert("visible".into(), Value::Bool(false));
    }
    if let Some(v) = node.get("absoluteBoundingBox") {
        out.insert("absoluteBoundingBox".into(), v.clone());
    }
    if let Some(arr) = node.get("children").and_then(|c| c.as_array()) {
        out.insert(
            "children".into(),
            Value::Array(arr.iter().map(strip_node).collect()),
        );
    }
    Value::Object(out)
}

pub fn count_nodes(node: &Value) -> usize {
    let mut n = 1;
    if let Some(arr) = node.get("children").and_then(|c| c.as_array()) {
        for c in arr {
            n += count_nodes(c);
        }
    }
    n
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    /// File was fetched and projected successfully.
    Ok,
    /// Figma returned 403 "File not exportable" — typically community files.
    /// Skip on subsequent runs unless `last_modified` changes.
    NotExportable,
    /// Transient failure. Retried on next run regardless of `last_modified`.
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub file_key: String,
    pub name: String,
    pub project_id: String,
    pub project_name: String,
    /// `lastModified` from the project listing — drives invalidation.
    pub last_modified: String,
    pub cached_at_epoch: u64,
    pub status: EntryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub updated_at_epoch: u64,
    pub files: Vec<ManifestEntry>,
}

pub struct CacheDir {
    pub root: PathBuf,
}

impl CacheDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;
        fs::create_dir_all(self.root.join("files"))?;
        Ok(())
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join("manifest.json")
    }

    pub fn file_path(&self, file_key: &str) -> PathBuf {
        self.root.join("files").join(format!("{file_key}.json"))
    }

    pub fn read_manifest(&self) -> Result<Manifest> {
        let p = self.manifest_path();
        if !p.exists() {
            return Ok(Manifest::default());
        }
        let s = fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        serde_json::from_str(&s).with_context(|| format!("parsing {}", p.display()))
    }

    pub fn write_manifest(&self, m: &Manifest) -> Result<()> {
        let s = serde_json::to_string_pretty(m)?;
        let path = self.manifest_path();
        // Same tempfile+rename trick as write_file — keeps a concurrent
        // tree/find that's also rewriting the manifest from leaving a
        // half-written file. Last-writer-wins is fine.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, &s).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("renaming into {}", path.display()))?;
        Ok(())
    }

    /// Read a cached payload by file_key. Returns `Ok(None)` if no file exists.
    pub fn read_file(&self, file_key: &str) -> Result<Option<Value>> {
        let path = self.file_path(file_key);
        if !path.exists() {
            return Ok(None);
        }
        let s = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let v: Value = serde_json::from_str(&s)
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(Some(v))
    }

    /// Write a file's projected payload to disk. Returns the number of bytes
    /// written.
    pub fn write_file(&self, file_key: &str, payload: &Value) -> Result<u64> {
        let path = self.file_path(file_key);
        let s = serde_json::to_string(payload)?;
        let bytes = s.len() as u64;
        // Write via a sibling tempfile + rename so a crash mid-write can't
        // leave a half-written cache entry shadowing a previous good one.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, &s).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("renaming into {}", path.display()))?;
        Ok(bytes)
    }
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Classify an API error string. Used to mark community / non-exportable
/// files differently from genuine failures so we don't retry them on every run.
pub fn is_not_exportable_error(err: &str) -> bool {
    err.contains("403") && err.to_lowercase().contains("not exportable")
}

/// Resolve the default cache dir, anchored at the working directory.
pub fn default_dir() -> PathBuf {
    Path::new(DEFAULT_DIR).to_path_buf()
}

/// One row from the Figma project listing — what `get_project_files` returns,
/// in the flat shape our cache uses.
#[derive(Debug, Clone)]
pub struct FileRef {
    pub file_key: String,
    pub name: String,
    pub last_modified: String,
    pub project_id: String,
    pub project_name: String,
}

/// Fetch every file across the given projects. One API call per project; all
/// projects are queried sequentially (small response, cheap relative to file
/// fetches).
pub async fn list_project_files(
    cfg: &Configuration,
    project_ids: &[String],
) -> Result<Vec<FileRef>> {
    let mut out = Vec::new();
    for pid in project_ids {
        let resp = api::get_project_files(
            cfg,
            api::GetProjectFilesParams {
                project_id: pid.clone(),
                branch_data: None,
            },
        )
        .await
        .map_err(into_anyhow)
        .with_context(|| format!("listing files for project {pid}"))?;
        let project_name = resp.name.clone();
        for f in resp.files {
            out.push(FileRef {
                file_key: f.key,
                name: f.name,
                last_modified: f.last_modified,
                project_id: pid.clone(),
                project_name: project_name.clone(),
            });
        }
    }
    Ok(out)
}

/// What `load_file_doc` should do given the current manifest state. Factored
/// out so the pure decision logic can be unit-tested without any I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadAction {
    /// No manifest entry exists — bypass cache, fetch live, don't write.
    Bypass,
    /// Manifest entry exists and is fresh; just read from disk.
    UseCache,
    /// Manifest entry exists but TTL expired or payload missing — re-list
    /// projects, compare `last_modified`, refetch this file if changed.
    CheckFreshness,
}

pub fn decide_action(
    has_entry: bool,
    manifest_updated_at: u64,
    now: u64,
    ttl_secs: u64,
    payload_exists: bool,
) -> LoadAction {
    if !has_entry {
        return LoadAction::Bypass;
    }
    if !payload_exists {
        return LoadAction::CheckFreshness;
    }
    let elapsed = now.saturating_sub(manifest_updated_at);
    if elapsed >= ttl_secs {
        LoadAction::CheckFreshness
    } else {
        LoadAction::UseCache
    }
}

fn parse_project_ids_env() -> Vec<String> {
    std::env::var("FIGMA_PROJECTS_IDS")
        .ok()
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Build the cached-payload wrapper from a freshly fetched live API response.
fn build_payload(file_ref: &FileRef, stripped_doc: Value, node_count: usize, now: u64) -> Value {
    json!({
        "file_key": file_ref.file_key,
        "name": file_ref.name,
        "project_id": file_ref.project_id,
        "project_name": file_ref.project_name,
        "last_modified": file_ref.last_modified,
        "cached_at_epoch": now,
        "node_count": node_count,
        "document": stripped_doc,
    })
}

/// Refetch a file, restrip, and update both the on-disk payload and the
/// manifest entry. Returns the new payload on success.
async fn refetch_and_store(
    cfg: &Configuration,
    cache: &CacheDir,
    manifest: &mut Manifest,
    idx: usize,
    file_ref: &FileRef,
    now: u64,
) -> Result<Value> {
    let file = crate::cmd::fetch_file_json(cfg, &file_ref.file_key, None).await?;
    let stripped = strip_node(&file["document"]);
    let node_count = count_nodes(&stripped);
    let payload = build_payload(file_ref, stripped, node_count, now);
    let bytes = cache.write_file(&file_ref.file_key, &payload)?;
    let entry = &mut manifest.files[idx];
    entry.name = file_ref.name.clone();
    entry.project_id = file_ref.project_id.clone();
    entry.project_name = file_ref.project_name.clone();
    entry.last_modified = file_ref.last_modified.clone();
    entry.cached_at_epoch = now;
    entry.status = EntryStatus::Ok;
    entry.error = None;
    entry.node_count = Some(node_count);
    entry.bytes = Some(bytes);
    Ok(payload)
}

/// Best-effort freshness check + lazy refetch for `file_key`. Mutates the
/// manifest in place (and rewrites it to disk) but never returns the
/// refreshed payload — the outer `load_file_doc` re-reads from disk so the
/// success and stale-fallback paths share the same return code.
///
/// Failures log to stderr and leave the cache untouched. Listing failures
/// don't bump `updated_at_epoch`, so the next call will retry.
async fn try_refresh(
    cfg: &Configuration,
    cache: &CacheDir,
    manifest: &mut Manifest,
    file_key: &str,
    idx: usize,
    now: u64,
) {
    let project_ids = parse_project_ids_env();
    if project_ids.is_empty() {
        eprintln!(
            "cache: TTL expired but FIGMA_PROJECTS_IDS unset — serving cached entry for {file_key}"
        );
        return;
    }

    let listings = match list_project_files(cfg, &project_ids).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cache: freshness check failed: {e:#} — serving cached entry for {file_key}");
            return;
        }
    };

    let current = match listings.iter().find(|f| f.file_key == file_key) {
        Some(c) => c.clone(),
        None => {
            eprintln!(
                "cache: file {file_key} no longer present in any configured project — serving cached entry"
            );
            // Listing itself succeeded, so the global timestamp is current.
            manifest.updated_at_epoch = now;
            let _ = cache.write_manifest(manifest);
            return;
        }
    };

    let cached_unchanged = manifest.files[idx].last_modified == current.last_modified;
    let cached_status = manifest.files[idx].status;

    // Listing fetched OK — even if nothing changed, our knowledge of "what's
    // current" is now fresh; bump the global timestamp.
    manifest.updated_at_epoch = now;

    if cached_unchanged && cached_status == EntryStatus::Ok {
        let _ = cache.write_manifest(manifest);
        return;
    }
    if cached_unchanged && cached_status == EntryStatus::NotExportable {
        // Don't burn an API call refetching a known not-exportable file
        // whose timestamp hasn't moved.
        let _ = cache.write_manifest(manifest);
        return;
    }

    // Either the file changed or we previously failed to fetch it. Try again.
    match refetch_and_store(cfg, cache, manifest, idx, &current, now).await {
        Ok(_) => {
            let entry = &manifest.files[idx];
            eprintln!(
                "cache: refreshed {} ({}, {} nodes, {} KB)",
                entry.file_key,
                entry.name,
                entry.node_count.unwrap_or(0),
                entry.bytes.unwrap_or(0) / 1024
            );
            let _ = cache.write_manifest(manifest);
        }
        Err(e) => {
            let msg = format!("{e:#}");
            eprintln!("cache: refetch of {file_key} failed: {msg} — serving stale cached payload");
            let entry = &mut manifest.files[idx];
            entry.status = if is_not_exportable_error(&msg) {
                EntryStatus::NotExportable
            } else {
                EntryStatus::Failed
            };
            entry.error = Some(msg);
            let _ = cache.write_manifest(manifest);
        }
    }
}

/// Cache-first loader for the four structural commands.
///
/// - If the file is in the manifest and within TTL: returns the on-disk payload.
/// - If TTL expired (or the payload file is missing): re-queries project
///   listings; refetches the file when its `last_modified` advanced.
/// - If the file isn't in the manifest at all: falls through to a live fetch
///   (the cache is scoped to FIGMA_PROJECTS_IDS — random external files don't
///   belong in it).
///
/// Returns the full cached wrapper (`{file_key, name, last_modified, document, …}`)
/// for on-manifest reads, or the raw Figma API response for off-manifest
/// fallbacks. Both shapes carry `["document"]` and `["name"]` at the same
/// paths, so callers can ignore the distinction.
pub async fn load_file_doc(cfg: &Configuration, file_key: &str) -> Result<Value> {
    let cache = CacheDir::new(default_dir());
    cache.ensure()?;
    let mut manifest = cache.read_manifest()?;
    let now = now_epoch();

    let idx = manifest.files.iter().position(|e| e.file_key == file_key);
    let payload_exists = cache.file_path(file_key).exists();
    let action = decide_action(
        idx.is_some(),
        manifest.updated_at_epoch,
        now,
        DEFAULT_TTL_SECS,
        payload_exists,
    );

    match action {
        LoadAction::Bypass => crate::cmd::fetch_file_json(cfg, file_key, None).await,
        LoadAction::UseCache => match cache.read_file(file_key)? {
            Some(v) => Ok(v),
            // Shouldn't happen — decide_action checked existence — but be defensive.
            None => crate::cmd::fetch_file_json(cfg, file_key, None).await,
        },
        LoadAction::CheckFreshness => {
            // idx is Some by construction of LoadAction
            let i = idx.expect("CheckFreshness implies manifest entry");
            try_refresh(cfg, &cache, &mut manifest, file_key, i, now).await;
            match cache.read_file(file_key)? {
                Some(v) => Ok(v),
                None => crate::cmd::fetch_file_json(cfg, file_key, None).await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strip_keeps_structural_fields_drops_paint() {
        let raw = json!({
            "id": "1:2",
            "name": "Hero",
            "type": "FRAME",
            "absoluteBoundingBox": { "x": 0, "y": 0, "width": 1440, "height": 800 },
            "fills": [{"type": "SOLID", "color": {"r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0}}],
            "strokes": [],
            "effects": [],
            "characters": "ignored",
            "children": [
                {"id": "1:3", "name": "Title", "type": "TEXT", "characters": "Hi", "style": {"fontSize": 32}},
                {"id": "1:4", "name": "ignored", "type": "TEXT", "visible": false}
            ]
        });
        let s = strip_node(&raw);
        assert_eq!(s["id"], "1:2");
        assert_eq!(s["type"], "FRAME");
        assert!(s.get("fills").is_none());
        assert!(s.get("characters").is_none());
        assert!(s.get("style").is_none());
        let kids = s["children"].as_array().unwrap();
        assert_eq!(kids.len(), 2);
        assert!(kids[0].get("characters").is_none());
        assert_eq!(kids[1]["visible"], false);
        // absoluteBoundingBox preserved verbatim — node::bounds() expects it.
        assert_eq!(s["absoluteBoundingBox"]["width"], 1440);
    }

    #[test]
    fn count_nodes_includes_self_plus_descendants() {
        let n = json!({
            "id": "a",
            "children": [
                {"id": "b"},
                {"id": "c", "children": [{"id": "d"}]}
            ]
        });
        assert_eq!(count_nodes(&n), 4);
    }

    #[test]
    fn not_exportable_classifier_matches_real_response() {
        assert!(is_not_exportable_error(
            "figma API error (403 Forbidden): {\"status\":403,\"err\":\"File not exportable\"}"
        ));
        assert!(!is_not_exportable_error("HTTP request failed: timeout"));
    }

    #[test]
    fn decide_action_no_entry_bypasses() {
        assert_eq!(
            decide_action(false, 0, 0, 3600, false),
            LoadAction::Bypass
        );
    }

    #[test]
    fn decide_action_within_ttl_uses_cache() {
        // now=1000, manifest_updated=500, ttl=3600 → 500s elapsed, fresh.
        assert_eq!(
            decide_action(true, 500, 1000, 3600, true),
            LoadAction::UseCache
        );
    }

    #[test]
    fn decide_action_ttl_expired_checks_freshness() {
        // 5000s elapsed against a 3600s TTL.
        assert_eq!(
            decide_action(true, 1000, 6000, 3600, true),
            LoadAction::CheckFreshness
        );
    }

    #[test]
    fn decide_action_missing_payload_forces_check_even_if_fresh() {
        // Fresh manifest but payload deleted from disk — must refresh
        // rather than serve a hole.
        assert_eq!(
            decide_action(true, 500, 1000, 3600, false),
            LoadAction::CheckFreshness
        );
    }

    #[test]
    fn decide_action_boundary_ttl_treated_as_expired() {
        // Exactly at TTL should refresh (>= comparison).
        assert_eq!(
            decide_action(true, 0, 3600, 3600, true),
            LoadAction::CheckFreshness
        );
    }
}

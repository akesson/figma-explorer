//! Local file cache.
//!
//! Stores a slimmed-down projection of each Figma file under `cache/files/`,
//! plus a `manifest.json` keyed by file_key. The projection keeps only the
//! fields the structural commands (`find`, `pages`, `frames`, `tree`,
//! `search`) consume: `id`, `name`, `type`, `visible`, `absoluteBoundingBox`,
//! and `children` recursively. Everything else — fills, strokes, effects,
//! type styles, characters, layout grids, vector geometry — is dropped.
//!
//! On-disk format per file: `[4-byte magic "FXC\0"][4-byte u32 LE version][rkyv body]`.
//! Magic catches "wrong kind of file" cases; version catches schema drift
//! (silent refetch on mismatch). The body is an rkyv archive of `CachedFile`.
//!
//! The manifest stays JSON — small, infrequent writes, human-readable matters
//! for debugging.
//!
//! Endianness: rkyv archives are not portable across endianness. This cache
//! is single-user and local; we don't try to support shared/transferred
//! cache directories.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use figma_api::apis::configuration::Configuration;
use figma_api::apis::projects_api as api;
use memmap2::Mmap;
use rkyv::rancor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::into_anyhow;
use crate::node::Bounds;

pub const DEFAULT_DIR: &str = "cache";
pub const DEFAULT_TTL_SECS: u64 = 3600;

/// 4-byte magic prefix on every `.rkyv` cache file. Distinguishes a cache
/// file from arbitrary bytes (truncated downloads, accidental replacement).
pub const CACHE_MAGIC: [u8; 4] = *b"FXC\0";

/// Bump when `CachedFile` / `CacheNode` schema changes. A file with a
/// different version is treated as a cache miss and silently refetched.
pub const CACHE_SCHEMA_VERSION: u32 = 1;

/// Combined header length: magic (4) + version (4).
const CACHE_HEADER_LEN: usize = 8;

/// Errors raised by cache I/O. Surfaced as typed values rather than wrapped
/// anyhow so the loader can route `VersionMismatch` to the refetch path
/// without string-matching.
#[derive(Debug)]
pub enum CacheError {
    BadMagic { found: [u8; 4] },
    VersionMismatch { found: u32, expected: u32 },
    TooShort { len: usize },
    Decode(String),
    Io(std::io::Error),
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMagic { found } => write!(
                f,
                "cache file magic mismatch: got {:02x?}, expected {:02x?}",
                found, CACHE_MAGIC
            ),
            Self::VersionMismatch { found, expected } => write!(
                f,
                "cache schema version mismatch: file is v{found}, build supports v{expected}"
            ),
            Self::TooShort { len } => {
                write!(f, "cache file too short: {len} bytes (need at least {CACHE_HEADER_LEN})")
            }
            Self::Decode(s) => write!(f, "decoding cache: {s}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for CacheError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Typed projection of a Figma node. Mirrors `strip_node`'s previous Value
/// shape: only the structural fields the navigation commands need.
#[derive(
    Clone,
    Debug,
    PartialEq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
// The recursive `Vec<CacheNode>` makes rkyv's derive try to generate an
// infinite trait-bound chain. `omit_bounds` skips the recursive bound on
// the field, and the explicit `*_bounds` directives tell the derive what
// concrete context bounds the serializer/deserializer/validator need.
#[rkyv(
    serialize_bounds(__S: rkyv::ser::Writer + rkyv::ser::Allocator, __S::Error: rkyv::rancor::Source),
    deserialize_bounds(__D::Error: rkyv::rancor::Source),
    bytecheck(bounds(__C: rkyv::validation::ArchiveContext, __C::Error: rkyv::rancor::Source))
)]
pub struct CacheNode {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub name: String,
    pub visible: bool,
    pub bounds: Option<Bounds>,
    #[rkyv(omit_bounds)]
    pub children: Vec<CacheNode>,
}

/// Cached payload wrapper: a single Figma file's projected document plus
/// listing metadata (so the cache stays self-describing).
#[derive(
    Clone,
    Debug,
    PartialEq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct CachedFile {
    pub file_key: String,
    pub name: String,
    pub project_id: String,
    pub project_name: String,
    pub last_modified: String,
    pub cached_at_epoch: u64,
    pub node_count: u64,
    pub document: CacheNode,
}

/// Project a raw Figma API node tree into the typed cache shape. Equivalent
/// to the previous `strip_node` Value → Value but materializes typed
/// `CacheNode`s directly so the result is ready for rkyv serialization.
pub fn project_to_cache(node: &Value) -> CacheNode {
    let id = node.get("id").and_then(Value::as_str).unwrap_or("").to_owned();
    let type_ = node.get("type").and_then(Value::as_str).unwrap_or("").to_owned();
    let name = node.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
    // Figma omits `visible` when the node is visible; missing → true.
    let visible = !matches!(node.get("visible"), Some(Value::Bool(false)));
    let bounds = node.get("absoluteBoundingBox").and_then(parse_bounds);
    let children = node
        .get("children")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(project_to_cache).collect())
        .unwrap_or_default();
    CacheNode { id, type_, name, visible, bounds, children }
}

fn parse_bounds(v: &Value) -> Option<Bounds> {
    let obj = v.as_object()?;
    Some(Bounds {
        x: obj.get("x")?.as_f64()?,
        y: obj.get("y")?.as_f64()?,
        width: obj.get("width")?.as_f64()?,
        height: obj.get("height")?.as_f64()?,
    })
}

/// Count `node` plus all its descendants (visible and hidden).
pub fn count_nodes(node: &CacheNode) -> usize {
    let mut n = 1usize;
    for c in &node.children {
        n += count_nodes(c);
    }
    n
}

/// Encode a `CachedFile` into the on-disk byte layout: magic + version + rkyv body.
pub fn encode_cached_file(payload: &CachedFile) -> Result<Vec<u8>, CacheError> {
    let body = rkyv::to_bytes::<rancor::Error>(payload)
        .map_err(|e| CacheError::Decode(format!("rkyv serialize: {e}")))?;
    let mut out = Vec::with_capacity(CACHE_HEADER_LEN + body.len());
    out.extend_from_slice(&CACHE_MAGIC);
    out.extend_from_slice(&CACHE_SCHEMA_VERSION.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decode the on-disk byte layout into an owned `CachedFile`. Verifies magic
/// and version before deserializing; returns typed `CacheError` so the
/// loader can route version mismatches to refetch.
pub fn decode_cached_file(bytes: &[u8]) -> Result<CachedFile, CacheError> {
    let (body, _) = split_header(bytes)?;
    rkyv::from_bytes::<CachedFile, rancor::Error>(body)
        .map_err(|e| CacheError::Decode(format!("rkyv deserialize: {e}")))
}

/// Validate the magic + version header and return the rkyv body slice.
fn split_header(bytes: &[u8]) -> Result<(&[u8], u32), CacheError> {
    if bytes.len() < CACHE_HEADER_LEN {
        return Err(CacheError::TooShort { len: bytes.len() });
    }
    let mut magic = [0u8; 4];
    magic.copy_from_slice(&bytes[..4]);
    if magic != CACHE_MAGIC {
        return Err(CacheError::BadMagic { found: magic });
    }
    let mut ver = [0u8; 4];
    ver.copy_from_slice(&bytes[4..8]);
    let version = u32::from_le_bytes(ver);
    if version != CACHE_SCHEMA_VERSION {
        return Err(CacheError::VersionMismatch {
            found: version,
            expected: CACHE_SCHEMA_VERSION,
        });
    }
    Ok((&bytes[CACHE_HEADER_LEN..], version))
}

/// A memory-mapped cache file with validated rkyv access. Holds the mmap so
/// the borrow into the archived value stays valid for the lifetime of the
/// handle.
pub struct MmappedCache {
    mmap: Mmap,
}

impl MmappedCache {
    pub fn archived(&self) -> &rkyv::Archived<CachedFile> {
        // Safety guaranteed by validated access at construction time —
        // we re-validate here so callers can't accidentally bypass it.
        let body = &self.mmap[CACHE_HEADER_LEN..];
        // SAFETY: header was validated in `MmappedCache::open`. Access
        // unchecked here would be sound, but stay safe and re-validate so
        // misuse is caught immediately.
        rkyv::access::<rkyv::Archived<CachedFile>, rancor::Error>(body)
            .expect("body validated at open")
    }
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
        self.root.join("files").join(format!("{file_key}.rkyv"))
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
    /// `Err(CacheError::VersionMismatch)` signals a schema drift that the
    /// caller should treat as a cache miss.
    pub fn read_file(&self, file_key: &str) -> std::result::Result<Option<CachedFile>, CacheError> {
        let path = self.file_path(file_key);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path)?;
        let payload = decode_cached_file(&bytes)?;
        Ok(Some(payload))
    }

    /// Open a cached payload as a memory-mapped rkyv archive. Zero-copy
    /// access; intended for the `search` hot path where the archive may be
    /// walked without ever materializing an owned tree.
    pub fn read_file_mmap(
        &self,
        file_key: &str,
    ) -> std::result::Result<Option<MmappedCache>, CacheError> {
        let path = self.file_path(file_key);
        if !path.exists() {
            return Ok(None);
        }
        let file = fs::File::open(&path)?;
        // SAFETY: the file is local and trusted; standard mmap caveats apply.
        let mmap = unsafe { Mmap::map(&file)? };
        // Validate header up front; body is validated on first `archived()` call.
        let (_body, _ver) = split_header(&mmap)?;
        // Pre-validate the rkyv body so subsequent `archived()` calls are cheap.
        let body = &mmap[CACHE_HEADER_LEN..];
        rkyv::access::<rkyv::Archived<CachedFile>, rancor::Error>(body)
            .map_err(|e| CacheError::Decode(format!("rkyv access: {e}")))?;
        Ok(Some(MmappedCache { mmap }))
    }

    /// Write a file's projected payload to disk. Returns the number of bytes
    /// written. Uses a sibling tempfile + atomic rename so a crash mid-write
    /// can't leave a half-written cache entry shadowing a previous good one.
    pub fn write_file(&self, file_key: &str, payload: &CachedFile) -> Result<u64> {
        let bytes = encode_cached_file(payload).map_err(|e| anyhow::anyhow!("{e}"))?;
        let path = self.file_path(file_key);
        let tmp = path.with_extension("rkyv.tmp");
        fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("renaming into {}", path.display()))?;
        Ok(bytes.len() as u64)
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

/// What `load_file` should do given the current manifest state. Factored out
/// so the pure decision logic can be unit-tested without any I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadAction {
    /// No manifest entry exists — bypass cache, fetch live, don't write.
    Bypass,
    /// Manifest entry exists and is fresh; just read from disk.
    UseCache,
    /// Manifest entry exists but TTL expired, payload missing, or version
    /// drifted — re-list projects, compare `last_modified`, refetch this
    /// file if changed.
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

/// Build the typed cached payload from a freshly fetched live API response.
pub fn build_cached_file(file_ref: &FileRef, raw_document: &Value, now: u64) -> CachedFile {
    let document = project_to_cache(raw_document);
    let node_count = count_nodes(&document) as u64;
    CachedFile {
        file_key: file_ref.file_key.clone(),
        name: file_ref.name.clone(),
        project_id: file_ref.project_id.clone(),
        project_name: file_ref.project_name.clone(),
        last_modified: file_ref.last_modified.clone(),
        cached_at_epoch: now,
        node_count,
        document,
    }
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
) -> Result<CachedFile> {
    let file = crate::cmd::fetch_file_json(cfg, &file_ref.file_key, None).await?;
    let payload = build_cached_file(file_ref, &file["document"], now);
    let bytes = cache.write_file(&file_ref.file_key, &payload)?;
    let entry = &mut manifest.files[idx];
    entry.name = file_ref.name.clone();
    entry.project_id = file_ref.project_id.clone();
    entry.project_name = file_ref.project_name.clone();
    entry.last_modified = file_ref.last_modified.clone();
    entry.cached_at_epoch = now;
    entry.status = EntryStatus::Ok;
    entry.error = None;
    entry.node_count = Some(payload.node_count as usize);
    entry.bytes = Some(bytes);
    Ok(payload)
}

/// Best-effort freshness check + lazy refetch for `file_key`. Mutates the
/// manifest in place (and rewrites it to disk) but never returns the
/// refreshed payload — the outer `load_file` re-reads from disk so the
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
    // If the on-disk payload is corrupt or version-mismatched, force a refetch
    // even when the listing says nothing changed.
    let payload_readable = matches!(cache.read_file(file_key), Ok(Some(_)));

    // Listing fetched OK — even if nothing changed, our knowledge of "what's
    // current" is now fresh; bump the global timestamp.
    manifest.updated_at_epoch = now;

    if cached_unchanged && cached_status == EntryStatus::Ok && payload_readable {
        let _ = cache.write_manifest(manifest);
        return;
    }
    if cached_unchanged && cached_status == EntryStatus::NotExportable {
        // Don't burn an API call refetching a known not-exportable file
        // whose timestamp hasn't moved.
        let _ = cache.write_manifest(manifest);
        return;
    }

    // Either the file changed, we previously failed to fetch it, or the
    // on-disk payload is unreadable (corrupt / version drift). Try again.
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
/// - If the file is in the manifest, within TTL, and the on-disk payload is
///   readable: returns it.
/// - If TTL expired, the payload file is missing, or the schema version on
///   disk drifted from the build's `CACHE_SCHEMA_VERSION`: re-queries
///   project listings; refetches the file when its `last_modified` advanced
///   or the on-disk payload is unreadable.
/// - If the file isn't in the manifest at all: falls through to a live fetch
///   that's projected on the fly (the cache is scoped to
///   `FIGMA_PROJECTS_IDS` — random external files don't belong in it).
pub async fn load_file(cfg: &Configuration, file_key: &str) -> Result<CachedFile> {
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
        LoadAction::Bypass => fetch_and_project(cfg, file_key, None).await,
        LoadAction::UseCache => match cache.read_file(file_key) {
            Ok(Some(v)) => Ok(v),
            Ok(None) => fetch_and_project(cfg, file_key, None).await,
            // Version drift on the on-disk payload — fall through to refetch.
            Err(CacheError::VersionMismatch { .. }) | Err(CacheError::TooShort { .. }) => {
                let i = idx.expect("UseCache implies manifest entry");
                try_refresh(cfg, &cache, &mut manifest, file_key, i, now).await;
                match cache.read_file(file_key) {
                    Ok(Some(v)) => Ok(v),
                    _ => fetch_and_project(cfg, file_key, None).await,
                }
            }
            Err(e) => Err(anyhow::anyhow!("{e}")),
        },
        LoadAction::CheckFreshness => {
            // idx is Some by construction of LoadAction
            let i = idx.expect("CheckFreshness implies manifest entry");
            try_refresh(cfg, &cache, &mut manifest, file_key, i, now).await;
            match cache.read_file(file_key) {
                Ok(Some(v)) => Ok(v),
                _ => fetch_and_project(cfg, file_key, None).await,
            }
        }
    }
}

/// Live fetch with on-the-fly projection. Used when the file isn't in the
/// manifest (bypass) or as a fall-through after a failed refresh. Returns a
/// `CachedFile` with no `cached_at_epoch` meaning (set to `now_epoch()`) and
/// empty listing metadata so downstream consumers can treat it uniformly.
async fn fetch_and_project(
    cfg: &Configuration,
    file_key: &str,
    depth: Option<f64>,
) -> Result<CachedFile> {
    let file = crate::cmd::fetch_file_json(cfg, file_key, depth).await?;
    let name = file
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let last_modified = file
        .get("lastModified")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let document = project_to_cache(&file["document"]);
    let node_count = count_nodes(&document) as u64;
    Ok(CachedFile {
        file_key: file_key.to_owned(),
        name,
        project_id: String::new(),
        project_name: String::new(),
        last_modified,
        cached_at_epoch: now_epoch(),
        node_count,
        document,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn project_keeps_structural_fields_drops_paint() {
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
        let s = project_to_cache(&raw);
        assert_eq!(s.id, "1:2");
        assert_eq!(s.type_, "FRAME");
        assert_eq!(s.name, "Hero");
        assert!(s.visible);
        let bounds = s.bounds.expect("bounds preserved");
        assert_eq!(bounds.width as i64, 1440);
        assert_eq!(s.children.len(), 2);
        assert_eq!(s.children[0].name, "Title");
        assert!(s.children[0].visible);
        assert!(!s.children[1].visible);
        // The CacheNode is a struct, so paint/character fields don't exist —
        // the projection drops them by construction.
    }

    #[test]
    fn count_nodes_includes_self_plus_descendants() {
        let n = CacheNode {
            id: "a".into(),
            type_: String::new(),
            name: String::new(),
            visible: true,
            bounds: None,
            children: vec![
                CacheNode {
                    id: "b".into(),
                    type_: String::new(),
                    name: String::new(),
                    visible: true,
                    bounds: None,
                    children: vec![],
                },
                CacheNode {
                    id: "c".into(),
                    type_: String::new(),
                    name: String::new(),
                    visible: true,
                    bounds: None,
                    children: vec![CacheNode {
                        id: "d".into(),
                        type_: String::new(),
                        name: String::new(),
                        visible: true,
                        bounds: None,
                        children: vec![],
                    }],
                },
            ],
        };
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

    fn leaf(id: &str, name: &str, type_: &str) -> CacheNode {
        CacheNode {
            id: id.into(),
            type_: type_.into(),
            name: name.into(),
            visible: true,
            bounds: None,
            children: vec![],
        }
    }

    fn sample_cached_file() -> CachedFile {
        CachedFile {
            file_key: "K".into(),
            name: "F".into(),
            project_id: "P".into(),
            project_name: "PN".into(),
            last_modified: "2026-05-11T00:00:00Z".into(),
            cached_at_epoch: 42,
            node_count: 3,
            document: CacheNode {
                id: "0:0".into(),
                type_: "DOCUMENT".into(),
                name: "doc".into(),
                visible: true,
                bounds: None,
                children: vec![CacheNode {
                    id: "1:0".into(),
                    type_: "CANVAS".into(),
                    name: "Page".into(),
                    visible: true,
                    bounds: None,
                    children: vec![leaf("1:1", "Hero", "FRAME")],
                }],
            },
        }
    }

    #[test]
    fn rkyv_roundtrip_preserves_payload() {
        let original = sample_cached_file();
        let bytes = encode_cached_file(&original).unwrap();
        let decoded = decode_cached_file(&bytes).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn encoded_payload_has_magic_and_version_prefix() {
        let original = sample_cached_file();
        let bytes = encode_cached_file(&original).unwrap();
        assert!(bytes.len() >= CACHE_HEADER_LEN);
        assert_eq!(&bytes[..4], &CACHE_MAGIC);
        let mut ver = [0u8; 4];
        ver.copy_from_slice(&bytes[4..8]);
        assert_eq!(u32::from_le_bytes(ver), CACHE_SCHEMA_VERSION);
    }

    #[test]
    fn version_mismatch_is_typed_error() {
        let original = sample_cached_file();
        let mut bytes = encode_cached_file(&original).unwrap();
        // Bump the on-disk version to something the build won't recognize.
        bytes[4..8].copy_from_slice(&(CACHE_SCHEMA_VERSION + 1).to_le_bytes());
        match decode_cached_file(&bytes) {
            Err(CacheError::VersionMismatch { found, expected }) => {
                assert_eq!(found, CACHE_SCHEMA_VERSION + 1);
                assert_eq!(expected, CACHE_SCHEMA_VERSION);
            }
            other => panic!("expected VersionMismatch, got {other:?}"),
        }
    }

    #[test]
    fn magic_mismatch_is_typed_error() {
        let original = sample_cached_file();
        let mut bytes = encode_cached_file(&original).unwrap();
        bytes[..4].copy_from_slice(b"NOPE");
        match decode_cached_file(&bytes) {
            Err(CacheError::BadMagic { found }) => assert_eq!(&found, b"NOPE"),
            other => panic!("expected BadMagic, got {other:?}"),
        }
    }

    #[test]
    fn too_short_is_typed_error() {
        match decode_cached_file(&[1, 2, 3]) {
            Err(CacheError::TooShort { len }) => assert_eq!(len, 3),
            other => panic!("expected TooShort, got {other:?}"),
        }
    }
}

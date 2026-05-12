//! Local file cache.
//!
//! Stores a slimmed-down projection of each Figma file under `files/`. Each
//! file gets two pieces on disk:
//!
//! - `files/{file_key}.rkyv` — rkyv-encoded `CachedFile` payload (structural
//!   projection of the document). Present only when status is `Ok`.
//! - `files/{file_key}.meta.json` — per-file sidecar with status, listing
//!   metadata, and timestamps. Always present (the only way to remember
//!   `Failed`/`NotExportable` markers between runs).
//!
//! On-disk payload format: `[4-byte magic "FXC\0"][4-byte u32 LE version][rkyv body]`.
//! Magic catches "wrong kind of file" cases; version catches schema drift
//! (silent refetch on mismatch).
//!
//! Layout root is resolved via `dirs::cache_dir()` (e.g.
//! `~/Library/Caches/figma-explorer/` on macOS), overridable via
//! `FIGMA_EXPLORER_CACHE_DIR`. There is no central manifest — every piece of
//! per-file state lives in its own `.meta.json` so concurrent writers touching
//! different file_keys never share a write path.
//!
//! Multi-repo coexistence: each meta records the `project_id` that produced
//! it. Operations that prune (`cache prefetch` invalidation) only touch metas
//! whose `project_id` is in the current process's `FIGMA_PROJECTS_IDS`. Files
//! claimed by other project sets are out of jurisdiction.
//!
//! Endianness: rkyv archives are not portable across endianness. This cache
//! is single-user and local; we don't try to support shared/transferred
//! cache directories across machines.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use figma_api::apis::configuration::Configuration;
use figma_api::apis::projects_api as api;
use figma_api::models::Comment;
use memmap2::Mmap;
use rkyv::rancor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::comment_assoc::{self, associate, AssociatedComment};
use crate::into_anyhow;
use crate::node::Bounds;

pub const DEFAULT_TTL_SECS: u64 = 3600;

/// Sidecar format version. Bumped when the on-disk shape of
/// `{file_key}.comments.json` changes. Sidecars older than the current
/// version are treated as stale → refetched on next access. Stored on
/// [`FileMeta::comments_schema_version`].
///
/// v1 = `Vec<AssociatedComment>` (pre-computed node associations).
/// (v0 / missing = legacy `Vec<Comment>` shape from before pre-association.)
pub const COMMENTS_SCHEMA_VERSION: u32 = 1;

/// Sidecar format version for `{file_key}.full.json.gz` — the raw
/// `/v1/files/{key}` response body, gzip-compressed. v1 is the initial
/// shape (untouched Figma JSON). A future bump signals "the wire format
/// changed in a way our reader cares about; refetch."
pub const FULL_SCHEMA_VERSION: u32 = 1;

/// Sidecar format version for `{file_key}.variables.json` — the raw
/// `/v1/files/{key}/variables/local` response body. v1 is the initial
/// shape.
pub const VARIABLES_SCHEMA_VERSION: u32 = 1;

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
                write!(
                    f,
                    "cache file too short: {len} bytes (need at least {CACHE_HEADER_LEN})"
                )
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
    let id = node
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let type_ = node
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let name = node
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    // Figma omits `visible` when the node is visible; missing → true.
    let visible = !matches!(node.get("visible"), Some(Value::Bool(false)));
    let bounds = node.get("absoluteBoundingBox").and_then(parse_bounds);
    let children = node
        .get("children")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(project_to_cache).collect())
        .unwrap_or_default();
    CacheNode {
        id,
        type_,
        name,
        visible,
        bounds,
        children,
    }
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
        let body = &self.mmap[CACHE_HEADER_LEN..];
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
    /// Transient failure. Retried on next access.
    Failed,
}

/// Per-file sidecar describing what we know about a cached file_key. Always
/// present for any file_key we've tried to cache (even if the fetch failed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMeta {
    pub file_key: String,
    pub name: String,
    /// Project the file was claimed by at last successful listing. Empty when
    /// the file was fetched via direct URL with no listing context.
    pub project_id: String,
    pub project_name: String,
    /// `lastModified` from the project listing (or file response) — drives
    /// invalidation.
    pub last_modified: String,
    pub cached_at_epoch: u64,
    /// Last time we confirmed (via listing or fresh fetch) that this is the
    /// current `last_modified`. TTL is measured from here.
    pub last_listed_at_epoch: u64,
    pub status: EntryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Epoch seconds of the last successful comments fetch for this file. None
    /// when comments have never been fetched (predates the feature, or never
    /// polled). Drives the `comments --max-age-secs` decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments_fetched_at_epoch: Option<u64>,
    /// Stable fingerprint of the comments sidecar (sorted ids + per-comment
    /// signature). Lets future tooling cheaply answer "did anything change
    /// since the last poll?" without re-diffing the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments_fingerprint: Option<String>,
    /// Set when the most recent comments fetch failed but the tree refresh
    /// succeeded. Persists until a subsequent fetch clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments_error: Option<String>,
    /// On-disk format of the `.comments.json` sidecar. `None` (or `< 1`) means
    /// either no sidecar has ever been written or the existing sidecar is in a
    /// pre-pre-association shape; callers treat such metas as needing a
    /// refetch the next time comments are requested. Set to
    /// [`COMMENTS_SCHEMA_VERSION`] after every successful sidecar write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments_schema_version: Option<u32>,
    /// Epoch seconds when the full raw-JSON sidecar (`{file_key}.full.json.gz`)
    /// was last written. `None` means it's never been written. Drives the
    /// `node-info` cache-only path: a missing sidecar with `cache_only=true`
    /// errors with a "run cache prefetch" hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_fetched_at_epoch: Option<u64>,
    /// Size of the (compressed) `.full.json.gz` sidecar in bytes. Surfaced in
    /// `cache prefetch` summaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_bytes: Option<u64>,
    /// On-disk format version stamped on the `.full.json.gz` sidecar. See
    /// [`FULL_SCHEMA_VERSION`]. Mismatched / missing → treat as stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_schema_version: Option<u32>,
    /// Epoch seconds of the last successful local-variables fetch. `None`
    /// when never fetched (cache predates the feature, or the account doesn't
    /// have Variables REST API access).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables_fetched_at_epoch: Option<u64>,
    /// Size of the `.variables.json` sidecar in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables_bytes: Option<u64>,
    /// Last error from the variables fetch, if any. Often "403 Forbidden" for
    /// non-Enterprise accounts. Surfaced by `node-info` so the user knows why
    /// the variables block is missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables_error: Option<String>,
    /// Variables sidecar schema version. See [`VARIABLES_SCHEMA_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables_schema_version: Option<u32>,
}

impl FileMeta {
    pub fn from_success(file_ref: &FileRef, payload: &CachedFile, bytes: u64, now: u64) -> Self {
        FileMeta {
            file_key: payload.file_key.clone(),
            name: payload.name.clone(),
            project_id: file_ref.project_id.clone(),
            project_name: file_ref.project_name.clone(),
            last_modified: payload.last_modified.clone(),
            cached_at_epoch: now,
            last_listed_at_epoch: now,
            status: EntryStatus::Ok,
            error: None,
            node_count: Some(payload.node_count as usize),
            bytes: Some(bytes),
            comments_fetched_at_epoch: None,
            comments_fingerprint: None,
            comments_error: None,
            comments_schema_version: None,
            full_fetched_at_epoch: None,
            full_bytes: None,
            full_schema_version: None,
            variables_fetched_at_epoch: None,
            variables_bytes: None,
            variables_error: None,
            variables_schema_version: None,
        }
    }
}

pub struct CacheDir {
    pub root: PathBuf,
}

impl CacheDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(self.files_dir())
            .with_context(|| format!("creating {}", self.files_dir().display()))?;
        Ok(())
    }

    pub fn files_dir(&self) -> PathBuf {
        self.root.join("files")
    }

    pub fn file_path(&self, file_key: &str) -> PathBuf {
        self.files_dir().join(format!("{file_key}.rkyv"))
    }

    pub fn meta_path(&self, file_key: &str) -> PathBuf {
        self.files_dir().join(format!("{file_key}.meta.json"))
    }

    pub fn comments_path(&self, file_key: &str) -> PathBuf {
        self.files_dir().join(format!("{file_key}.comments.json"))
    }

    /// Path of the gzipped full-JSON sidecar (raw `/v1/files/{key}` body).
    /// The structural cache (`.rkyv`) drops most fields; this sidecar keeps
    /// them so `node-info` (and future migrations of tokens/assets/context)
    /// can run offline.
    pub fn full_path(&self, file_key: &str) -> PathBuf {
        self.files_dir().join(format!("{file_key}.full.json.gz"))
    }

    /// Path of the variables sidecar (raw `/v1/files/{key}/variables/local`
    /// body, plaintext JSON).
    pub fn variables_path(&self, file_key: &str) -> PathBuf {
        self.files_dir().join(format!("{file_key}.variables.json"))
    }

    /// Read the comments sidecar for `file_key`. `Ok(None)` when the sidecar
    /// doesn't exist (file never polled for comments) *or* when its on-disk
    /// shape doesn't match the current `AssociatedComment` format — pre-
    /// pre-association sidecars (raw `Comment` arrays) fail this deserialize
    /// and are surfaced as "not cached," which steers the caller into the
    /// refetch path. Migration is automatic on the next fetch.
    pub fn read_comments(&self, file_key: &str) -> Result<Option<Vec<AssociatedComment>>> {
        let p = self.comments_path(file_key);
        if !p.exists() {
            return Ok(None);
        }
        let s = fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        match serde_json::from_str::<Vec<AssociatedComment>>(&s) {
            Ok(v) => Ok(Some(v)),
            Err(e) => {
                // Legacy raw-Comment array, or actual corruption — same
                // treatment either way: treat as "no usable sidecar" and let
                // the caller refresh.
                eprintln!(
                    "cache: comments sidecar for {file_key} not in current format ({e}); will refetch on next access"
                );
                Ok(None)
            }
        }
    }

    pub fn write_comments(&self, file_key: &str, comments: &[AssociatedComment]) -> Result<()> {
        let path = self.comments_path(file_key);
        let bytes = serde_json::to_vec_pretty(comments)?;
        atomic_write(&path, &bytes)
    }

    pub fn delete_comments(&self, file_key: &str) -> Result<()> {
        let p = self.comments_path(file_key);
        if p.exists() {
            fs::remove_file(&p).with_context(|| format!("removing {}", p.display()))?;
        }
        Ok(())
    }

    pub fn read_meta(&self, file_key: &str) -> Result<Option<FileMeta>> {
        let p = self.meta_path(file_key);
        if !p.exists() {
            return Ok(None);
        }
        let s = fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        let m = serde_json::from_str(&s).with_context(|| format!("parsing {}", p.display()))?;
        Ok(Some(m))
    }

    pub fn write_meta(&self, meta: &FileMeta) -> Result<()> {
        let path = self.meta_path(&meta.file_key);
        let bytes = serde_json::to_vec_pretty(meta)?;
        atomic_write(&path, &bytes)
    }

    /// Delete `{file_key}.meta.json` first, then every sidecar paired with
    /// the same file_key (rkyv payload, comments, full-JSON, variables). The
    /// meta-first ordering matters: readers seeing no meta treat the entry as
    /// uncached, so a transient "meta gone but other files linger" window is
    /// benign.
    pub fn delete_entry(&self, file_key: &str) -> Result<()> {
        let meta = self.meta_path(file_key);
        let payload = self.file_path(file_key);
        let comments = self.comments_path(file_key);
        let full = self.full_path(file_key);
        let variables = self.variables_path(file_key);
        if meta.exists() {
            fs::remove_file(&meta).with_context(|| format!("removing {}", meta.display()))?;
        }
        if payload.exists() {
            fs::remove_file(&payload).with_context(|| format!("removing {}", payload.display()))?;
        }
        if comments.exists() {
            fs::remove_file(&comments)
                .with_context(|| format!("removing {}", comments.display()))?;
        }
        if full.exists() {
            fs::remove_file(&full).with_context(|| format!("removing {}", full.display()))?;
        }
        if variables.exists() {
            fs::remove_file(&variables)
                .with_context(|| format!("removing {}", variables.display()))?;
        }
        Ok(())
    }

    /// List every meta currently on disk. Used by `cache prefetch` (to
    /// invalidate stale entries against a fresh listing) and `cache clear`
    /// (to sweep orphans).
    pub fn list_metas(&self) -> Result<Vec<FileMeta>> {
        let dir = self.files_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_owned(),
                None => continue,
            };
            if !name.ends_with(".meta.json") {
                continue;
            }
            let s = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("cache: skipping unreadable meta {}: {e}", path.display());
                    continue;
                }
            };
            match serde_json::from_str::<FileMeta>(&s) {
                Ok(m) => out.push(m),
                Err(e) => {
                    eprintln!("cache: skipping malformed meta {}: {e}", path.display());
                }
            }
        }
        Ok(out)
    }

    /// Read a cached payload by file_key. Returns `Ok(None)` if no file exists.
    /// `Err(CacheError::VersionMismatch)` (and friends) signal corruption /
    /// schema drift that the caller should treat as a cache miss.
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
        let (_body, _ver) = split_header(&mmap)?;
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
        atomic_write(&path, &bytes)?;
        Ok(bytes.len() as u64)
    }
}

/// Write `bytes` to `path` atomically: tempfile in the same directory, then
/// rename. Crashes leave the previous file intact.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating tempfile in {}", parent.display()))?;
    tmp.write_all(bytes)
        .with_context(|| format!("writing tempfile for {}", path.display()))?;
    tmp.persist(path)
        .map_err(|e| anyhow::anyhow!("persisting {}: {}", path.display(), e))?;
    Ok(())
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

/// Resolve the cache root.
///
/// Precedence: `FIGMA_EXPLORER_CACHE_DIR` env, then `dirs::cache_dir()`
/// (e.g. `~/Library/Caches/figma-explorer/`), with a final fallback to a
/// CWD-local `cache/` directory for headless environments where neither
/// works.
pub fn default_dir() -> PathBuf {
    if let Ok(s) = std::env::var("FIGMA_EXPLORER_CACHE_DIR") {
        if !s.is_empty() {
            return PathBuf::from(s);
        }
    }
    dirs::cache_dir()
        .map(|d| d.join("figma-explorer"))
        .unwrap_or_else(|| PathBuf::from("cache"))
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

/// Outcome of `load_file`'s freshness decision over a `FileMeta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadAction {
    /// Meta is fresh and status=Ok — serve the cached payload directly.
    UseCache,
    /// Meta says NotExportable and is still fresh — surface the cached error
    /// without burning an API call.
    NotExportableCached,
    /// Either there's no meta, TTL expired, or the cached status is Failed —
    /// caller should attempt a refresh (single-project listing if possible)
    /// and then a live fetch.
    Refresh,
}

pub fn decide_action(
    meta: Option<&FileMeta>,
    payload_exists: bool,
    now: u64,
    ttl_secs: u64,
) -> LoadAction {
    let Some(m) = meta else {
        return LoadAction::Refresh;
    };
    let elapsed = now.saturating_sub(m.last_listed_at_epoch);
    let fresh = elapsed < ttl_secs;
    match m.status {
        EntryStatus::Ok if fresh && payload_exists => LoadAction::UseCache,
        EntryStatus::NotExportable if fresh => LoadAction::NotExportableCached,
        _ => LoadAction::Refresh,
    }
}

/// Attempt to refresh a single file_key via a one-project listing.
///
/// Returns:
/// - `Ok(Some(payload))` — we successfully refetched or confirmed the cached
///   payload is current; payload is returned.
/// - `Ok(None)` — listing was attempted but didn't yield a decision (file
///   absent from the project, or marker meta confirmed unchanged). The
///   caller should fall back to serving stale or fetching live.
/// - `Err(e)` — propagate a hard error (e.g. NotExportable that the caller
///   should surface to the user).
async fn try_refresh_single(
    cfg: &Configuration,
    cache: &CacheDir,
    file_key: &str,
    project_id: &str,
    meta: Option<&FileMeta>,
    now: u64,
) -> Result<Option<CachedFile>> {
    let listings = match list_project_files(cfg, std::slice::from_ref(&project_id.to_owned())).await
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cache: freshness check failed for {file_key}: {e:#} — serving stale");
            return Ok(None);
        }
    };

    let Some(current) = listings.into_iter().find(|f| f.file_key == file_key) else {
        // File no longer in the project listing — out of jurisdiction for
        // automatic deletion (that's `cache prefetch`'s job). Serve stale.
        return Ok(None);
    };

    let cached_unchanged = meta.is_some_and(|m| m.last_modified == current.last_modified);
    let cached_status = meta.map(|m| m.status).unwrap_or(EntryStatus::Failed);
    let payload_readable = matches!(cache.read_file(file_key), Ok(Some(_)));

    if cached_unchanged && cached_status == EntryStatus::Ok && payload_readable {
        // Bump last_listed_at to reset TTL window. Even though the document
        // is unchanged we still re-fetch comments — Figma's `lastModified`
        // doesn't tick for comment activity, so this is the only path that
        // observes new comments on otherwise-stable files.
        if let Some(m) = meta {
            let mut updated = m.clone();
            updated.last_listed_at_epoch = now;
            // Keep project info fresh from the listing in case it drifted.
            updated.project_name = current.project_name.clone();
            updated.name = current.name.clone();
            fetch_comments_into_meta(cfg, cache, file_key, now, &mut updated).await;
            let _ = cache.write_meta(&updated);
            // Register comm synths for any newly-arrived comments — the
            // file synth was already interned on the previous fetch path,
            // so it's safe to look up directly here.
            if let Ok(state) = crate::synth::SynthState::load(cache) {
                if let Some(file_synth) = state.file_synth(file_key) {
                    if let Ok(Some(comments)) = cache.read_comments(file_key) {
                        intern_comment_synths(cache, file_synth, &comments);
                    }
                }
            }
        }
        return cache
            .read_file(file_key)
            .map_err(|e| anyhow::anyhow!("{e}"));
    }

    if cached_unchanged && cached_status == EntryStatus::NotExportable {
        // Known-bad community file, timestamp unchanged — don't burn an API
        // call. Bump listed_at to silence the TTL until next change.
        if let Some(m) = meta {
            let mut updated = m.clone();
            updated.last_listed_at_epoch = now;
            let _ = cache.write_meta(&updated);
        }
        anyhow::bail!("file {file_key} is not exportable (cached marker, unchanged on Figma)");
    }

    // Either last_modified changed, prior status was Failed, or payload is
    // unreadable — refetch.
    Ok(Some(
        fetch_and_cache(cfg, cache, file_key, Some(&current), now).await?,
    ))
}

/// Live fetch + write to cache. `file_ref` carries project context when we
/// have a listing in hand; without it we record `project_id=""` (direct-URL
/// access outside any configured project).
async fn fetch_and_cache(
    cfg: &Configuration,
    cache: &CacheDir,
    file_key: &str,
    file_ref: Option<&FileRef>,
    now: u64,
) -> Result<CachedFile> {
    match crate::cmd::fetch_file_json(cfg, file_key, None).await {
        Ok(file) => {
            let last_modified = file
                .get("lastModified")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let name = file
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let (project_id, project_name) = file_ref
                .map(|fr| (fr.project_id.clone(), fr.project_name.clone()))
                .unwrap_or_default();
            let synthetic_ref = FileRef {
                file_key: file_key.to_owned(),
                name: name.clone(),
                last_modified: last_modified.clone(),
                project_id,
                project_name,
            };
            let payload = build_cached_file(&synthetic_ref, &file["document"], now);
            let bytes = cache.write_file(file_key, &payload)?;
            let mut meta = FileMeta::from_success(&synthetic_ref, &payload, bytes, now);
            // Fetch comments alongside the structural payload — same cadence,
            // best-effort. A failure flips `meta.comments_error` but does not
            // poison the tree refresh. Comments are pre-associated against
            // the just-written tree at line 805 above.
            fetch_comments_into_meta(cfg, cache, file_key, now, &mut meta).await;
            cache.write_meta(&meta)?;
            // Intern synth IDs so downstream commands (`ls`, etc.) can render
            // qualified `file:N:x:y` / `file:N:comm:M` lines. File synth is
            // assigned (or retrieved) here; comment synths are interned
            // immediately after using that synth as their scope.
            // Best-effort: a synth save failure logs and continues.
            let file_synth = match crate::synth::with_lock(cache, |s| {
                if !meta.project_id.is_empty() {
                    s.intern_project(&meta.project_id);
                }
                s.intern_file(&meta.file_key)
            }) {
                Ok(synth) => Some(synth),
                Err(e) => {
                    eprintln!("cache: synth intern failed for {file_key}: {e:#}");
                    None
                }
            };
            if let Some(synth) = file_synth {
                if let Ok(Some(comments)) = cache.read_comments(file_key) {
                    intern_comment_synths(cache, synth, &comments);
                }
            }
            Ok(payload)
        }
        Err(e) => {
            let msg = format!("{e:#}");
            let status = if is_not_exportable_error(&msg) {
                EntryStatus::NotExportable
            } else {
                EntryStatus::Failed
            };
            // Record a marker meta so subsequent loads don't keep retrying
            // NotExportable on every call.
            let (project_id, project_name, name, last_modified) = file_ref
                .map(|fr| {
                    (
                        fr.project_id.clone(),
                        fr.project_name.clone(),
                        fr.name.clone(),
                        fr.last_modified.clone(),
                    )
                })
                .unwrap_or_default();
            let marker = FileMeta {
                file_key: file_key.to_owned(),
                name,
                project_id,
                project_name,
                last_modified,
                cached_at_epoch: now,
                last_listed_at_epoch: now,
                status,
                error: Some(msg.clone()),
                node_count: None,
                bytes: None,
                comments_fetched_at_epoch: None,
                comments_fingerprint: None,
                comments_error: None,
                comments_schema_version: None,
                full_fetched_at_epoch: None,
                full_bytes: None,
                full_schema_version: None,
                variables_fetched_at_epoch: None,
                variables_bytes: None,
                variables_error: None,
                variables_schema_version: None,
            };
            // Also drop any stale payload — meta-first ordering.
            let _ = cache.delete_entry(file_key);
            let _ = cache.write_meta(&marker);
            Err(e)
        }
    }
}

/// Stable signature over the comment set so future polling tooling can answer
/// "did anything change?" without re-diffing. Captures id, message text,
/// resolution state, and reaction count — the fields that change in practice.
/// Sorted by id first to be insensitive to API response ordering.
pub fn fingerprint_comments(comments: &[Comment]) -> String {
    let mut entries: Vec<(&str, &str, &str, usize)> = comments
        .iter()
        .map(|c| {
            let resolved = c
                .resolved_at
                .as_ref()
                .and_then(|outer| outer.as_deref())
                .unwrap_or("");
            (
                c.id.as_str(),
                c.message.as_str(),
                resolved,
                c.reactions.len(),
            )
        })
        .collect();
    entries.sort_by_key(|e| e.0);
    let mut h = DefaultHasher::new();
    for (id, msg, resolved, n) in entries {
        id.hash(&mut h);
        msg.hash(&mut h);
        resolved.hash(&mut h);
        n.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

/// Fetch comments for `file_key`, pre-associate each one with its anchor
/// node, and update `meta` in place. On success: writes the `.comments.json`
/// sidecar as a `Vec<AssociatedComment>`, stamps the epoch, fingerprint, and
/// schema version, clears `comments_error`. On failure: leaves any prior
/// sidecar untouched, sets `comments_error`, logs to stderr.
///
/// Pre-association reads the cached tree document so it can resolve each
/// comment's anchor up front. The tree **must already be on disk** before
/// this runs — callers in `fetch_and_cache` write the tree first; the
/// `try_refresh_single` path only runs when the meta already exists (which
/// implies the tree exists).
///
/// Best-effort: returns `()` even on API failure. The error is reflected
/// in `meta.comments_error` so the caller can write the updated meta and
/// downstream tooling can surface staleness.
pub async fn fetch_comments_into_meta(
    cfg: &Configuration,
    cache: &CacheDir,
    file_key: &str,
    now: u64,
    meta: &mut FileMeta,
) {
    let url = format!("{}/v1/files/{}/comments", cfg.base_path, file_key);
    let raw_json = match crate::cmd::get_json(cfg, &url).await {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("{e:#}");
            eprintln!("comments: fetch failed for {file_key}: {msg}");
            meta.comments_error = Some(msg);
            return;
        }
    };
    match parse_comments_lenient(&raw_json) {
        Ok(raw) => {
            // Fingerprint over raw API state so it's invariant under future
            // threshold / association changes.
            let fp = fingerprint_comments(&raw);

            // Pre-compute node associations. Requires the cached tree.
            let document = match cache.read_file(file_key) {
                Ok(Some(payload)) => payload,
                Ok(None) => {
                    let msg =
                        format!("tree not cached for {file_key}; cannot pre-associate comments");
                    eprintln!("comments: {msg}");
                    meta.comments_error = Some(msg);
                    return;
                }
                Err(e) => {
                    let msg = format!("reading tree for association: {e}");
                    eprintln!("comments: {file_key}: {msg}");
                    meta.comments_error = Some(msg);
                    return;
                }
            };
            let associated = associate(
                &document.document,
                &raw,
                comment_assoc::DEFAULT_ASSOC_THRESHOLD_PX,
            );

            match cache.write_comments(file_key, &associated) {
                Ok(()) => {
                    meta.comments_fetched_at_epoch = Some(now);
                    meta.comments_fingerprint = Some(fp);
                    meta.comments_schema_version = Some(COMMENTS_SCHEMA_VERSION);
                    meta.comments_error = None;
                }
                Err(e) => {
                    let msg = format!("write_comments: {e:#}");
                    eprintln!("comments: {file_key}: {msg}");
                    meta.comments_error = Some(msg);
                }
            }
        }
        Err(e) => {
            let msg = format!("parsing comments response: {e:#}");
            eprintln!("comments: {file_key}: {msg}");
            meta.comments_error = Some(msg);
        }
    }
}

/// Pull a `Vec<Comment>` out of the raw `/v1/files/{key}/comments` JSON,
/// tolerating real-world spec drift. Specifically:
///
/// - `client_meta: null` (some deleted/orphan threads) → substituted with a
///   `Vector` at origin so the untagged enum can deserialize. The comment
///   then falls into the canvas-level bucket at association time.
/// - `parent_id: ""` (Figma's wire format for top-level threads — the spec
///   models it as `Option<String>` so empty-string ≠ "no parent") →
///   rewritten to `null` so downstream "is this a head?" checks work.
/// - Comments that fail to deserialize for any other reason are logged and
///   skipped rather than aborting the whole batch — one weird comment must
///   not poison an entire file's sidecar.
fn parse_comments_lenient(raw: &Value) -> Result<Vec<Comment>> {
    let arr = raw
        .get("comments")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("response missing `comments` array"))?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let mut v = entry.clone();
        if let Some(obj) = v.as_object_mut() {
            // Normalize null/missing client_meta → default Vector at origin.
            // Figma occasionally returns null for older threads where the
            // anchor was deleted along with its target.
            let needs_default = matches!(obj.get("client_meta"), None | Some(Value::Null));
            if needs_default {
                obj.insert(
                    "client_meta".into(),
                    serde_json::json!({ "x": 0.0, "y": 0.0 }),
                );
            }
            // Normalize empty-string parent_id → null so `Option<String>`
            // round-trips to `None` for thread heads.
            if matches!(obj.get("parent_id"), Some(Value::String(s)) if s.is_empty()) {
                obj.insert("parent_id".into(), Value::Null);
            }
        }
        match serde_json::from_value::<Comment>(v) {
            Ok(c) => out.push(c),
            Err(e) => {
                let id_hint = entry.get("id").and_then(Value::as_str).unwrap_or("?");
                eprintln!("comments: skipping malformed comment {id_hint}: {e}");
            }
        }
    }
    Ok(out)
}

/// Intern every comment id from `comments` under `file_synth`. Best-effort —
/// errors logged but never propagated, since the sidecar is the source of
/// truth and synth IDs are recovered on the next prefetch otherwise.
fn intern_comment_synths(cache: &CacheDir, file_synth: u32, comments: &[AssociatedComment]) {
    if comments.is_empty() {
        return;
    }
    if let Err(e) = crate::synth::with_lock(cache, |s| {
        for c in comments {
            s.intern_comment(file_synth, &c.comment_id);
        }
    }) {
        eprintln!("cache: comment-synth intern failed for file_synth={file_synth}: {e:#}");
    }
}

/// Cache-first loader for the structural commands.
///
/// Flow (see plan):
/// 1. Read meta. If fresh + Ok + payload present → return.
/// 2. If meta says NotExportable and is fresh → return the cached error.
/// 3. If TTL expired and `meta.project_id ∈ FIGMA_PROJECTS_IDS` → list that
///    one project, decide refetch vs. serve-stale vs. confirm-current.
/// 4. Otherwise → fetch live. The fetch always writes meta+payload (or a
///    failure marker meta on error).
/// 5. Rkyv corruption / version mismatch is treated as a cache miss: the
///    entry is deleted and we fall through to refetch.
pub async fn load_file(cfg: &Configuration, file_key: &str) -> Result<CachedFile> {
    let cache = CacheDir::new(default_dir());
    cache.ensure()?;
    let now = now_epoch();
    let mut meta = cache.read_meta(file_key).ok().flatten();

    // Sanity sweep: if meta claims Ok but payload is missing or corrupt,
    // drop the entry so the freshness decision doesn't try to serve junk.
    if let Some(m) = &meta {
        if m.status == EntryStatus::Ok {
            let payload_path = cache.file_path(file_key);
            if !payload_path.exists() {
                let _ = cache.delete_entry(file_key);
                meta = None;
            } else if let Err(
                CacheError::VersionMismatch { .. }
                | CacheError::BadMagic { .. }
                | CacheError::TooShort { .. }
                | CacheError::Decode(_),
            ) = cache.read_file(file_key)
            {
                let _ = cache.delete_entry(file_key);
                meta = None;
            }
        }
    }

    let payload_exists = cache.file_path(file_key).exists();
    match decide_action(meta.as_ref(), payload_exists, now, DEFAULT_TTL_SECS) {
        LoadAction::UseCache => {
            return cache
                .read_file(file_key)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .ok_or_else(|| {
                    anyhow::anyhow!("cache: meta says Ok but payload vanished mid-read")
                });
        }
        LoadAction::NotExportableCached => {
            let err = meta
                .as_ref()
                .and_then(|m| m.error.clone())
                .unwrap_or_else(|| "file marked not exportable".to_owned());
            anyhow::bail!("{err}");
        }
        LoadAction::Refresh => { /* fall through */ }
    }

    // Refresh path. Prefer a single-project listing when we have a project
    // hint that matches the user's env — that's cheaper than a blind refetch
    // and lets us preserve the cache entry when last_modified is unchanged.
    let env_projects = parse_project_ids_env();
    let project_hint = meta.as_ref().map(|m| m.project_id.as_str()).unwrap_or("");
    if !project_hint.is_empty() && env_projects.iter().any(|p| p == project_hint) {
        match try_refresh_single(cfg, &cache, file_key, project_hint, meta.as_ref(), now).await {
            Ok(Some(payload)) => return Ok(payload),
            Ok(None) => {
                // No decision possible. Serve stale if we have an Ok payload.
                if let Some(m) = &meta {
                    if m.status == EntryStatus::Ok {
                        if let Ok(Some(v)) = cache.read_file(file_key) {
                            return Ok(v);
                        }
                    }
                }
                // Otherwise fall through to live fetch.
            }
            Err(e) => return Err(e),
        }
    }

    // Final fallback: blind live fetch (cold load, or refresh fell through).
    fetch_and_cache(cfg, &cache, file_key, None, now).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

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

    fn ok_meta(now: u64, listed_at: u64) -> FileMeta {
        FileMeta {
            file_key: "K".into(),
            name: "n".into(),
            project_id: "10".into(),
            project_name: "P".into(),
            last_modified: "ts".into(),
            cached_at_epoch: now,
            last_listed_at_epoch: listed_at,
            status: EntryStatus::Ok,
            error: None,
            node_count: Some(1),
            bytes: Some(100),
            comments_fetched_at_epoch: None,
            comments_fingerprint: None,
            comments_error: None,
            comments_schema_version: None,
            full_fetched_at_epoch: None,
            full_bytes: None,
            full_schema_version: None,
            variables_fetched_at_epoch: None,
            variables_bytes: None,
            variables_error: None,
            variables_schema_version: None,
        }
    }

    #[test]
    fn decide_action_no_meta_refreshes() {
        assert_eq!(decide_action(None, false, 0, 3600), LoadAction::Refresh);
    }

    #[test]
    fn decide_action_within_ttl_uses_cache() {
        let m = ok_meta(0, 500);
        assert_eq!(
            decide_action(Some(&m), true, 1000, 3600),
            LoadAction::UseCache
        );
    }

    #[test]
    fn decide_action_ttl_expired_refreshes() {
        let m = ok_meta(0, 1000);
        assert_eq!(
            decide_action(Some(&m), true, 6000, 3600),
            LoadAction::Refresh
        );
    }

    #[test]
    fn decide_action_missing_payload_forces_refresh() {
        let m = ok_meta(0, 500);
        assert_eq!(
            decide_action(Some(&m), false, 1000, 3600),
            LoadAction::Refresh
        );
    }

    #[test]
    fn decide_action_not_exportable_within_ttl_surfaces_error() {
        let mut m = ok_meta(0, 500);
        m.status = EntryStatus::NotExportable;
        assert_eq!(
            decide_action(Some(&m), false, 1000, 3600),
            LoadAction::NotExportableCached
        );
    }

    #[test]
    fn decide_action_failed_status_always_refreshes() {
        let mut m = ok_meta(0, 500);
        m.status = EntryStatus::Failed;
        assert_eq!(
            decide_action(Some(&m), false, 1000, 3600),
            LoadAction::Refresh
        );
    }

    #[test]
    fn decide_action_boundary_ttl_treated_as_expired() {
        let m = ok_meta(0, 0);
        assert_eq!(
            decide_action(Some(&m), true, 3600, 3600),
            LoadAction::Refresh
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

    // ─────────────────────────────────────────────────────────────────────
    // Filesystem integration tests (tempdir-scoped CacheDir)
    // ─────────────────────────────────────────────────────────────────────

    fn meta_for(file_key: &str, project_id: &str, last_modified: &str, now: u64) -> FileMeta {
        FileMeta {
            file_key: file_key.into(),
            name: "x".into(),
            project_id: project_id.into(),
            project_name: "P".into(),
            last_modified: last_modified.into(),
            cached_at_epoch: now,
            last_listed_at_epoch: now,
            status: EntryStatus::Ok,
            error: None,
            node_count: Some(1),
            bytes: Some(1),
            comments_schema_version: None,
            comments_fetched_at_epoch: None,
            comments_fingerprint: None,
            comments_error: None,
            full_fetched_at_epoch: None,
            full_bytes: None,
            full_schema_version: None,
            variables_fetched_at_epoch: None,
            variables_bytes: None,
            variables_error: None,
            variables_schema_version: None,
        }
    }

    #[test]
    fn write_and_read_meta_roundtrip() {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        let m = meta_for("abc", "10", "ts1", 42);
        cache.write_meta(&m).unwrap();
        let back = cache.read_meta("abc").unwrap().unwrap();
        assert_eq!(back.file_key, "abc");
        assert_eq!(back.project_id, "10");
        assert_eq!(back.last_modified, "ts1");
        assert_eq!(back.status, EntryStatus::Ok);
    }

    #[test]
    fn write_and_read_payload_roundtrip() {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        let payload = sample_cached_file();
        let bytes = cache.write_file("K", &payload).unwrap();
        assert!(bytes > 0);
        let read = cache.read_file("K").unwrap().unwrap();
        assert_eq!(read, payload);
    }

    #[test]
    fn delete_entry_removes_both_meta_and_payload() {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        let payload = sample_cached_file();
        cache.write_file("K", &payload).unwrap();
        cache.write_meta(&meta_for("K", "10", "ts", 42)).unwrap();
        assert!(cache.meta_path("K").exists());
        assert!(cache.file_path("K").exists());

        cache.delete_entry("K").unwrap();
        assert!(!cache.meta_path("K").exists());
        assert!(!cache.file_path("K").exists());
    }

    #[test]
    fn delete_entry_is_idempotent() {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        // Deleting nothing should not error.
        cache.delete_entry("never-existed").unwrap();
    }

    #[test]
    fn list_metas_returns_all_sidecars_skipping_payloads() {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        cache.write_meta(&meta_for("A", "10", "t", 1)).unwrap();
        cache.write_meta(&meta_for("B", "20", "t", 1)).unwrap();
        // Drop a stray rkyv file with no matching meta — list_metas must
        // ignore it (it's an orphan payload, not a meta).
        cache.write_file("C", &sample_cached_file()).unwrap();

        let mut metas = cache.list_metas().unwrap();
        metas.sort_by(|a, b| a.file_key.cmp(&b.file_key));
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].file_key, "A");
        assert_eq!(metas[1].file_key, "B");
    }

    #[test]
    fn list_metas_skips_malformed_json() {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        cache.write_meta(&meta_for("A", "10", "t", 1)).unwrap();
        // Drop a garbage .meta.json.
        fs::write(
            cache.files_dir().join("BROKEN.meta.json"),
            "not valid json {",
        )
        .unwrap();

        let metas = cache.list_metas().unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].file_key, "A");
    }

    #[test]
    fn default_dir_respects_env_override() {
        let prev = std::env::var("FIGMA_EXPLORER_CACHE_DIR").ok();
        std::env::set_var("FIGMA_EXPLORER_CACHE_DIR", "/tmp/figma-explorer-test-cache");
        assert_eq!(
            default_dir(),
            PathBuf::from("/tmp/figma-explorer-test-cache")
        );
        match prev {
            Some(v) => std::env::set_var("FIGMA_EXPLORER_CACHE_DIR", v),
            None => std::env::remove_var("FIGMA_EXPLORER_CACHE_DIR"),
        }
    }

    #[test]
    fn write_meta_is_atomic_no_tmp_left_behind() {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        cache.write_meta(&meta_for("K", "10", "t", 1)).unwrap();

        let entries: Vec<_> = fs::read_dir(cache.files_dir())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(entries.iter().any(|n| n == "K.meta.json"));
        assert!(
            !entries.iter().any(|n| n.contains(".tmp")),
            "found stray tempfile: {entries:?}"
        );
    }
}

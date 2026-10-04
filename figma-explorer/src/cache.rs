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

use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use figma_api::apis::configuration::Configuration;
use figma_api::apis::folders_api;
use figma_api::apis::projects_api;
use figma_api::models::Comment;
use figma_common::StableHasher;
use memmap2::Mmap;
use rkyv::rancor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::comment_assoc::{self, associate, AssociatedComment};
use crate::into_anyhow;
use crate::node::Bounds;

pub const DEFAULT_TTL_SECS: u64 = 3600;

/// How long a confirmed `version` stays trusted. Within this window
/// [`ensure_fresh`] serves the cache without a network call; past it, one
/// cheap `/v1/files/{key}/meta` probe decides between "unchanged" and
/// "refetch". Designers edit files live, so this is the staleness bound for
/// every command that reads cached file data.
pub const VERSION_CHECK_SECS: u64 = 300;

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

/// Schema version for the team-library catalog sidecar
/// (`teams/{team_id}.catalog.json.gz`). Bumped when `TeamCatalog`'s on-disk
/// shape changes; a mismatched sidecar is treated as missing → refetched.
pub const CATALOG_SCHEMA_VERSION: u32 = 1;

/// Refresh interval for the team-library catalog. Far longer than the
/// per-file [`DEFAULT_TTL_SECS`]: a design system changes slowly and a full
/// catalog refetch is several paginated requests. `library search`
/// auto-refetches a catalog older than this; `--refresh` overrides it.
pub const CATALOG_TTL_SECS: u64 = 86_400;

/// 4-byte magic prefix on every `.rkyv` cache file. Distinguishes a cache
/// file from arbitrary bytes (truncated downloads, accidental replacement).
pub const CACHE_MAGIC: [u8; 4] = *b"FXC\0";

/// Bump when `CachedFile` / `CacheNode` schema changes. A file with a
/// different version is treated as a cache miss and silently refetched.
///
/// v2 adds `CacheNode::characters` (truncated TEXT content) so `find` can
/// match user-visible copy, not just layer names. Existing v1 caches read as
/// a version mismatch → silent refetch on next access.
pub const CACHE_SCHEMA_VERSION: u32 = 2;

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
    /// First [`CHARACTERS_CAPTURE_MAX`] chars of a TEXT node's `characters`
    /// (its visible copy). `None` for non-text nodes and empty strings.
    /// Captured so `find` can match user-visible text, not just layer names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub characters: Option<String>,
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
///
/// This is the single untrusted-`Value` → `CacheNode` ingestion point, so it
/// enforces `MAX_NODE_DEPTH`. Every cached tree is therefore depth-bounded,
/// which is what keeps the downstream `CacheNode` walkers safe.
pub fn project_to_cache(node: &Value) -> CacheNode {
    project_rec(node, 0)
}

/// Cap on captured TEXT content. Enough for search plus a short display
/// snippet; the full text stays available via `node-info`'s raw-JSON sidecar.
const CHARACTERS_CAPTURE_MAX: usize = 160;

fn project_rec(node: &Value, depth: usize) -> CacheNode {
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
    // TEXT nodes carry their visible copy in `characters`; keep a truncated
    // prefix so `find` can match it. `chars().take()` is UTF-8-safe.
    let characters = node
        .get("characters")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(CHARACTERS_CAPTURE_MAX).collect::<String>());
    let children = if depth >= crate::MAX_NODE_DEPTH {
        eprintln!(
            "cache: node tree exceeded max depth {}; truncating children of {id}",
            crate::MAX_NODE_DEPTH
        );
        Vec::new()
    } else {
        node.get("children")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().map(|c| project_rec(c, depth + 1)).collect())
            .unwrap_or_default()
    };
    CacheNode {
        id,
        type_,
        name,
        visible,
        bounds,
        characters,
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

/// Count `node` plus all its descendants (visible and hidden). Unbounded
/// recursion is safe here: every `CacheNode` came through `project_to_cache`,
/// which caps depth at `MAX_NODE_DEPTH`.
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
    /// polled). Surfaced as the freshness header of `comments` output;
    /// `comments --refresh` re-fetches on demand.
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
    /// Last comments fetch *attempt*, successful or not. Comment activity
    /// doesn't change Figma's `version`, so [`ensure_fresh`] never notices it;
    /// [`ensure_comments_fresh`] re-fetches once this is older than
    /// [`VERSION_CHECK_SECS`]. Stamped on failure too, so an unreachable
    /// comments endpoint costs one attempt per window, not one per command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments_checked_at_epoch: Option<u64>,
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
    /// Figma's `version` for the document the payload was built from (the
    /// `version` field of `/v1/files/{key}`, which `/v1/files/{key}/meta`
    /// also reports). `None` on metas written before freshness checks
    /// existed — [`ensure_fresh`] treats that as "changed" and refetches once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Last time [`ensure_fresh`] confirmed `version` against Figma (or a
    /// fetch wrote it). Probes are skipped within [`VERSION_CHECK_SECS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_checked_at_epoch: Option<u64>,
    /// When [`ensure_fresh`] last failed to refetch this file after seeing a
    /// changed `version` (e.g. Figma took >30s to start sending a big file).
    /// Within [`VERSION_CHECK_SECS`] of it the file is neither re-probed nor
    /// refetched — each command just says it is serving an out-of-date copy —
    /// so a struggling file costs one timeout per window, not one per
    /// command. Cleared by any successful fetch (`from_success`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refetch_failed_at_epoch: Option<u64>,
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
            comments_checked_at_epoch: None,
            full_fetched_at_epoch: None,
            full_bytes: None,
            full_schema_version: None,
            variables_fetched_at_epoch: None,
            variables_bytes: None,
            variables_error: None,
            variables_schema_version: None,
            version: None,
            version_checked_at_epoch: None,
            refetch_failed_at_epoch: None,
        }
    }

    /// Marker meta for a file that failed to fetch/project (`NotExportable`
    /// or `Failed`). Records identity + status + error so subsequent loads
    /// don't keep retrying; all sidecar fields are left unset. Single source
    /// of truth so adding a new `FileMeta` field can't silently drift a
    /// hand-rolled failure literal.
    #[allow(clippy::too_many_arguments)]
    pub fn failure_marker(
        file_key: String,
        name: String,
        project_id: String,
        project_name: String,
        last_modified: String,
        status: EntryStatus,
        error: String,
        now: u64,
    ) -> Self {
        FileMeta {
            file_key,
            name,
            project_id,
            project_name,
            last_modified,
            cached_at_epoch: now,
            last_listed_at_epoch: now,
            status,
            error: Some(error),
            node_count: None,
            bytes: None,
            comments_fetched_at_epoch: None,
            comments_fingerprint: None,
            comments_error: None,
            comments_schema_version: None,
            comments_checked_at_epoch: None,
            full_fetched_at_epoch: None,
            full_bytes: None,
            full_schema_version: None,
            variables_fetched_at_epoch: None,
            variables_bytes: None,
            variables_error: None,
            variables_schema_version: None,
            version: None,
            version_checked_at_epoch: None,
            refetch_failed_at_epoch: None,
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
        fs::create_dir_all(self.teams_dir())
            .with_context(|| format!("creating {}", self.teams_dir().display()))?;
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
    /// them so `node-info` can run offline.
    pub fn full_path(&self, file_key: &str) -> PathBuf {
        self.files_dir().join(format!("{file_key}.full.json.gz"))
    }

    /// Path of the variables sidecar (raw `/v1/files/{key}/variables/local`
    /// body, plaintext JSON).
    pub fn variables_path(&self, file_key: &str) -> PathBuf {
        self.files_dir().join(format!("{file_key}.variables.json"))
    }

    /// Directory holding team-scoped sidecars. Parallel to `files/`: team
    /// data is keyed by `team_id`, not `file_key`, so it lives separately.
    pub fn teams_dir(&self) -> PathBuf {
        self.root.join("teams")
    }

    /// Per-folder listing stamps (`folders/{folder_id}.json`) — when each
    /// configured folder's file list was last checked against Figma. See
    /// [`sync_folders`].
    pub fn folders_dir(&self) -> PathBuf {
        self.root.join("folders")
    }

    fn folder_stamp_path(&self, folder_id: &str) -> PathBuf {
        self.folders_dir().join(format!("{folder_id}.json"))
    }

    /// When `folder_id`'s file list was last checked. Lenient: a missing or
    /// unreadable stamp reads as never, which only costs one listing request.
    pub fn folder_listed_at(&self, folder_id: &str) -> Option<u64> {
        let bytes = fs::read(self.folder_stamp_path(folder_id)).ok()?;
        serde_json::from_slice::<FolderStamp>(&bytes)
            .ok()
            .map(|s| s.listed_at_epoch)
    }

    pub fn stamp_folder_listed(&self, folder_id: &str, now: u64) -> Result<()> {
        let bytes = serde_json::to_vec(&FolderStamp {
            listed_at_epoch: now,
        })?;
        atomic_write(&self.folder_stamp_path(folder_id), &bytes)
    }

    /// Path of the gzipped team-library catalog sidecar — the published
    /// components, component sets, and styles across a team's libraries.
    pub fn catalog_path(&self, team_id: &str) -> PathBuf {
        self.teams_dir().join(format!("{team_id}.catalog.json.gz"))
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
    /// (to sweep orphans). Sorted by `(project_id, name, file_key)` —
    /// `read_dir` order is filesystem-dependent, and prefetch interns
    /// `file:N` synths in this order, so it must be deterministic.
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
        out.sort_by(|a, b| {
            (&a.project_id, &a.name, &a.file_key).cmp(&(&b.project_id, &b.name, &b.file_key))
        });
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
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
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

/// One row from the Figma folder listing — what `get_folder_files` returns,
/// in the flat shape our cache uses.
///
/// Figma renamed "projects" to "folders" in August 2026; the numeric ids are
/// unchanged, so `project_id` here is the same value as the folder id and the
/// on-disk meta field keeps its historical name.
#[derive(Debug, Clone)]
pub struct FileRef {
    pub file_key: String,
    pub name: String,
    pub last_modified: String,
    pub project_id: String,
    pub project_name: String,
}

/// Fetch every file across the given projects (Figma folders). One API call
/// per project; all projects are queried sequentially (small response, cheap
/// relative to file fetches).
///
/// Uses `GET /v2/folders/{id}/files`. Personal access tokens created after
/// 2026-08-03 carry `folders:read` instead of `projects:read`, and the
/// deprecated `GET /v1/projects/{id}/files` rejects them with 403. Tokens
/// that predate the rename are documented to keep working, but Figma does not
/// say whether they are accepted by the v2 endpoints — so a 403 from v2 falls
/// back to v1 once, and only a failure on both surfaces to the caller.
pub async fn list_project_files(
    cfg: &Configuration,
    project_ids: &[String],
) -> Result<Vec<FileRef>> {
    let mut out = Vec::new();
    for pid in project_ids {
        let (project_name, files) = list_folder_files(cfg, pid)
            .await
            .with_context(|| format!("listing files for project (folder) {pid}"))?;
        for (file_key, name, last_modified) in files {
            out.push(FileRef {
                file_key,
                name,
                last_modified,
                project_id: pid.clone(),
                project_name: project_name.clone(),
            });
        }
    }
    Ok(out)
}

/// `(folder_name, [(key, name, last_modified)])` for one folder — v2 first,
/// v1 on a 403 (see `list_project_files`).
async fn list_folder_files(
    cfg: &Configuration,
    folder_id: &str,
) -> Result<(String, Vec<(String, String, String)>)> {
    let v2 = folders_api::get_folder_files(
        cfg,
        folders_api::GetFolderFilesParams {
            folder_id: folder_id.to_owned(),
            branch_data: None,
        },
    )
    .await;
    let v2_err = match v2 {
        Ok(resp) => {
            let files = resp
                .files
                .into_iter()
                .map(|f| (f.key, f.name, f.last_modified))
                .collect();
            return Ok((resp.name, files));
        }
        Err(figma_api::apis::Error::ResponseError(r)) if r.status.as_u16() == 403 => {
            figma_api::apis::Error::ResponseError(r)
        }
        Err(e) => return Err(into_anyhow(e)),
    };

    // 403 on v2: possibly a pre-rename token that only holds `projects:read`.
    match projects_api::get_project_files(
        cfg,
        projects_api::GetProjectFilesParams {
            project_id: folder_id.to_owned(),
            branch_data: None,
        },
    )
    .await
    {
        Ok(resp) => {
            eprintln!(
                "cache: folder {folder_id}: v2 folders endpoint returned 403, deprecated v1 \
                 projects endpoint succeeded — this token predates Figma's projects→folders \
                 rename; regenerate it before v1 is removed"
            );
            let files = resp
                .files
                .into_iter()
                .map(|f| (f.key, f.name, f.last_modified))
                .collect();
            Ok((resp.name, files))
        }
        Err(v1_err) => Err(anyhow::anyhow!(
            "v2 folders endpoint: {:#}; deprecated v1 projects fallback: {:#}",
            into_anyhow(v2_err),
            into_anyhow(v1_err)
        )),
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
            if let Err(e) = cache.write_meta(&updated) {
                eprintln!("cache: write_meta failed for {file_key}: {e:#}");
            }
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
            if let Err(e) = cache.write_meta(&updated) {
                eprintln!("cache: write_meta failed for {file_key}: {e:#}");
            }
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
///
/// A failed fetch records a `Failed`/`NotExportable` marker (dropping any
/// cached payload) so cold loads don't hammer a broken file. Freshness
/// refetches deliberately do *not* go through here — see [`ensure_fresh`].
async fn fetch_and_cache(
    cfg: &Configuration,
    cache: &CacheDir,
    file_key: &str,
    file_ref: Option<&FileRef>,
    now: u64,
) -> Result<CachedFile> {
    match fetch_validated(cfg, file_key).await {
        Ok(file) => write_fetched(cfg, cache, file_key, file_ref, &file, now).await,
        Err(e) => record_fetch_failure(cache, file_key, file_ref, e, now),
    }
}

/// `GET /v1/files/{key}`, rejecting a 200 with no `document` (auth quirk,
/// partial body, schema drift) so it is never cached as an empty file.
async fn fetch_validated(cfg: &Configuration, file_key: &str) -> Result<Value> {
    let file = crate::cmd::fetch_file_json(cfg, file_key, None).await?;
    crate::cmd::require_document(&file, file_key)?;
    Ok(file)
}

/// Write everything derived from one validated `/v1/files/{key}` response:
/// the structural payload, the `.full.json.gz` sidecar, comments, and a meta
/// stamped with Figma's `version`. Interns the file (and comment) synths.
async fn write_fetched(
    cfg: &Configuration,
    cache: &CacheDir,
    file_key: &str,
    file_ref: Option<&FileRef>,
    file: &Value,
    now: u64,
) -> Result<CachedFile> {
    let doc = crate::cmd::require_document(file, file_key)?;
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
    // Read before the payload write: the variables and comments sidecars
    // aren't part of this response and stay on disk, so their stamps must
    // survive the new meta.
    let prev = cache.read_meta(file_key).ok().flatten();
    let payload = build_cached_file(&synthetic_ref, doc, now);
    let bytes = cache.write_file(file_key, &payload)?;
    let mut meta = FileMeta::from_success(&synthetic_ref, &payload, bytes, now);
    if let Some(p) = prev {
        // Likewise the comments sidecar: if this response's comments fetch
        // fails below, the old sidecar is still served, so its stamps must
        // still describe it.
        meta.comments_fetched_at_epoch = p.comments_fetched_at_epoch;
        meta.comments_fingerprint = p.comments_fingerprint;
        meta.comments_schema_version = p.comments_schema_version;
        meta.variables_fetched_at_epoch = p.variables_fetched_at_epoch;
        meta.variables_bytes = p.variables_bytes;
        meta.variables_error = p.variables_error;
        meta.variables_schema_version = p.variables_schema_version;
    }
    meta.version = crate::cmd::file_version(file);
    meta.version_checked_at_epoch = Some(now);
    // Write `node-info`'s full-JSON sidecar from this same response so it can
    // never describe a different version than the payload. On a failed write
    // the `full_*` stamps stay unset (`from_success`), and `node-info`'s
    // `load_full` treats an unstamped sidecar as stale.
    match crate::full_cache::write_full(cache, file_key, file) {
        Ok(n) => {
            meta.full_fetched_at_epoch = Some(now);
            meta.full_bytes = Some(n);
            meta.full_schema_version = Some(FULL_SCHEMA_VERSION);
        }
        Err(e) => eprintln!("cache: write_full failed for {file_key}: {e:#}"),
    }
    // Fetch comments alongside the structural payload — same cadence,
    // best-effort. A failure flips `meta.comments_error` but does not poison
    // the tree refresh. Comments are pre-associated against the just-written
    // tree.
    fetch_comments_into_meta(cfg, cache, file_key, now, &mut meta).await;
    // The variables sidecar describes the same document, so refresh it with
    // the payload — but only for files that have had one: everywhere else
    // the endpoint is a 403 (it's Enterprise-only), and probing it on every
    // refetch would be one wasted request each. A failure keeps the old
    // sidecar; `node-info` warns that it predates the payload.
    if meta.variables_fetched_at_epoch.is_some() && !variables_disabled_by_env() {
        if let Err(msg) = fetch_variables_into_meta(cfg, cache, file_key, now, &mut meta).await {
            eprintln!("cache: {file_key} variables: {msg}");
        }
    }
    cache.write_meta(&meta)?;
    // Intern synth IDs so downstream commands (`ls`, etc.) can render
    // qualified `file:N:x:y` / `file:N:comm:M` lines. File synth is assigned
    // (or retrieved) here; comment synths are interned immediately after
    // using that synth as their scope. Best-effort: a synth save failure logs
    // and continues.
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

/// Concurrent `/meta` probes in one [`ensure_fresh`] sweep. Each is ~1 KB.
const PROBE_CONCURRENCY: usize = 8;

/// Concurrent full-file refetches in one [`ensure_fresh`] sweep. Matches
/// `cache prefetch`'s default: `GET /v1/files/{key}` is the expensive,
/// tightly rate-limited endpoint.
const REFETCH_CONCURRENCY: usize = 3;

/// What [`ensure_fresh`] did. Files it skipped (checked within
/// [`VERSION_CHECK_SECS`], or not `Ok`) appear in neither list.
#[derive(Debug, Default)]
pub struct FreshReport {
    /// Files whose Figma `version` changed (or was never recorded) and were
    /// refetched successfully.
    pub refetched: Vec<String>,
    /// Files that could not be verified — the probe or the refetch failed —
    /// and are served from the existing cache.
    pub unverified: Vec<String>,
}

/// Whether `meta`'s cached content is due a version probe: `Ok` entries
/// whose last confirmation is missing or older than [`VERSION_CHECK_SECS`].
pub fn version_check_due(meta: &FileMeta, now: u64) -> bool {
    meta.status == EntryStatus::Ok
        && meta
            .version_checked_at_epoch
            .is_none_or(|t| now.saturating_sub(t) >= VERSION_CHECK_SECS)
}

/// Make sure the cached copies of `file_keys` match Figma's current version
/// before a command reads them. Designers edit files live, so this — not
/// `cache prefetch` — is what keeps day-to-day reads current.
///
/// 1. Skip files confirmed within [`VERSION_CHECK_SECS`] (the common case:
///    no network at all) and files without an `Ok` entry.
/// 2. Probe the rest via `/v1/files/{key}/meta` (~1 KB each, concurrently).
///    An unchanged version just restamps `version_checked_at_epoch`.
/// 3. Refetch files whose version changed — or was never recorded — through
///    [`write_fetched`], so payload, full sidecar, and meta stay in lockstep.
///
/// Never makes things worse than serving stale: a failed probe or refetch
/// leaves the existing entry untouched (unlike [`fetch_and_cache`], which
/// records a failure marker) and is reported on stderr and in
/// [`FreshReport::unverified`]. The next command past the window retries.
pub async fn ensure_fresh(
    cfg: &Configuration,
    cache: &CacheDir,
    file_keys: &[String],
) -> FreshReport {
    use futures::stream::{self, StreamExt};

    let mut report = FreshReport::default();
    let now = now_epoch();
    let (backing_off, due): (Vec<FileMeta>, Vec<FileMeta>) = file_keys
        .iter()
        .filter_map(|k| cache.read_meta(k).ok().flatten())
        .filter(|m| version_check_due(m, now))
        .partition(|m| {
            m.refetch_failed_at_epoch
                .is_some_and(|t| now.saturating_sub(t) < VERSION_CHECK_SECS)
        });
    for m in backing_off {
        let ago = crate::cmd::cache::age(now, m.refetch_failed_at_epoch.unwrap_or(now));
        eprintln!(
            "cache: {} changed on Figma but its refetch failed {ago} ago — serving the cached copy, which is out of date (retrying within {}m)",
            m.name,
            VERSION_CHECK_SECS / 60
        );
        report.unverified.push(m.file_key);
    }
    if due.is_empty() {
        return report;
    }

    let probes: Vec<(FileMeta, Result<String>)> = stream::iter(due.into_iter().map(|m| async {
        let r = crate::cmd::fetch_file_version(cfg, &m.file_key).await;
        (m, r)
    }))
    .buffer_unordered(PROBE_CONCURRENCY)
    .collect()
    .await;

    let mut changed = Vec::new();
    let mut probe_errors = Vec::new();
    let restamp = |m: &mut FileMeta| {
        m.version_checked_at_epoch = Some(now);
        if let Err(e) = cache.write_meta(m) {
            eprintln!("cache: write_meta failed for {}: {e:#}", m.file_key);
        }
    };
    for (mut m, r) in probes {
        match r {
            Ok(v) if m.version.as_deref() == Some(v.as_str()) => restamp(&mut m),
            Ok(_) => changed.push(m),
            Err(e) => {
                // A 403/404 won't heal by retrying on the next command (file
                // unshared or deleted, or cached under another account's
                // token): back off for a full window instead of re-probing on
                // every sweep. Transient failures (network, 5xx) retry.
                if is_access_error(&format!("{e:#}")) {
                    restamp(&mut m);
                }
                probe_errors.push((m, e));
            }
        }
    }
    if let Some((m, e)) = probe_errors.first() {
        eprintln!(
            "cache: couldn't check {} against Figma ({}: {e:#}) — serving cached copies, which may be stale",
            count_files(probe_errors.len()),
            m.name,
        );
    }
    report
        .unverified
        .extend(probe_errors.into_iter().map(|(m, _)| m.file_key));

    if changed.is_empty() {
        return report;
    }
    eprintln!(
        "cache: {} changed on Figma ({}) — refetching…",
        count_files(changed.len()),
        name_list(changed.iter().map(|m| m.name.as_str()), 5)
    );
    let refetches: Vec<(FileMeta, Result<()>)> = stream::iter(changed.into_iter().map(|m| async {
        let r = refetch_known(cfg, cache, &m).await.map(|_| ());
        (m, r)
    }))
    .buffer_unordered(REFETCH_CONCURRENCY)
    .collect()
    .await;

    for (m, r) in refetches {
        match r {
            Ok(()) => report.refetched.push(m.file_key),
            Err(e) => {
                eprintln!(
                    "cache: refetch of {} failed ({e:#}) — serving the cached copy, which is out of date",
                    m.name
                );
                // Back off: the next commands in this window warn without
                // re-probing or waiting on another timeout.
                let mut m = m;
                m.refetch_failed_at_epoch = Some(now_epoch());
                if let Err(we) = cache.write_meta(&m) {
                    eprintln!("cache: write_meta failed for {}: {we:#}", m.file_key);
                }
                report.unverified.push(m.file_key);
            }
        }
    }
    report
}

/// `--cache-only` counterpart of [`ensure_fresh`]: no probe, but tell the
/// user when data being served hasn't been checked against Figma within
/// [`VERSION_CHECK_SECS`]. One stderr line however many files are involved.
pub fn note_unverified_cache_only<'a>(metas: impl IntoIterator<Item = &'a FileMeta>) {
    let now = now_epoch();
    let last_checked = |m: &FileMeta| m.version_checked_at_epoch.unwrap_or(m.cached_at_epoch);
    let due: Vec<&FileMeta> = metas
        .into_iter()
        .filter(|m| version_check_due(m, now))
        .collect();
    let Some(oldest) = due.iter().min_by_key(|m| last_checked(m)) else {
        return;
    };
    let ago = crate::cmd::cache::age(now, last_checked(oldest));
    if due.len() == 1 {
        eprintln!(
            "cache: {} last checked against Figma {ago} ago (--cache-only) — may be stale",
            oldest.name
        );
    } else {
        eprintln!(
            "cache: {} not checked against Figma in the last {}m (oldest: {}, {ago} ago; --cache-only) — may be stale",
            count_files(due.len()),
            VERSION_CHECK_SECS / 60,
            oldest.name
        );
    }
}

#[derive(Serialize, Deserialize)]
struct FolderStamp {
    listed_at_epoch: u64,
}

/// The folders this process may list and prune: `FIGMA_PROJECTS_IDS`.
/// Folders another repo's cache entries belong to are shown but never
/// synced — the same jurisdiction rule `cache prefetch` follows.
pub fn configured_folder_ids() -> Vec<String> {
    parse_project_ids_env()
}

/// What [`sync_folders`] changed, by file name.
#[derive(Debug, Default)]
pub struct FolderSyncReport {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// Files whose name or folder changed (meta updated in place).
    pub updated: Vec<String>,
}

/// Keep the cached file *list* of `folder_ids` in step with Figma.
/// [`ensure_fresh`] keeps each cached file current, but only for files the
/// cache already knows: a file added to a folder never appeared, and a
/// deleted one lingered, until the next `cache prefetch`.
///
/// `folder_ids` are synced as one set: once any of them is past
/// [`VERSION_CHECK_SECS`], all are listed (one request each). Listing them
/// together is what makes "not in any listing" mean "gone": a file moved
/// from one configured folder to another is still listed, just elsewhere.
/// Then, for files in the listings:
/// - not cached yet (or a previous fetch `Failed`) → fetched with bounded
///   concurrency, but only in folders that already have cached files — an
///   empty cache is `cache prefetch`'s job;
/// - cached under one of these folders but in no listing → entry deleted,
///   only when every listing succeeded (a failed one could hold the file);
/// - renamed or moved between folders → meta name/folder updated in place,
///   interning the destination folder's `proj:N`.
///
/// Changed `last_modified` is deliberately ignored — the version check owns
/// content freshness. Every folder is stamped whether its listing succeeded
/// or not, so a broken folder costs one request per window; the caller keeps
/// serving what is cached.
pub async fn sync_folders(
    cfg: &Configuration,
    cache: &CacheDir,
    folder_ids: &[String],
) -> FolderSyncReport {
    use futures::stream::{self, StreamExt};
    use std::collections::{HashMap, HashSet};

    let mut report = FolderSyncReport::default();
    let now = now_epoch();
    let any_due = folder_ids.iter().any(|id| {
        cache
            .folder_listed_at(id)
            .is_none_or(|t| now.saturating_sub(t) >= VERSION_CHECK_SECS)
    });
    if !any_due {
        return report;
    }

    let mut synced: HashSet<String> = HashSet::new();
    let mut listed: Vec<FileRef> = Vec::new();
    // Sequential: a handful of configured folders, ~1 small request each.
    for id in folder_ids {
        match list_project_files(cfg, std::slice::from_ref(id)).await {
            Ok(refs) => {
                synced.insert(id.clone());
                listed.extend(refs);
            }
            Err(e) => eprintln!(
                "cache: couldn't list folder {id} ({e:#}) — showing its cached files, which may be out of date"
            ),
        }
        if let Err(e) = cache.stamp_folder_listed(id, now) {
            eprintln!("cache: stamping folder {id} failed: {e:#}");
        }
    }

    let all_listed = synced.len() == folder_ids.len();

    let by_key: HashMap<&str, &FileRef> = listed.iter().map(|r| (r.file_key.as_str(), r)).collect();
    let metas = cache.list_metas().unwrap_or_default();
    // A `Failed` marker (say, a 429 on the first fetch) isn't a cached file:
    // leave it out so the file is retried — at most once per window, since
    // that's how often this runs.
    let known: HashSet<&str> = metas
        .iter()
        .filter(|m| m.status != EntryStatus::Failed)
        .map(|m| m.file_key.as_str())
        .collect();
    let mut moved_to: HashSet<String> = HashSet::new();
    for m in &metas {
        match by_key.get(m.file_key.as_str()) {
            Some(r)
                if m.name != r.name
                    || m.project_id != r.project_id
                    || m.project_name != r.project_name =>
            {
                let mut updated = m.clone();
                updated.name = r.name.clone();
                updated.project_id = r.project_id.clone();
                updated.project_name = r.project_name.clone();
                if updated.project_id != m.project_id {
                    moved_to.insert(updated.project_id.clone());
                }
                match cache.write_meta(&updated) {
                    Ok(()) => report.updated.push(r.name.clone()),
                    Err(e) => eprintln!("cache: write_meta failed for {}: {e:#}", m.file_key),
                }
            }
            Some(_) => {}
            None if all_listed && synced.contains(&m.project_id) => {
                match cache.delete_entry(&m.file_key) {
                    Ok(()) => report.removed.push(m.name.clone()),
                    Err(e) => eprintln!("cache: removing {} failed: {e:#}", m.file_key),
                }
            }
            None => {}
        }
    }

    // Root `ls` groups files under interned projects; a file moved into a
    // folder with no `proj:N` yet would otherwise drop out of every listing.
    if !moved_to.is_empty() {
        if let Err(e) = crate::synth::with_lock(cache, |s| {
            for p in &moved_to {
                s.intern_project(p);
            }
        }) {
            eprintln!("cache: synth intern failed: {e:#}");
        }
    }

    // Only folders the cache already holds files for: syncing keeps a
    // populated cache current, it doesn't populate one. Otherwise the first
    // `ls` on a fresh (or just-cleared) cache would quietly become a full
    // `cache prefetch`.
    let populated: HashSet<&str> = metas
        .iter()
        .filter(|m| m.status == EntryStatus::Ok)
        .map(|m| m.project_id.as_str())
        .collect();
    let new: Vec<&FileRef> = listed
        .iter()
        .filter(|r| {
            !known.contains(r.file_key.as_str()) && populated.contains(r.project_id.as_str())
        })
        .collect();
    if !report.removed.is_empty() {
        eprintln!(
            "cache: {} removed from Figma folders ({}) — dropped from the cache",
            count_files(report.removed.len()),
            name_list(report.removed.iter().map(String::as_str), 5)
        );
    }
    if new.is_empty() {
        return report;
    }
    eprintln!(
        "cache: {} added to Figma folders ({}) — fetching…",
        count_files(new.len()),
        name_list(new.iter().map(|r| r.name.as_str()), 5)
    );
    let fetched: Vec<(&FileRef, Result<CachedFile>)> =
        stream::iter(new.into_iter().map(|r| async move {
            (
                r,
                fetch_and_cache(cfg, cache, &r.file_key, Some(r), now_epoch()).await,
            )
        }))
        .buffer_unordered(REFETCH_CONCURRENCY)
        .collect()
        .await;
    for (r, res) in fetched {
        match res {
            Ok(_) => report.added.push(r.name.clone()),
            Err(e) => eprintln!("cache: fetching new file {} failed: {e:#}", r.name),
        }
    }
    report
}

/// Refetch a file that already has a meta, keeping its project context.
/// Unlike [`fetch_and_cache`], a failure leaves the existing entry untouched.
/// Returns the raw `/v1/files/{key}` body (the caller may want fields the
/// payload drops — `node-info` does).
async fn refetch_known(cfg: &Configuration, cache: &CacheDir, meta: &FileMeta) -> Result<Value> {
    let file = fetch_validated(cfg, &meta.file_key).await?;
    write_fetched(
        cfg,
        cache,
        &meta.file_key,
        Some(&file_ref_of(meta)),
        &file,
        now_epoch(),
    )
    .await?;
    Ok(file)
}

/// The project context a refetch keeps: identity from the existing meta.
fn file_ref_of(meta: &FileMeta) -> FileRef {
    FileRef {
        file_key: meta.file_key.clone(),
        name: meta.name.clone(),
        last_modified: meta.last_modified.clone(),
        project_id: meta.project_id.clone(),
        project_name: meta.project_name.clone(),
    }
}

/// Refetch `file_key` and rewrite its payload, full sidecar, and meta from one
/// response, returning the raw body. With no meta on disk this is a cold
/// fetch (failure marker on error, like [`load_file`]); otherwise a failure
/// leaves the cached entry as it was. The caller wants the body itself, so a
/// cache write failure (full disk, read-only cache dir) is logged and the
/// body is still returned.
pub async fn refetch_file(cfg: &Configuration, cache: &CacheDir, file_key: &str) -> Result<Value> {
    let meta = cache.read_meta(file_key).ok().flatten();
    let file = match fetch_validated(cfg, file_key).await {
        Ok(f) => f,
        Err(e) if meta.is_none() => {
            return record_fetch_failure(cache, file_key, None, e, now_epoch()).map(|_| Value::Null)
        }
        Err(e) => return Err(e),
    };
    let file_ref = meta.as_ref().map(file_ref_of);
    if let Err(e) = write_fetched(cfg, cache, file_key, file_ref.as_ref(), &file, now_epoch()).await
    {
        eprintln!("cache: couldn't write refetched {file_key} to the cache: {e:#}");
    }
    Ok(file)
}

/// Whether the `.full.json.gz` sidecar described by `meta` was written from
/// the same (or a newer) response as the structural payload, in the current
/// format. Every fetch path writes both together; an unstamped or older
/// sidecar is one a refetch failed to replace.
pub fn full_sidecar_current(meta: &FileMeta) -> bool {
    meta.full_schema_version == Some(FULL_SCHEMA_VERSION)
        && meta
            .full_fetched_at_epoch
            .is_some_and(|t| t >= meta.cached_at_epoch)
}

/// `a, b, c` — or `a, b, … +N more` past `max`, so the one-time migration
/// refetch of a whole cache doesn't print every file name.
fn name_list<'a>(names: impl ExactSizeIterator<Item = &'a str>, max: usize) -> String {
    let total = names.len();
    let shown: Vec<&str> = names.take(max).collect();
    let more = total - shown.len();
    let mut s = shown.join(", ");
    if more > 0 {
        s.push_str(&format!(", … +{more} more"));
    }
    s
}

/// Does this probe error mean "no access" rather than "try again"? Matches
/// `figma_common::get_text`'s `figma API error (403 Forbidden): …` shape.
fn is_access_error(msg: &str) -> bool {
    msg.contains("(403 ") || msg.contains("(404 ")
}

fn count_files(n: usize) -> String {
    if n == 1 {
        "1 file".to_owned()
    } else {
        format!("{n} files")
    }
}

/// Persist a failure/`NotExportable` marker meta for a file we could not fetch
/// (network error) or whose response was malformed (missing `document`), so
/// subsequent loads don't keep retrying on every call. Drops any stale payload
/// first (meta-first ordering) and returns the original error to propagate.
fn record_fetch_failure(
    cache: &CacheDir,
    file_key: &str,
    file_ref: Option<&FileRef>,
    e: anyhow::Error,
    now: u64,
) -> Result<CachedFile> {
    let msg = format!("{e:#}");
    let status = if is_not_exportable_error(&msg) {
        EntryStatus::NotExportable
    } else {
        EntryStatus::Failed
    };
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
    let marker = FileMeta::failure_marker(
        file_key.to_owned(),
        name,
        project_id,
        project_name,
        last_modified,
        status,
        msg.clone(),
        now,
    );
    let _ = cache.delete_entry(file_key);
    if let Err(we) = cache.write_meta(&marker) {
        eprintln!("cache: write_meta failed for {file_key}: {we:#}");
    }
    Err(e)
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
    let mut h = StableHasher::default();
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
    meta.comments_checked_at_epoch = Some(now);
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

/// Single-file comment refresh: fetch + associate + write sidecar + stamp
/// meta + intern comm synths — the one public entry point bundling what the
/// prefetch/cold-load call sites of [`fetch_comments_into_meta`] do by hand.
/// Used by `comments --refresh` (and its missing-sidecar cold path) so a
/// single file's comments can be refreshed without a full `cache prefetch`.
///
/// Requires the file to already be cached (meta + tree on disk — anchoring
/// reads the tree); errors otherwise with the standard remedy hint. Unlike
/// `fetch_comments_into_meta` this is *not* best-effort: a fetch/parse/write
/// failure is returned as an error (after persisting it on the meta).
pub async fn refresh_file_comments(
    cfg: &Configuration,
    cache: &CacheDir,
    file_key: &str,
    file_synth: u32,
) -> Result<(Vec<AssociatedComment>, FileMeta)> {
    let mut meta = cache.read_meta(file_key)?.ok_or_else(|| {
        anyhow::anyhow!(
            "file {file_key} is not cached; run `figma-explorer cache prefetch` or pass the file's Figma URL first"
        )
    })?;
    let now = now_epoch();
    fetch_comments_into_meta(cfg, cache, file_key, now, &mut meta).await;
    cache.write_meta(&meta)?;
    if let Some(err) = &meta.comments_error {
        anyhow::bail!("refreshing comments for {file_key}: {err}");
    }
    let comments = cache.read_comments(file_key)?.unwrap_or_default();
    intern_comment_synths(cache, file_synth, &comments);
    Ok((comments, meta))
}

/// `FIGMA_EXPLORER_FETCH_VARIABLES=0` turns off every variables fetch —
/// `cache prefetch` and the refetch in [`write_fetched`] alike. Any other
/// value (or none) leaves them on.
pub fn variables_disabled_by_env() -> bool {
    std::env::var("FIGMA_EXPLORER_FETCH_VARIABLES").is_ok_and(|s| s.trim() == "0")
}

/// Fetch `file_key`'s local variables and write the `.variables.json`
/// sidecar. On success stamps `meta` and clears `variables_error`; on failure
/// leaves any prior sidecar (and its stamps) untouched, records the error on
/// `meta`, and returns it for the caller to report (`cache prefetch` counts
/// 403s rather than printing each).
pub async fn fetch_variables_into_meta(
    cfg: &Configuration,
    cache: &CacheDir,
    file_key: &str,
    now: u64,
    meta: &mut FileMeta,
) -> std::result::Result<(), String> {
    let written = match crate::cmd::fetch_local_variables(cfg, file_key).await {
        Ok(vars) => crate::full_cache::write_variables(cache, file_key, &vars)
            .map_err(|e| format!("write_variables: {e:#}")),
        Err(e) => Err(format!("{e:#}")),
    };
    match written {
        Ok(n) => {
            meta.variables_fetched_at_epoch = Some(now);
            meta.variables_bytes = Some(n);
            meta.variables_schema_version = Some(VARIABLES_SCHEMA_VERSION);
            meta.variables_error = None;
            Ok(())
        }
        Err(msg) => {
            meta.variables_error = Some(msg.clone());
            Err(msg)
        }
    }
}

/// Whether the variables sidecar was written before the current payload —
/// a refetch brought in a newer document but couldn't refresh variables.
pub fn variables_predate_payload(meta: &FileMeta) -> bool {
    meta.variables_fetched_at_epoch
        .is_some_and(|t| t < meta.cached_at_epoch)
}

/// Whether `meta`'s comments are due a re-fetch: never attempted, or last
/// attempted more than [`VERSION_CHECK_SECS`] ago. Metas written before
/// `comments_checked_at_epoch` existed fall back to the last success.
pub fn comments_check_due(meta: &FileMeta, now: u64) -> bool {
    meta.comments_checked_at_epoch
        .or(meta.comments_fetched_at_epoch)
        .is_none_or(|t| now.saturating_sub(t) >= VERSION_CHECK_SECS)
}

/// Keep a file's comments current before a command reads them. Comment
/// activity doesn't change Figma's `version`, so [`ensure_fresh`] can't see
/// it; this gives comments their own clock on the same window.
///
/// Best-effort, like [`ensure_fresh`]: within the window it does nothing; past
/// it, it re-fetches once, and a failure keeps the existing sidecar and says
/// on stderr that it is serving an older copy. Returns whether the sidecar was
/// rewritten, so a caller holding comments or a meta read earlier reloads.
pub async fn ensure_comments_fresh(
    cfg: &Configuration,
    cache: &CacheDir,
    file_key: &str,
    file_synth: u32,
) -> bool {
    let Ok(Some(mut meta)) = cache.read_meta(file_key) else {
        return false;
    };
    let now = now_epoch();
    if meta.status != EntryStatus::Ok || !comments_check_due(&meta, now) {
        return false;
    }
    let before = meta.comments_fetched_at_epoch;
    fetch_comments_into_meta(cfg, cache, file_key, now, &mut meta).await;
    if let Err(e) = cache.write_meta(&meta) {
        eprintln!("cache: write_meta failed for {file_key}: {e:#}");
    }
    if meta.comments_error.is_some() {
        let ago = before
            .map(|t| format!("fetched {} ago", crate::cmd::cache::age(now, t)))
            .unwrap_or_else(|| "never fetched".to_owned());
        eprintln!(
            "comments: serving the cached comments for {} ({ago}); retrying within {}m",
            meta.name,
            VERSION_CHECK_SECS / 60
        );
        return false;
    }
    if let Ok(Some(comments)) = cache.read_comments(file_key) {
        intern_comment_synths(cache, file_synth, &comments);
    }
    true
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
/// The caller supplies the cache dir (the resolver injects its own, so tests
/// stay hermetic in tempdirs).
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
pub async fn load_file(
    cfg: &Configuration,
    cache: &CacheDir,
    file_key: &str,
) -> Result<(CachedFile, u32)> {
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
    let payload = match decide_action(meta.as_ref(), payload_exists, now, DEFAULT_TTL_SECS) {
        LoadAction::UseCache => cache
            .read_file(file_key)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .ok_or_else(|| anyhow::anyhow!("cache: meta says Ok but payload vanished mid-read"))?,
        LoadAction::NotExportableCached => {
            let err = meta
                .as_ref()
                .and_then(|m| m.error.clone())
                .unwrap_or_else(|| "file marked not exportable".to_owned());
            anyhow::bail!("{err}");
        }
        LoadAction::Refresh => {
            // Prefer a single-project listing when we have a project hint that
            // matches the user's env — cheaper than a blind refetch and lets
            // us preserve the cache entry when `last_modified` is unchanged.
            let env_projects = parse_project_ids_env();
            let project_hint = meta.as_ref().map(|m| m.project_id.as_str()).unwrap_or("");
            let mut refreshed: Option<CachedFile> = None;
            if !project_hint.is_empty() && env_projects.iter().any(|p| p == project_hint) {
                match try_refresh_single(cfg, cache, file_key, project_hint, meta.as_ref(), now)
                    .await
                {
                    Ok(Some(p)) => refreshed = Some(p),
                    Ok(None) => {
                        // No decision possible. Serve stale if we have an Ok payload.
                        if let Some(m) = &meta {
                            if m.status == EntryStatus::Ok {
                                if let Ok(Some(v)) = cache.read_file(file_key) {
                                    refreshed = Some(v);
                                }
                            }
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
            match refreshed {
                Some(p) => p,
                // Final fallback: blind live fetch (cold load, or refresh fell through).
                None => fetch_and_cache(cfg, cache, file_key, None, now).await?,
            }
        }
    };

    // Guarantee the caller gets a `file_synth` for the file_key in hand —
    // `intern_file` is idempotent, so the UseCache path is essentially free
    // and the refresh path's prior intern (inside `fetch_and_cache`) collapses
    // to a no-op. Loading a file without a usable synth would force the only
    // production caller (`resolver::resolve_url`) to reload synth.json from
    // disk just to learn the value we already had in our `with_lock` window.
    let file_synth = crate::synth::with_lock(cache, |s| s.intern_file(file_key))
        .with_context(|| format!("interning file synth for {file_key}"))?;
    Ok((payload, file_synth))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn teams_dir_and_catalog_path_layout() {
        let cache = CacheDir::new("/tmp/fx-cache");
        assert_eq!(cache.teams_dir(), PathBuf::from("/tmp/fx-cache/teams"));
        assert_eq!(
            cache.catalog_path("651911646771145269"),
            PathBuf::from("/tmp/fx-cache/teams/651911646771145269.catalog.json.gz"),
        );
    }

    #[test]
    fn project_to_cache_caps_depth_on_deep_tree() {
        // Run on a large-stack thread: the input Value is deeper than the cap,
        // and serde_json's `Value` has a recursive `Drop`, so tearing it down
        // would overflow the default ~2MB test-thread stack regardless of our
        // guard. The guard is what bounds `project_to_cache`'s own recursion.
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let mut node = json!({ "id": "leaf", "type": "FRAME", "name": "leaf" });
                for i in 0..(crate::MAX_NODE_DEPTH + 50) {
                    node = json!({ "id": format!("n{i}"), "type": "FRAME", "name": "n", "children": [node] });
                }
                let projected = project_to_cache(&node);

                // Measure the (linear) chain depth iteratively.
                let mut depth = 1usize;
                let mut cur = &projected;
                while let Some(child) = cur.children.first() {
                    depth += 1;
                    cur = child;
                }
                // Root at depth 0 … node at depth MAX_NODE_DEPTH keeps no
                // children, so the chain is exactly MAX_NODE_DEPTH + 1 nodes.
                assert_eq!(depth, crate::MAX_NODE_DEPTH + 1);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn project_keeps_structural_fields_and_characters_drops_paint() {
        let raw = json!({
            "id": "1:2",
            "name": "Hero",
            "type": "FRAME",
            "absoluteBoundingBox": { "x": 0, "y": 0, "width": 1440, "height": 800 },
            "fills": [{"type": "SOLID", "color": {"r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0}}],
            "strokes": [],
            "effects": [],
            "characters": "framecopy",
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
        // `characters` is now captured (paint fields still dropped).
        assert_eq!(s.characters.as_deref(), Some("framecopy"));
        assert_eq!(s.children.len(), 2);
        assert_eq!(s.children[0].name, "Title");
        assert_eq!(s.children[0].characters.as_deref(), Some("Hi"));
        assert!(s.children[0].visible);
        assert!(!s.children[1].visible);
    }

    #[test]
    fn project_captures_characters_truncated_utf8_safe() {
        // 300 multi-byte chars → capped at CHARACTERS_CAPTURE_MAX, no split.
        let long: String = "é".repeat(300);
        let raw = json!({ "id": "1:1", "name": "T", "type": "TEXT", "characters": long });
        let s = project_to_cache(&raw);
        let captured = s.characters.expect("captured");
        assert_eq!(captured.chars().count(), CHARACTERS_CAPTURE_MAX);
        // Empty characters → None, not Some("").
        let empty = json!({ "id": "1:2", "name": "T", "type": "TEXT", "characters": "" });
        assert_eq!(project_to_cache(&empty).characters, None);
        // No characters field → None.
        let none = json!({ "id": "1:3", "name": "F", "type": "FRAME" });
        assert_eq!(project_to_cache(&none).characters, None);
    }

    #[test]
    fn count_nodes_includes_self_plus_descendants() {
        let n = CacheNode {
            id: "a".into(),
            type_: String::new(),
            name: String::new(),
            visible: true,
            bounds: None,
            characters: None,
            children: vec![
                CacheNode {
                    id: "b".into(),
                    type_: String::new(),
                    name: String::new(),
                    visible: true,
                    bounds: None,
                    characters: None,
                    children: vec![],
                },
                CacheNode {
                    id: "c".into(),
                    type_: String::new(),
                    name: String::new(),
                    visible: true,
                    bounds: None,
                    characters: None,
                    children: vec![CacheNode {
                        id: "d".into(),
                        type_: String::new(),
                        name: String::new(),
                        visible: true,
                        bounds: None,
                        characters: None,
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
            comments_checked_at_epoch: None,
            full_fetched_at_epoch: None,
            full_bytes: None,
            full_schema_version: None,
            variables_fetched_at_epoch: None,
            variables_bytes: None,
            variables_error: None,
            variables_schema_version: None,
            version: None,
            version_checked_at_epoch: None,
            refetch_failed_at_epoch: None,
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
            characters: None,
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
                characters: None,
                children: vec![CacheNode {
                    id: "1:0".into(),
                    type_: "CANVAS".into(),
                    name: "Page".into(),
                    visible: true,
                    bounds: None,
                    characters: None,
                    children: vec![{
                        // Exercise the new `characters` field through rkyv.
                        let mut t = leaf("1:1", "Title", "TEXT");
                        t.characters = Some("Leave details".into());
                        t
                    }],
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
            comments_checked_at_epoch: None,
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
            version: None,
            version_checked_at_epoch: None,
            refetch_failed_at_epoch: None,
        }
    }

    #[test]
    fn list_metas_is_sorted_by_project_name_key() {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        let mut b = meta_for("zz", "10", "ts", 1);
        b.name = "Alpha".into();
        let mut a = meta_for("yy", "10", "ts", 1);
        a.name = "Alpha".into();
        let mut c = meta_for("aa", "10", "ts", 1);
        c.name = "Beta".into();
        let d = meta_for("bb", "09", "ts", 1);
        for m in [&c, &b, &d, &a] {
            cache.write_meta(m).unwrap();
        }
        let keys: Vec<String> = cache
            .list_metas()
            .unwrap()
            .into_iter()
            .map(|m| m.file_key)
            .collect();
        assert_eq!(keys, ["bb", "yy", "zz", "aa"]);
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

    /// Minimal one-shot HTTP/1.1 server on a std thread: serves the queued
    /// `(status, body)` responses in order and records each request path.
    /// Avoids a mock-server dev-dependency and any tokio `net` feature.
    use crate::test_http::{self as mock_http, cfg_for};

    /// Recorded 2026-09-05 from `GET /v2/folders/{id}/files` (values
    /// anonymised, shape verbatim). Identical to the v1 project-files shape
    /// apart from the optional `branches` array.
    const V2_FOLDER_FILES: &str = r#"{"name":"Desktop","files":[
        {"key":"aaaaaaaaaaaaaaaaaaaaaa","name":"404 / No Access",
         "thumbnail_url":"https://s3-alpha.figma.com/thumbnails/a","last_modified":"2026-03-25T17:07:04Z"},
        {"key":"bbbbbbbbbbbbbbbbbbbbbb","name":"Wall Chart",
         "thumbnail_url":"https://s3-alpha.figma.com/thumbnails/b","last_modified":"2026-07-15T09:57:35Z",
         "branches":[{"key":"cccccccccccccccccccccc","name":"experiment","last_modified":"2026-07-16T08:00:00Z"}]}
    ]}"#;

    /// Verbatim 403 body a post-2026-08-03 token gets from the v1 endpoint.
    const V1_SCOPE_403: &str = r#"{"error":true,"status":403,"message":"Invalid scope: [\"folders:read\"]. This endpoint requires the file_read or files:read or projects:read scope."}"#;

    /// The listing must go to `/v2/folders/{id}/files` (the only endpoint a
    /// post-rename token can call) and map the response onto `FileRef`
    /// unchanged — including `project_id` = the folder id, so existing metas
    /// and `FIGMA_PROJECTS_IDS` values keep matching.
    #[tokio::test]
    async fn list_project_files_uses_v2_folders_endpoint() {
        let server = mock_http::serve(vec![(200, V2_FOLDER_FILES.into())]);
        let cfg = cfg_for(&server);

        let refs = list_project_files(&cfg, &["77195660".to_owned()])
            .await
            .unwrap();

        assert_eq!(server.paths(), vec!["/v2/folders/77195660/files"]);
        assert_eq!(refs.len(), 2, "branches must not be flattened into files");
        assert_eq!(refs[0].file_key, "aaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(refs[0].name, "404 / No Access");
        assert_eq!(refs[0].last_modified, "2026-03-25T17:07:04Z");
        assert_eq!(refs[0].project_id, "77195660");
        assert_eq!(refs[0].project_name, "Desktop");
        assert_eq!(refs[1].file_key, "bbbbbbbbbbbbbbbbbbbbbb");
    }

    /// A 403 from v2 retries the deprecated v1 endpoint once (pre-rename
    /// tokens are documented to keep working there); success on v1 is a
    /// success for the caller.
    #[tokio::test]
    async fn list_project_files_falls_back_to_v1_on_403() {
        let v1_body = r#"{"name":"Desktop","files":[{"key":"aaaaaaaaaaaaaaaaaaaaaa","name":"A","last_modified":"2026-03-25T17:07:04Z"}]}"#;
        let server = mock_http::serve(vec![
            (
                403,
                r#"{"error":true,"status":403,"message":"Invalid scope"}"#.into(),
            ),
            (200, v1_body.into()),
        ]);
        let cfg = cfg_for(&server);

        let refs = list_project_files(&cfg, &["77195660".to_owned()])
            .await
            .unwrap();

        assert_eq!(
            server.paths(),
            vec!["/v2/folders/77195660/files", "/v1/projects/77195660/files"]
        );
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].project_name, "Desktop");
    }

    /// Both endpoints refusing is a hard error that names the folder and
    /// carries both bodies, so the scope message Figma returns is visible.
    #[tokio::test]
    async fn list_project_files_reports_both_failures() {
        let server = mock_http::serve(vec![
            (
                403,
                r#"{"error":true,"status":403,"message":"no v2 for you"}"#.into(),
            ),
            (403, V1_SCOPE_403.into()),
        ]);
        let cfg = cfg_for(&server);

        let err = list_project_files(&cfg, &["77195660".to_owned()])
            .await
            .unwrap_err();
        let msg = format!("{err:#}");

        assert_eq!(server.paths().len(), 2);
        assert!(msg.contains("77195660"), "{msg}");
        assert!(msg.contains("no v2 for you"), "{msg}");
        assert!(
            msg.contains("requires the file_read or files:read or projects:read"),
            "{msg}"
        );
    }

    /// A non-403 failure on v2 (here: 500) must NOT fall back — v1 would
    /// only mask a real outage and double the request volume.
    #[tokio::test]
    async fn list_project_files_does_not_fall_back_on_non_403() {
        let server = mock_http::serve(vec![(500, "{}".into())]);
        let cfg = cfg_for(&server);

        let err = list_project_files(&cfg, &["77195660".to_owned()])
            .await
            .unwrap_err();

        assert_eq!(server.paths(), vec!["/v2/folders/77195660/files"]);
        assert!(format!("{err:#}").contains("500"), "{err:#}");
    }

    // ── ensure_fresh ────────────────────────────────────────────────────

    const FRESH_KEY: &str = "K";

    /// `GET /v1/files/K` body at Figma version `v2`.
    const FILE_V2: &str = r#"{"name":"F2","lastModified":"t2","version":"v2",
        "document":{"id":"0:0","name":"Document","type":"DOCUMENT","children":[
            {"id":"0:1","name":"Page","type":"CANVAS","children":[]}]}}"#;

    fn meta_body(version: &str) -> String {
        format!(r#"{{"file":{{"name":"F","last_touched_at":"t","version":"{version}"}}}}"#)
    }

    /// Cache with one `Ok` entry for [`FRESH_KEY`] at `version`, last
    /// confirmed at `checked_at`, claimed by project `10`.
    fn seed_fresh(version: Option<&str>, checked_at: Option<u64>) -> (TempDir, CacheDir) {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        let file_ref = FileRef {
            file_key: FRESH_KEY.into(),
            name: "F".into(),
            last_modified: "t1".into(),
            project_id: "10".into(),
            project_name: "P".into(),
        };
        let doc = json!({"id": "0:0", "name": "Document", "type": "DOCUMENT", "children": []});
        let payload = build_cached_file(&file_ref, &doc, 1);
        let bytes = cache.write_file(FRESH_KEY, &payload).unwrap();
        let mut meta = FileMeta::from_success(&file_ref, &payload, bytes, 1);
        meta.version = version.map(str::to_owned);
        meta.version_checked_at_epoch = checked_at;
        cache.write_meta(&meta).unwrap();
        (td, cache)
    }

    fn keys() -> Vec<String> {
        vec![FRESH_KEY.to_owned()]
    }

    #[tokio::test]
    async fn ensure_fresh_within_window_makes_no_requests() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(now_epoch()));
        let server = mock_http::serve(vec![]);
        let report = ensure_fresh(&cfg_for(&server), &cache, &keys()).await;
        assert!(server.paths().is_empty());
        assert!(report.refetched.is_empty() && report.unverified.is_empty());
    }

    #[tokio::test]
    async fn ensure_fresh_unchanged_version_only_restamps() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(0));
        let server = mock_http::serve(vec![(200, meta_body("v1"))]);
        let before = now_epoch();
        let report = ensure_fresh(&cfg_for(&server), &cache, &keys()).await;

        assert_eq!(server.paths(), vec!["/v1/files/K/meta"]);
        assert!(report.refetched.is_empty() && report.unverified.is_empty());
        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert!(meta.version_checked_at_epoch.unwrap() >= before);
        assert_eq!(meta.version.as_deref(), Some("v1"));
        assert_eq!(meta.last_modified, "t1", "payload untouched");
    }

    #[tokio::test]
    async fn ensure_fresh_changed_version_refetches_payload_and_sidecar() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(0));
        let server = mock_http::serve(vec![
            (200, meta_body("v2")),
            (200, FILE_V2.into()),
            (200, r#"{"comments":[]}"#.into()),
        ]);
        let report = ensure_fresh(&cfg_for(&server), &cache, &keys()).await;

        assert_eq!(
            server.paths(),
            vec!["/v1/files/K/meta", "/v1/files/K", "/v1/files/K/comments"]
        );
        assert_eq!(report.refetched, keys());
        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert_eq!(meta.version.as_deref(), Some("v2"));
        assert_eq!(meta.last_modified, "t2");
        assert_eq!(meta.project_id, "10", "project context preserved");
        assert!(meta.version_checked_at_epoch.is_some());
        assert!(
            full_sidecar_current(&meta),
            "full sidecar written in lockstep"
        );
        let full = crate::full_cache::read_full(&cache, FRESH_KEY)
            .unwrap()
            .unwrap();
        assert_eq!(full["version"], "v2");
        let payload = cache.read_file(FRESH_KEY).unwrap().unwrap();
        assert_eq!(payload.node_count, 2, "new document projected");
    }

    /// Metas written before freshness checks have no `version`: the first
    /// probe can't prove them current, so they refetch once.
    #[tokio::test]
    async fn ensure_fresh_unversioned_meta_refetches() {
        let (_td, cache) = seed_fresh(None, None);
        let server = mock_http::serve(vec![
            (200, meta_body("v2")),
            (200, FILE_V2.into()),
            (200, r#"{"comments":[]}"#.into()),
        ]);
        let report = ensure_fresh(&cfg_for(&server), &cache, &keys()).await;
        assert_eq!(server.paths().len(), 3);
        assert_eq!(report.refetched, keys());
        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert_eq!(meta.version.as_deref(), Some("v2"));
    }

    #[tokio::test]
    async fn ensure_fresh_probe_failure_serves_cache_untouched() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(0));
        let server = mock_http::serve(vec![(500, "{}".into())]);
        let report = ensure_fresh(&cfg_for(&server), &cache, &keys()).await;

        assert_eq!(server.paths(), vec!["/v1/files/K/meta"]);
        assert_eq!(report.unverified, keys());
        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert_eq!(meta.version_checked_at_epoch, Some(0), "not restamped");
        assert!(cache.read_file(FRESH_KEY).unwrap().is_some());
    }

    /// The regression this guards: `fetch_and_cache` turns a failed fetch
    /// into a failure marker that deletes the payload. A failed *freshness*
    /// refetch must instead keep serving the (stale) cached copy.
    #[tokio::test]
    async fn ensure_fresh_refetch_failure_keeps_cached_entry() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(0));
        let server = mock_http::serve(vec![(200, meta_body("v2")), (500, "{}".into())]);
        let report = ensure_fresh(&cfg_for(&server), &cache, &keys()).await;

        assert_eq!(server.paths(), vec!["/v1/files/K/meta", "/v1/files/K"]);
        assert_eq!(report.unverified, keys());
        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert_eq!(meta.status, EntryStatus::Ok);
        assert_eq!(meta.version.as_deref(), Some("v1"));
        assert!(cache.read_file(FRESH_KEY).unwrap().is_some());
    }

    /// After a failed refetch the file backs off for a window: no probe, no
    /// second timeout — just a warning — and it retries once the window ends.
    #[tokio::test]
    async fn ensure_fresh_failed_refetch_backs_off_then_retries() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(0));
        let server = mock_http::serve(vec![(200, meta_body("v2")), (500, "{}".into())]);
        ensure_fresh(&cfg_for(&server), &cache, &keys()).await;
        assert_eq!(server.paths().len(), 2);
        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert!(meta.refetch_failed_at_epoch.is_some(), "failure recorded");

        // Within the window: zero requests, still reported.
        let server = mock_http::serve(vec![]);
        let report = ensure_fresh(&cfg_for(&server), &cache, &keys()).await;
        assert!(server.paths().is_empty());
        assert_eq!(report.unverified, keys());

        // Window over: probe + refetch again; success clears the marker.
        let mut meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        meta.refetch_failed_at_epoch = Some(0);
        cache.write_meta(&meta).unwrap();
        let server = mock_http::serve(vec![
            (200, meta_body("v2")),
            (200, FILE_V2.into()),
            (200, r#"{"comments":[]}"#.into()),
        ]);
        let report = ensure_fresh(&cfg_for(&server), &cache, &keys()).await;
        assert_eq!(server.paths().len(), 3);
        assert_eq!(report.refetched, keys());
        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert_eq!(meta.refetch_failed_at_epoch, None);
        assert_eq!(meta.version.as_deref(), Some("v2"));
    }

    #[tokio::test]
    async fn ensure_fresh_skips_non_ok_entries() {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        let marker = FileMeta::failure_marker(
            FRESH_KEY.into(),
            "F".into(),
            "10".into(),
            "P".into(),
            "t1".into(),
            EntryStatus::Failed,
            "boom".into(),
            0,
        );
        cache.write_meta(&marker).unwrap();
        let server = mock_http::serve(vec![]);
        ensure_fresh(&cfg_for(&server), &cache, &keys()).await;
        assert!(server.paths().is_empty());
    }

    #[test]
    fn full_sidecar_current_requires_stamp_at_or_after_payload() {
        let mut m = ok_meta(100, 100);
        assert!(!full_sidecar_current(&m), "unstamped");
        m.full_schema_version = Some(FULL_SCHEMA_VERSION);
        m.full_fetched_at_epoch = Some(99);
        assert!(!full_sidecar_current(&m), "older than payload");
        m.full_fetched_at_epoch = Some(100);
        assert!(full_sidecar_current(&m));
        m.full_schema_version = Some(FULL_SCHEMA_VERSION + 1);
        assert!(!full_sidecar_current(&m), "other schema");
    }

    /// A 403/404 won't heal by retrying, so it restamps the check time and
    /// backs off a full window — unlike the 500 case above, which retries.
    #[tokio::test]
    async fn ensure_fresh_access_error_backs_off() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(0));
        let forbidden = r#"{"status":403,"err":"Forbidden"}"#;
        // `/meta` 403s, and so does the `?depth=1` fallback: no access.
        let server = mock_http::serve(vec![(403, forbidden.into()), (403, forbidden.into())]);
        let before = now_epoch();
        let report = ensure_fresh(&cfg_for(&server), &cache, &keys()).await;

        assert_eq!(
            server.paths(),
            vec!["/v1/files/K/meta", "/v1/files/K?depth=1"]
        );
        assert_eq!(report.unverified, keys());
        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert!(
            meta.version_checked_at_epoch.unwrap() >= before,
            "backed off"
        );
        assert_eq!(
            meta.version.as_deref(),
            Some("v1"),
            "still the cached version"
        );
    }

    /// A token without `file_metadata:read` gets a scope 403 from `/meta` but
    /// can still read the file: the probe falls back to `?depth=1` and works.
    #[tokio::test]
    async fn ensure_fresh_scope_403_falls_back_to_depth_1() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(0));
        let server = mock_http::serve(vec![
            (403, V1_SCOPE_403.into()),
            (
                200,
                r#"{"name":"F","version":"v1","document":{"id":"0:0","type":"DOCUMENT"}}"#.into(),
            ),
        ]);
        let before = now_epoch();
        let report = ensure_fresh(&cfg_for(&server), &cache, &keys()).await;

        assert_eq!(
            server.paths(),
            vec!["/v1/files/K/meta", "/v1/files/K?depth=1"]
        );
        assert!(report.unverified.is_empty() && report.refetched.is_empty());
        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert!(meta.version_checked_at_epoch.unwrap() >= before);
    }

    /// [`seed_fresh`] at `v1` (version check due) with a variables sidecar
    /// written at epoch 0, before the payload's `cached_at_epoch`.
    fn seed_with_variables() -> (TempDir, CacheDir) {
        let (td, cache) = seed_fresh(Some("v1"), Some(0));
        crate::full_cache::write_variables(&cache, FRESH_KEY, &json!({"old": true})).unwrap();
        let mut meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        meta.variables_fetched_at_epoch = Some(0);
        meta.variables_bytes = Some(42);
        meta.variables_schema_version = Some(VARIABLES_SCHEMA_VERSION);
        cache.write_meta(&meta).unwrap();
        (td, cache)
    }

    /// A file that has a variables sidecar gets it refreshed along with the
    /// payload, so variable values never describe an older document.
    #[tokio::test]
    async fn refetch_refreshes_existing_variables_sidecar() {
        let (_td, cache) = seed_with_variables();
        let server = mock_http::serve(vec![
            (200, meta_body("v2")),
            (200, FILE_V2.into()),
            (200, r#"{"comments":[]}"#.into()),
            (200, r#"{"meta":{"variables":{"new":1}}}"#.into()),
        ]);
        let report = ensure_fresh(&cfg_for(&server), &cache, &keys()).await;
        assert_eq!(server.paths()[3], "/v1/files/K/variables/local");
        assert_eq!(report.refetched, keys());

        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert_eq!(meta.version.as_deref(), Some("v2"));
        assert_eq!(meta.variables_fetched_at_epoch, Some(meta.cached_at_epoch));
        assert!(!variables_predate_payload(&meta));
        let vars = crate::full_cache::read_variables(&cache, FRESH_KEY)
            .unwrap()
            .unwrap();
        assert_eq!(vars["meta"]["variables"]["new"], 1);
    }

    /// A failed variables fetch doesn't fail the refetch: the old sidecar and
    /// its stamps stay, and the meta shows they predate the new payload.
    #[tokio::test]
    async fn refetch_keeps_old_variables_when_their_fetch_fails() {
        let (_td, cache) = seed_with_variables();
        let server = mock_http::serve(vec![
            (200, meta_body("v2")),
            (200, FILE_V2.into()),
            (200, r#"{"comments":[]}"#.into()),
            (500, "{}".into()),
        ]);
        let report = ensure_fresh(&cfg_for(&server), &cache, &keys()).await;
        assert_eq!(report.refetched, keys());

        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert_eq!(meta.version.as_deref(), Some("v2"));
        assert_eq!(meta.variables_fetched_at_epoch, Some(0));
        assert_eq!(meta.variables_bytes, Some(42));
        assert!(meta.variables_error.is_some());
        assert!(variables_predate_payload(&meta));
        let vars = crate::full_cache::read_variables(&cache, FRESH_KEY)
            .unwrap()
            .unwrap();
        assert_eq!(vars["old"], true);
    }

    #[test]
    fn is_access_error_matches_403_and_404_only() {
        assert!(is_access_error("figma API error (403 Forbidden): {}"));
        assert!(is_access_error("figma API error (404 Not Found): {}"));
        assert!(!is_access_error(
            "figma API error (500 Internal Server Error): {}"
        ));
        assert!(!is_access_error("HTTP request failed: operation timed out"));
    }

    /// `node-info` wants the body, not the cache update: a write failure
    /// (here: the payload path is a directory) must not lose the fetched body.
    #[tokio::test]
    async fn refetch_file_returns_body_when_cache_write_fails() {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        let file_ref = FileRef {
            file_key: FRESH_KEY.into(),
            name: "F".into(),
            last_modified: "t1".into(),
            project_id: "10".into(),
            project_name: "P".into(),
        };
        let doc = json!({"id": "0:0", "name": "Document", "type": "DOCUMENT", "children": []});
        let payload = build_cached_file(&file_ref, &doc, 1);
        cache
            .write_meta(&FileMeta::from_success(&file_ref, &payload, 0, 1))
            .unwrap();
        fs::create_dir_all(cache.file_path(FRESH_KEY)).unwrap();

        let server = mock_http::serve(vec![(200, FILE_V2.into())]);
        let body = refetch_file(&cfg_for(&server), &cache, FRESH_KEY)
            .await
            .unwrap();

        assert_eq!(server.paths(), vec!["/v1/files/K"]);
        assert_eq!(body["version"], "v2");
    }

    #[test]
    fn name_list_caps_at_max() {
        assert_eq!(name_list(["a", "b"].into_iter(), 5), "a, b");
        assert_eq!(
            name_list(["a", "b", "c", "d"].into_iter(), 2),
            "a, b, … +2 more"
        );
    }

    #[test]
    fn version_check_due_respects_window() {
        let mut m = ok_meta(0, 0);
        assert!(version_check_due(&m, 1_000), "never checked");
        m.version_checked_at_epoch = Some(1_000);
        assert!(!version_check_due(&m, 1_000 + VERSION_CHECK_SECS - 1));
        assert!(version_check_due(&m, 1_000 + VERSION_CHECK_SECS));
        m.status = EntryStatus::NotExportable;
        assert!(!version_check_due(&m, 10_000), "only Ok entries");
    }

    // ── ensure_comments_fresh ───────────────────────────────────────────

    /// `GET /v1/files/K/comments` body with one canvas-level thread `id`.
    fn comments_body(id: &str) -> String {
        format!(
            r#"{{"comments":[{{"id":"{id}","file_key":"K","parent_id":"","message":"m",
            "user":{{"id":"u","handle":"h","img_url":""}},"created_at":"2026-10-01T00:00:00Z",
            "resolved_at":null,"client_meta":{{"x":0,"y":0}},"order_id":"1","reactions":[]}}]}}"#
        )
    }

    fn comment_ids(cache: &CacheDir) -> Vec<String> {
        cache
            .read_comments(FRESH_KEY)
            .unwrap()
            .unwrap_or_default()
            .into_iter()
            .map(|c| c.comment_id)
            .collect()
    }

    /// [`seed_fresh`] plus a comments sidecar holding thread `old`, last
    /// fetched and checked at `checked_at`.
    async fn seed_comments(checked_at: u64) -> (TempDir, CacheDir) {
        let (td, cache) = seed_fresh(Some("v1"), Some(now_epoch()));
        let mut meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        let server = mock_http::serve(vec![(200, comments_body("old"))]);
        fetch_comments_into_meta(&cfg_for(&server), &cache, FRESH_KEY, checked_at, &mut meta).await;
        cache.write_meta(&meta).unwrap();
        assert_eq!(comment_ids(&cache), ["old"]);
        (td, cache)
    }

    #[tokio::test]
    async fn ensure_comments_fresh_within_window_makes_no_requests() {
        let (_td, cache) = seed_comments(now_epoch()).await;
        let server = mock_http::serve(vec![]);
        assert!(!ensure_comments_fresh(&cfg_for(&server), &cache, FRESH_KEY, 1).await);
        assert!(server.paths().is_empty());
    }

    /// The bug this guards: a new comment doesn't change the file's
    /// `version`, so only the comments clock can pick it up.
    #[tokio::test]
    async fn ensure_comments_fresh_refetches_past_window() {
        let (_td, cache) = seed_comments(0).await;
        let server = mock_http::serve(vec![(200, comments_body("new"))]);
        let before = now_epoch();
        assert!(ensure_comments_fresh(&cfg_for(&server), &cache, FRESH_KEY, 1).await);

        assert_eq!(server.paths(), vec!["/v1/files/K/comments"]);
        assert_eq!(comment_ids(&cache), ["new"]);
        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert!(meta.comments_fetched_at_epoch.unwrap() >= before);
        assert!(meta.comments_checked_at_epoch.unwrap() >= before);
    }

    /// A failed fetch keeps the old sidecar and still stamps the attempt, so
    /// the next command in the window doesn't hit the endpoint again.
    #[tokio::test]
    async fn ensure_comments_fresh_failure_keeps_sidecar_and_backs_off() {
        let (_td, cache) = seed_comments(0).await;
        let server = mock_http::serve(vec![(500, "{}".into())]);
        assert!(!ensure_comments_fresh(&cfg_for(&server), &cache, FRESH_KEY, 1).await);
        assert_eq!(server.paths().len(), 1);
        assert_eq!(comment_ids(&cache), ["old"], "old sidecar served");
        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert_eq!(meta.comments_fetched_at_epoch, Some(0), "success not faked");
        assert!(meta.comments_error.is_some());

        let server = mock_http::serve(vec![]);
        assert!(!ensure_comments_fresh(&cfg_for(&server), &cache, FRESH_KEY, 1).await);
        assert!(server.paths().is_empty(), "backed off for the window");
    }

    /// A version refetch whose comments call fails still serves the old
    /// sidecar, so the new meta must keep that sidecar's stamps.
    #[tokio::test]
    async fn refetch_keeps_comments_stamps_when_comments_fetch_fails() {
        let (_td, cache) = seed_comments(5).await;
        let mut meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        meta.version_checked_at_epoch = Some(0);
        cache.write_meta(&meta).unwrap();

        let server = mock_http::serve(vec![
            (200, meta_body("v2")),
            (200, FILE_V2.into()),
            (500, "{}".into()),
        ]);
        let report = ensure_fresh(&cfg_for(&server), &cache, &keys()).await;
        assert_eq!(report.refetched, keys());

        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert_eq!(meta.version.as_deref(), Some("v2"));
        assert_eq!(meta.comments_fetched_at_epoch, Some(5));
        assert!(meta.comments_fingerprint.is_some());
        assert_eq!(meta.comments_schema_version, Some(COMMENTS_SCHEMA_VERSION));
        assert!(meta.comments_error.is_some());
        assert_eq!(comment_ids(&cache), ["old"]);
    }

    /// Metas from before `comments_checked_at_epoch` fall back to the last
    /// successful fetch rather than counting as never checked.
    #[test]
    fn comments_check_due_falls_back_to_last_fetch() {
        let (_td, cache) = seed_fresh(Some("v1"), None);
        let mut meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        let now = 10_000;
        assert!(comments_check_due(&meta, now), "never fetched");
        meta.comments_fetched_at_epoch = Some(now - 10);
        assert!(!comments_check_due(&meta, now));
        meta.comments_checked_at_epoch = Some(now - VERSION_CHECK_SECS);
        assert!(comments_check_due(&meta, now), "attempt stamp wins");
    }

    // ── sync_folders ────────────────────────────────────────────────────

    /// `GET /v2/folders/10/files` body listing `(key, name)` pairs.
    fn folder_body(files: &[(&str, &str)]) -> String {
        let files: Vec<Value> = files
            .iter()
            .map(|(k, n)| json!({"key": k, "name": n, "thumbnail_url": "", "last_modified": "t"}))
            .collect();
        json!({"name": "P", "files": files}).to_string()
    }

    fn folders() -> Vec<String> {
        vec!["10".to_owned()]
    }

    #[tokio::test]
    async fn sync_folders_within_window_makes_no_requests() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(now_epoch()));
        cache.stamp_folder_listed("10", now_epoch()).unwrap();
        let server = mock_http::serve(vec![]);
        let report = sync_folders(&cfg_for(&server), &cache, &folders()).await;
        assert!(server.paths().is_empty());
        assert!(report.added.is_empty() && report.removed.is_empty());
    }

    /// The bug this guards: a file added to a folder on Figma never showed up
    /// until the next `cache prefetch`.
    #[tokio::test]
    async fn sync_folders_fetches_new_file_in_populated_folder() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(now_epoch()));
        let server = mock_http::serve(vec![
            (200, folder_body(&[("K", "F"), ("N", "New")])),
            (200, FILE_V2.into()),
            (200, r#"{"comments":[]}"#.into()),
        ]);
        let report = sync_folders(&cfg_for(&server), &cache, &folders()).await;

        assert_eq!(
            server.paths(),
            vec![
                "/v2/folders/10/files",
                "/v1/files/N",
                "/v1/files/N/comments"
            ]
        );
        assert_eq!(report.added, ["New"]);
        let meta = cache.read_meta("N").unwrap().unwrap();
        assert_eq!(meta.status, EntryStatus::Ok);
        assert_eq!(meta.project_id, "10");
        assert!(
            meta.version_checked_at_epoch.is_some(),
            "no probe needed next"
        );
        assert!(crate::synth::SynthState::load(&cache)
            .unwrap()
            .file_synth("N")
            .is_some());
        assert!(cache.folder_listed_at("10").is_some());
    }

    #[tokio::test]
    async fn sync_folders_drops_removed_and_updates_renamed_files() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(now_epoch()));
        // A second cached file in the same folder, about to be deleted.
        let mut gone = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        gone.file_key = "G".into();
        gone.name = "Gone".into();
        cache.write_meta(&gone).unwrap();

        let server = mock_http::serve(vec![(200, folder_body(&[("K", "Renamed")]))]);
        let report = sync_folders(&cfg_for(&server), &cache, &folders()).await;

        assert_eq!(server.paths().len(), 1);
        assert_eq!(report.removed, ["Gone"]);
        assert!(cache.read_meta("G").unwrap().is_none());
        assert_eq!(report.updated, ["Renamed"]);
        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert_eq!(meta.name, "Renamed");
        assert!(
            cache.read_file(FRESH_KEY).unwrap().is_some(),
            "not refetched"
        );
    }

    /// Syncing keeps a populated cache current; it never turns `ls` on an
    /// empty cache into a full prefetch.
    #[tokio::test]
    async fn sync_folders_leaves_unpopulated_folder_to_prefetch() {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        let server = mock_http::serve(vec![(200, folder_body(&[("N", "New")]))]);
        let report = sync_folders(&cfg_for(&server), &cache, &folders()).await;
        assert_eq!(server.paths(), vec!["/v2/folders/10/files"]);
        assert!(report.added.is_empty());
        assert!(cache.read_meta("N").unwrap().is_none());
    }

    /// A failed listing changes nothing — no file is treated as removed — and
    /// backs off for the window.
    #[tokio::test]
    async fn sync_folders_listing_failure_keeps_cache_and_backs_off() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(now_epoch()));
        let server = mock_http::serve(vec![(500, "{}".into())]);
        let report = sync_folders(&cfg_for(&server), &cache, &folders()).await;
        assert!(report.removed.is_empty());
        assert!(cache.read_meta(FRESH_KEY).unwrap().is_some());

        let server = mock_http::serve(vec![]);
        sync_folders(&cfg_for(&server), &cache, &folders()).await;
        assert!(server.paths().is_empty(), "backed off for the window");
    }

    /// Files cached under folders outside the synced set (another repo's
    /// `FIGMA_PROJECTS_IDS`) are never pruned.
    #[tokio::test]
    async fn sync_folders_leaves_other_folders_alone() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(now_epoch()));
        let mut other = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        other.file_key = "O".into();
        other.project_id = "20".into();
        cache.write_meta(&other).unwrap();

        let server = mock_http::serve(vec![(200, folder_body(&[("K", "F")]))]);
        let report = sync_folders(&cfg_for(&server), &cache, &folders()).await;
        assert!(report.removed.is_empty());
        assert!(cache.read_meta("O").unwrap().is_some());
    }

    /// `GET /v2/folders/{id}/files` body for folder `name`.
    fn named_folder_body(name: &str, files: &[(&str, &str)]) -> String {
        let mut v: Value = serde_json::from_str(&folder_body(files)).unwrap();
        v["name"] = json!(name);
        v.to_string()
    }

    /// Review finding: a `Failed` marker from a transient error on the first
    /// fetch must not hide the file for good.
    #[tokio::test]
    async fn sync_folders_retries_file_whose_fetch_failed() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(now_epoch()));
        let mut failed = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        failed.file_key = "N".into();
        failed.status = EntryStatus::Failed;
        cache.write_meta(&failed).unwrap();

        let server = mock_http::serve(vec![
            (200, folder_body(&[("K", "F"), ("N", "New")])),
            (200, FILE_V2.into()),
            (200, r#"{"comments":[]}"#.into()),
        ]);
        let report = sync_folders(&cfg_for(&server), &cache, &folders()).await;
        assert_eq!(report.added, ["New"]);
        assert_eq!(
            cache.read_meta("N").unwrap().unwrap().status,
            EntryStatus::Ok
        );
    }

    /// Review finding: a file moved between configured folders is a move,
    /// not a deletion — and its new folder gets a `proj:N`.
    #[tokio::test]
    async fn sync_folders_moves_file_between_folders() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(now_epoch()));
        // Folder 20 is within its window; 10 being due still lists both.
        cache.stamp_folder_listed("20", now_epoch()).unwrap();
        let server = mock_http::serve(vec![
            (200, folder_body(&[])),
            (200, named_folder_body("Q", &[("K", "F")])),
        ]);
        let ids = vec!["10".to_owned(), "20".to_owned()];
        let report = sync_folders(&cfg_for(&server), &cache, &ids).await;

        assert_eq!(server.paths().len(), 2, "whole set listed");
        assert!(report.removed.is_empty());
        let meta = cache.read_meta(FRESH_KEY).unwrap().unwrap();
        assert_eq!(meta.project_id, "20");
        assert_eq!(meta.project_name, "Q");
        assert!(
            cache.read_file(FRESH_KEY).unwrap().is_some(),
            "payload kept"
        );
        assert!(crate::synth::SynthState::load(&cache)
            .unwrap()
            .project_synth("20")
            .is_some());
    }

    /// Review finding: if any listing failed, a file missing from the others
    /// may be in that one — delete nothing.
    #[tokio::test]
    async fn sync_folders_skips_deletions_when_a_listing_failed() {
        let (_td, cache) = seed_fresh(Some("v1"), Some(now_epoch()));
        let server = mock_http::serve(vec![(200, folder_body(&[])), (500, "{}".into())]);
        let ids = vec!["10".to_owned(), "20".to_owned()];
        let report = sync_folders(&cfg_for(&server), &cache, &ids).await;
        assert!(report.removed.is_empty());
        assert!(cache.read_meta(FRESH_KEY).unwrap().is_some());
    }
}

//! Single entry point that turns an [`Id`] into something the commands can
//! operate on. Wraps the cache, synth state, and the lazily-built node index.
//!
//! Built once per CLI invocation in `main.rs` (or per command — both fine,
//! the work is cheap). The expensive piece — the [`NodeIndex`] — is only
//! constructed when a bare-node lookup actually needs it, via `OnceCell`.

use std::cell::OnceCell;

use anyhow::Context;
use figma_api::apis::configuration::Configuration;

use crate::cache::{self, CacheDir, CacheNode, CachedFile, EntryStatus, FileMeta};
use crate::comment_assoc::AssociatedComment;
use crate::id::Id;
use crate::marks::MarkStore;
use crate::node_index::NodeIndex;
use crate::synth::SynthState;
use crate::url::ParsedUrl;

/// Concrete target an [`Id`] resolved to.
#[derive(Debug, Clone)]
pub enum ResolvedTarget {
    /// No ID was passed — caller wants the root listing (projects + files
    /// from the cache manifest). Not a single entity; the caller pulls the
    /// data it needs from the resolver/cache.
    Root,
    /// `proj:N` — a project synth that exists in [`SynthState`].
    Project { synth: u32, project_id: String },
    /// `file:N` — a cached file's structural payload.
    File {
        synth: u32,
        meta: FileMeta,
        document: CachedFile,
    },
    /// A specific node inside a file, with the surrounding file context.
    Node {
        file_synth: u32,
        meta: FileMeta,
        node: CacheNode,
    },
    /// `file:N:comm:M` — a specific comment in a file, pre-associated with
    /// its anchor node in the `.comments.json` sidecar. Carries the comm
    /// synth so callers can render qualified ids and surface replies that
    /// belong to the same thread.
    Comment {
        file_synth: u32,
        comm_synth: u32,
        meta: FileMeta,
        comment: AssociatedComment,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("id is malformed: {0}")]
    Malformed(String),
    #[error("tag {0:?} is reserved for future use, not implemented yet")]
    ReservedTag(String),
    #[error("nothing cached for {0}")]
    NotCached(String),
    #[error("node id {node_id} is ambiguous across {} cached files", candidates.len())]
    Ambiguous {
        node_id: String,
        candidates: Vec<(u32, String)>,
    },
    #[error("--cache-only set but {0} is not in the local cache; remove --cache-only or run `figma-explorer cache prefetch`")]
    CacheOnlyMiss(String),
    /// The id was syntactically valid and namespaced to a real entity, but no
    /// command currently knows what to do with it. Currently unused — comment
    /// ids resolve for real (consumed by `node-info` and `comments`); kept
    /// for the reserved tags when they grow parse support.
    #[error("{id} cannot be resolved yet: {hint}")]
    NotResolvableYet { id: String, hint: String },
    #[error("internal: {0}")]
    Internal(String),
}

impl ResolveError {
    fn internal(e: impl std::fmt::Display) -> Self {
        ResolveError::Internal(format!("{e}"))
    }
}

/// Lazy, per-invocation resolution context.
pub struct Resolver {
    cache: CacheDir,
    synth: SynthState,
    /// Built on first bare-node lookup. None of the tagged paths need it,
    /// so common operations (`file:N`, `file:N:x:y`, URL) pay zero cost here.
    node_index: OnceCell<NodeIndex>,
    /// Loaded on first `mark:<key>` resolution. Cheap (a small JSON file), but
    /// deferred so the common tagged paths never touch it.
    marks: OnceCell<MarkStore>,
    cache_only: bool,
}

impl Resolver {
    /// Construct from the default cache directory. Loads [`SynthState`] once;
    /// [`NodeIndex`] is deferred. Cheap.
    pub fn new(cache_only: bool) -> anyhow::Result<Self> {
        let cache = CacheDir::new(cache::default_dir());
        Self::from_cache(cache, cache_only)
    }

    /// Construct against an explicit cache directory. Used by tests so they
    /// don't have to race on the global `FIGMA_EXPLORER_CACHE_DIR` env var.
    pub fn from_cache(cache: CacheDir, cache_only: bool) -> anyhow::Result<Self> {
        cache.ensure().context("preparing cache directory")?;
        let synth = SynthState::load(&cache).context("loading synth state")?;
        Ok(Self {
            cache,
            synth,
            node_index: OnceCell::new(),
            marks: OnceCell::new(),
            cache_only,
        })
    }

    pub fn cache(&self) -> &CacheDir {
        &self.cache
    }

    pub fn synth(&self) -> &SynthState {
        &self.synth
    }

    pub fn cache_only(&self) -> bool {
        self.cache_only
    }

    /// Build the node index on first call, cache it for the rest of the
    /// invocation. Errors from index construction surface as [`ResolveError::Internal`].
    pub fn node_index(&self) -> Result<&NodeIndex, ResolveError> {
        if let Some(idx) = self.node_index.get() {
            return Ok(idx);
        }
        let idx =
            NodeIndex::load_or_build(&self.cache, &self.synth).map_err(ResolveError::internal)?;
        Ok(self.node_index.get_or_init(|| idx))
    }

    /// Load the mark store on first call, cache it for the rest of the
    /// invocation. A corrupt `marks.json` surfaces as [`ResolveError::Internal`]
    /// here (direct `mark:<key>` resolution) — unlike the read-only `find` /
    /// `library` injection, which degrades to "no marks".
    pub fn marks(&self) -> Result<&MarkStore, ResolveError> {
        if let Some(m) = self.marks.get() {
            return Ok(m);
        }
        let store = MarkStore::load(&self.cache).map_err(ResolveError::internal)?;
        Ok(self.marks.get_or_init(|| store))
    }

    /// Resolve an [`Id`] to a concrete target.
    ///
    /// `cfg` is consulted only when the target's file is missing from the
    /// cache *and* `cache_only` is false — URL and tagged synth paths alike
    /// cold-fetch through [`cache::load_file`] on a miss.
    pub async fn resolve(
        &self,
        cfg: &Configuration,
        id: &Id,
    ) -> Result<ResolvedTarget, ResolveError> {
        match id {
            Id::Project(n) => self.resolve_project(*n),
            Id::File(n) => self.resolve_file(cfg, *n).await,
            Id::Node { file, node } => self.resolve_node(cfg, *file, node).await,
            Id::Comment { file, comm } => self.resolve_comment(*file, *comm),
            Id::BareNode(node_id) => self.resolve_bare(cfg, node_id).await,
            Id::Mark(key) => self.resolve_mark(cfg, key).await,
            Id::Url(parsed) => self.resolve_url(cfg, parsed).await,
        }
    }

    fn resolve_project(&self, synth: u32) -> Result<ResolvedTarget, ResolveError> {
        let project_id = self
            .synth
            .project_id(synth)
            .ok_or_else(|| ResolveError::NotCached(format!("proj:{synth}")))?
            .to_owned();
        Ok(ResolvedTarget::Project { synth, project_id })
    }

    async fn resolve_file(
        &self,
        cfg: &Configuration,
        synth: u32,
    ) -> Result<ResolvedTarget, ResolveError> {
        let file_key = self
            .synth
            .file_key(synth)
            .ok_or_else(|| ResolveError::NotCached(format!("file:{synth}")))?
            .to_owned();
        self.load_file_target(cfg, synth, &file_key, &format!("file:{synth}"))
            .await
    }

    async fn resolve_node(
        &self,
        cfg: &Configuration,
        file_synth: u32,
        node_id: &str,
    ) -> Result<ResolvedTarget, ResolveError> {
        let file_key = self
            .synth
            .file_key(file_synth)
            .ok_or_else(|| ResolveError::NotCached(format!("file:{file_synth}")))?
            .to_owned();
        let (meta, payload) = self
            .read_file_or_fetch(cfg, &format!("file:{file_synth}:{node_id}"), &file_key)
            .await?;
        let node = find_node(&payload.document, node_id).ok_or_else(|| {
            ResolveError::NotCached(format!(
                "file:{file_synth}:{node_id} (node id not found in file)"
            ))
        })?;
        Ok(ResolvedTarget::Node {
            file_synth,
            meta,
            node,
        })
    }

    /// Resolve `file:N:comm:M` by:
    ///
    /// 1. Looking up the file synth → file_key in [`SynthState`].
    /// 2. Looking up `(file_synth, comm_synth)` → Figma comment id in
    ///    [`SynthState`]'s comment table.
    /// 3. Reading the `.comments.json` sidecar and locating the entry by id.
    ///
    /// All four "not found" outcomes (unknown file synth, unknown comm synth,
    /// missing sidecar, comment id present in synth table but absent from
    /// sidecar) collapse to [`ResolveError::NotCached`] with a hint pointing
    /// at `cache prefetch`.
    fn resolve_comment(
        &self,
        file_synth: u32,
        comm_synth: u32,
    ) -> Result<ResolvedTarget, ResolveError> {
        let file_key = self
            .synth
            .file_key(file_synth)
            .ok_or_else(|| ResolveError::NotCached(format!("file:{file_synth}")))?
            .to_owned();
        let meta = self
            .cache
            .read_meta(&file_key)
            .map_err(ResolveError::internal)?
            .ok_or_else(|| {
                ResolveError::NotCached(format!("file:{file_synth} (no meta on disk)"))
            })?;
        let comment_id = self
            .synth
            .comment_id(file_synth, comm_synth)
            .ok_or_else(|| {
                ResolveError::NotCached(format!(
                    "file:{file_synth}:comm:{comm_synth} (unknown comment synth — run `cache prefetch`)"
                ))
            })?
            .to_owned();
        let comments = self
            .cache
            .read_comments(&file_key)
            .map_err(ResolveError::internal)?
            .ok_or_else(|| {
                ResolveError::NotCached(format!(
                    "file:{file_synth} has no comments sidecar (run `cache prefetch`)"
                ))
            })?;
        let comment = comments
            .into_iter()
            .find(|c| c.comment_id == comment_id)
            .ok_or_else(|| {
                ResolveError::NotCached(format!(
                    "file:{file_synth}:comm:{comm_synth} (synth points at comment {comment_id} but the sidecar no longer contains it; run `cache prefetch`)"
                ))
            })?;
        Ok(ResolvedTarget::Comment {
            file_synth,
            comm_synth,
            meta,
            comment,
        })
    }

    async fn resolve_bare(
        &self,
        cfg: &Configuration,
        node_id: &str,
    ) -> Result<ResolvedTarget, ResolveError> {
        let index = self.node_index()?;
        let candidates = index.lookup(node_id);
        match candidates.len() {
            0 => Err(ResolveError::NotCached(format!(
                "{node_id} (no cached file contains this node id; paste a Figma URL if the file isn't cached)"
            ))),
            1 => self.resolve_node(cfg, candidates[0], node_id).await,
            _ => {
                let mut named: Vec<(u32, String)> = candidates
                    .iter()
                    .filter_map(|&s| {
                        let key = self.synth.file_key(s)?;
                        let meta = self.cache.read_meta(key).ok().flatten()?;
                        Some((s, meta.name))
                    })
                    .collect();
                named.sort_by_key(|(s, _)| *s);
                Err(ResolveError::Ambiguous {
                    node_id: node_id.to_owned(),
                    candidates: named,
                })
            }
        }
    }

    /// Resolve `mark:<key>` through the mark store.
    ///
    /// - Unknown key → [`ResolveError::NotCached`] listing known keys (capped).
    /// - Multi-node mark → [`ResolveError::NotResolvableYet`] with the
    ///   paste-ready ids so the caller can pick one; a mark that fans out to
    ///   several nodes has no single target.
    /// - Single-node mark → resolves exactly like the underlying node, so
    ///   `node-info mark:k`, `screenshot mark:k`, and `--in mark:k` work with
    ///   zero per-command changes.
    async fn resolve_mark(
        &self,
        cfg: &Configuration,
        key: &str,
    ) -> Result<ResolvedTarget, ResolveError> {
        let store = self.marks()?;
        let mark = store.get(key).ok_or_else(|| {
            let mut known: Vec<&str> = store.keys().collect();
            known.sort_unstable();
            let shown: Vec<&str> = known.iter().take(10).copied().collect();
            let more = known.len().saturating_sub(shown.len());
            let list = if shown.is_empty() {
                "no marks yet — add one with `mark add <key> <ID>`".to_owned()
            } else {
                let suffix = if more > 0 {
                    format!(", … (+{more} more; `mark list`)")
                } else {
                    String::new()
                };
                format!("known marks: {}{suffix}", shown.join(", "))
            };
            ResolveError::NotCached(format!("mark:{key} (unknown key; {list})"))
        })?;

        match mark.nodes.as_slice() {
            [] => Err(ResolveError::NotCached(format!(
                "mark:{key} has no nodes (re-add it with `mark add {key} <ID>`)"
            ))),
            [single] => {
                let (file_key, node_id, stamp_name) = (
                    single.file_key.clone(),
                    single.node_id.clone(),
                    single.stamp.name.clone(),
                );
                self.resolve_mark_node(cfg, key, &file_key, &node_id, &stamp_name)
                    .await
            }
            many => {
                let ids: Vec<String> = many
                    .iter()
                    .map(|mn| {
                        self.synth
                            .file_synth(&mn.file_key)
                            .map(|n| format!("file:{n}:{}", mn.node_id))
                            .unwrap_or_else(|| format!("{}:{}", mn.file_key, mn.node_id))
                    })
                    .collect();
                Err(ResolveError::NotResolvableYet {
                    id: format!("mark:{key}"),
                    hint: format!(
                        "mark points at {} nodes; target one directly: {}",
                        many.len(),
                        ids.join(", ")
                    ),
                })
            }
        }
    }

    /// Resolve a single mark node (native `file_key` + `node_id`) to a
    /// [`ResolvedTarget::Node`]. Mirrors [`Self::resolve_url`]'s synth handling:
    /// use the interned synth when known, otherwise cold-fetch to mint one
    /// (or `CacheOnlyMiss` under `--cache-only`), then read the node.
    async fn resolve_mark_node(
        &self,
        cfg: &Configuration,
        mark_key: &str,
        file_key: &str,
        node_id: &str,
        stamp_name: &str,
    ) -> Result<ResolvedTarget, ResolveError> {
        let display = format!("mark:{mark_key}");
        let synth = match self.synth.file_synth(file_key) {
            Some(s) => s,
            None if self.cache_only => return Err(ResolveError::CacheOnlyMiss(display)),
            None => {
                let (_payload, s) =
                    cache::load_file(cfg, &self.cache, file_key)
                        .await
                        .map_err(|e| {
                            ResolveError::Internal(format!(
                                "fetching {file_key} for {display}: {e:#}"
                            ))
                        })?;
                s
            }
        };
        let (meta, payload) = self.read_file_or_fetch(cfg, &display, file_key).await?;
        let node = find_node(&payload.document, node_id).ok_or_else(|| {
            ResolveError::NotCached(format!(
                "mark:{mark_key} points at node {node_id} (\"{stamp_name}\") but it is no longer \
                 in the file — the design may have moved or deleted it. Run `cache prefetch` then \
                 `mark list` to check its status."
            ))
        })?;
        Ok(ResolvedTarget::Node {
            file_synth: synth,
            meta,
            node,
        })
    }

    async fn resolve_url(
        &self,
        cfg: &Configuration,
        url: &ParsedUrl,
    ) -> Result<ResolvedTarget, ResolveError> {
        // Try the cache first — a URL whose file_key we've already cached
        // becomes a no-op disk read (with a cold-fetch fallback should the
        // entry have been evicted since the synth was interned).
        if let Some(synth) = self.synth.file_synth(&url.file_key) {
            let display = format!("url:{}", url.file_key);
            let target = self
                .load_file_target(cfg, synth, &url.file_key, &display)
                .await?;
            return narrow_to_node_if_requested(target, url.node_id.as_deref());
        }

        if self.cache_only {
            return Err(ResolveError::CacheOnlyMiss(format!("url:{}", url.file_key)));
        }

        // Cold path: live fetch. `cache::load_file` writes meta+payload,
        // interns the file synth, and hands it back so we don't have to
        // reload SynthState from disk to learn what it just assigned.
        let (_payload, synth) = cache::load_file(cfg, &self.cache, &url.file_key)
            .await
            .map_err(|e| ResolveError::Internal(format!("fetching {}: {e:#}", url.file_key)))?;
        // `self.synth` is intentionally not updated here. The current
        // invocation needs the synth for this one resolution, which we hold,
        // and resolve_url is always terminal in a command's flow — no later
        // step queries `self.synth` for `url.file_key`.
        let display = format!("url:{}", url.file_key);
        let target = self
            .load_file_target(cfg, synth, &url.file_key, &display)
            .await?;
        narrow_to_node_if_requested(target, url.node_id.as_deref())
    }

    async fn load_file_target(
        &self,
        cfg: &Configuration,
        synth: u32,
        file_key: &str,
        display_id: &str,
    ) -> Result<ResolvedTarget, ResolveError> {
        let (meta, payload) = self.read_file_or_fetch(cfg, display_id, file_key).await?;
        Ok(ResolvedTarget::File {
            synth,
            meta,
            document: payload,
        })
    }

    /// [`Self::read_file`] with a cold-fetch fallback. Tagged synth paths
    /// behave like the URL lane: on a cache miss, refetch via
    /// [`cache::load_file`] when allowed (its `decide_action` throttles
    /// NotExportable markers by TTL, so a 403'd file is not re-hammered), or
    /// fail with [`ResolveError::CacheOnlyMiss`] under `--cache-only`.
    /// Deliberately does NOT pre-warm node-info's `.full.json.gz` sidecar —
    /// that would tax every refresh to save one API call on a fully-cold
    /// `node-info`; its own `load_full` self-heals on the next call.
    async fn read_file_or_fetch(
        &self,
        cfg: &Configuration,
        display_id: &str,
        file_key: &str,
    ) -> Result<(FileMeta, CachedFile), ResolveError> {
        match self.read_file(file_key) {
            Ok(pair) => Ok(pair),
            Err(ResolveError::NotCached(_)) if self.cache_only => {
                Err(ResolveError::CacheOnlyMiss(display_id.to_owned()))
            }
            Err(ResolveError::NotCached(_)) => {
                cache::load_file(cfg, &self.cache, file_key)
                    .await
                    .map_err(|e| ResolveError::Internal(format!("fetching {file_key}: {e:#}")))?;
                self.read_file(file_key)
            }
            Err(other) => Err(other),
        }
    }

    /// Disk-only read of meta + payload. No TTL refresh, no live fetch — the
    /// cold-fetch fallback lives in [`Self::read_file_or_fetch`], which every
    /// file-loading lane routes through. Keep this as the sync primitive for
    /// callers that must not touch the network.
    fn read_file(&self, file_key: &str) -> Result<(FileMeta, CachedFile), ResolveError> {
        let meta = self
            .cache
            .read_meta(file_key)
            .map_err(ResolveError::internal)?
            .ok_or_else(|| {
                ResolveError::NotCached(format!(
                    "file_key {file_key} (no meta on disk); run `figma-explorer cache prefetch` or pass the file's Figma URL"
                ))
            })?;
        if meta.status != EntryStatus::Ok {
            return Err(ResolveError::NotCached(format!(
                "file_key {file_key} is cached with status {:?}; run `cache prefetch` to refresh",
                meta.status
            )));
        }
        let payload = match self.cache.read_file(file_key) {
            Ok(Some(p)) => p,
            Ok(None) => {
                return Err(ResolveError::NotCached(format!(
                    "file_key {file_key} payload missing"
                )))
            }
            // Schema drift (a `CACHE_SCHEMA_VERSION` bump) or rkyv corruption:
            // per `cache::read_file`'s contract, treat it as a cache miss so
            // `read_file_or_fetch` refetches into the new schema (or reports a
            // `CacheOnlyMiss` under `--cache-only`) instead of dead-ending on a
            // raw "schema version mismatch" internal error.
            Err(e) => {
                return Err(ResolveError::NotCached(format!(
                    "file_key {file_key} payload unreadable ({e}); refetching"
                )))
            }
        };
        Ok((meta, payload))
    }
}

fn narrow_to_node_if_requested(
    target: ResolvedTarget,
    node_id: Option<&str>,
) -> Result<ResolvedTarget, ResolveError> {
    let Some(node_id) = node_id else {
        return Ok(target);
    };
    match target {
        ResolvedTarget::File {
            synth,
            meta,
            document,
        } => {
            let node = find_node(&document.document, node_id).ok_or_else(|| {
                ResolveError::NotCached(format!(
                    "file:{synth}:{node_id} (node id not found in file)"
                ))
            })?;
            Ok(ResolvedTarget::Node {
                file_synth: synth,
                meta,
                node,
            })
        }
        other => Ok(other),
    }
}

/// DFS for an exact-match node id within a (potentially huge) cache tree.
/// Returns an owned clone — callers want an independent subtree they can
/// hold past the lifetime of `root`.
///
/// Deliberately separate from [`crate::node_search::resolve_node_id`], which
/// is the same DFS over the raw `serde_json::Value` document and returns a
/// borrow. The split is by type and ownership, not an oversight: this one
/// walks the structural `CacheNode` projection and yields an owned subtree;
/// the other walks the full untyped JSON (for fields the cache drops) and
/// yields a borrow. Two ~8-line functions aren't worth a generic over both.
///
/// Unbounded recursion is safe: `CacheNode` trees come through
/// `cache::project_to_cache`, which caps depth at `MAX_NODE_DEPTH`.
fn find_node(root: &CacheNode, target_id: &str) -> Option<CacheNode> {
    if root.id == target_id {
        return Some(root.clone());
    }
    for c in &root.children {
        if let Some(hit) = find_node(c, target_id) {
            return Some(hit);
        }
    }
    None
}

/// Translate an id parse error into [`ResolveError`]. Used by callers that
/// take user-provided strings and feed them to the resolver.
pub fn parse_id(input: &str) -> Result<Id, ResolveError> {
    input.parse::<Id>().map_err(|e| match e {
        crate::id::IdParseError::ReservedTag { tag } => ResolveError::ReservedTag(tag),
        other => ResolveError::Malformed(format!("{other}")),
    })
}

/// Pretty-print a [`ResolveError`] before returning it. Ambiguity errors get
/// special-cased: YAML mode lists candidates on stderr with a fixup hint;
/// JSON mode emits a structured error object to stdout so `| jq` pipelines
/// can consume it. Other variants fall through to a plain `anyhow` error.
///
/// Returns `anyhow::Error` so command code can `?`-propagate without losing
/// the non-zero exit code.
pub fn render_resolve_error(err: ResolveError, output: crate::Output) -> anyhow::Error {
    use serde_json::{json, Value};
    if let ResolveError::Ambiguous {
        node_id,
        candidates,
    } = &err
    {
        match output {
            crate::Output::Json => {
                let cands: Vec<Value> = candidates
                    .iter()
                    .take(5)
                    .map(|(s, name)| json!({ "id": format!("file:{s}"), "name": name }))
                    .collect();
                let overflow = candidates.len().saturating_sub(5);
                let payload = json!({
                    "error": "ambiguous",
                    "node_id": node_id,
                    "candidates": cands,
                    "more": overflow,
                });
                if let Ok(s) = serde_json::to_string_pretty(&payload) {
                    println!("{s}");
                }
            }
            crate::Output::Yaml => {
                eprintln!(
                    "error: node {node_id} is ambiguous across {} cached files.",
                    candidates.len()
                );
                eprintln!("       Disambiguate with --in or paste a URL. Top candidates:");
                for (s, name) in candidates.iter().take(5) {
                    eprintln!("         file:{s}   FILE \"{name}\"");
                }
                let overflow = candidates.len().saturating_sub(5);
                if overflow > 0 {
                    eprintln!("         ... {overflow} more (see `figma-explorer ls`)");
                }
                eprintln!("       Try:  figma-explorer ls file:<N>:{node_id}");
                eprintln!("       Or:   figma-explorer ls https://www.figma.com/design/...");
            }
        }
    }
    anyhow::anyhow!("{err}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{build_cached_file, FileRef};
    use serde_json::json;

    fn fixture_with_two_files() -> (tempfile::TempDir, Resolver) {
        let tmp = tempfile::tempdir().unwrap();
        let cache = CacheDir::new(tmp.path());
        cache.ensure().unwrap();

        // File A: 0:0 (DOCUMENT), 0:1 (CANVAS), 1:2 (FRAME)
        let doc_a = json!({
            "id": "0:0", "name": "doc", "type": "DOCUMENT",
            "children": [{
                "id": "0:1", "name": "Cover", "type": "CANVAS",
                "children": [{ "id": "1:2", "name": "Header", "type": "FRAME" }]
            }]
        });
        let ref_a = FileRef {
            file_key: "file-a".into(),
            name: "A".into(),
            last_modified: "2024-01-01".into(),
            project_id: "p1".into(),
            project_name: "P".into(),
        };
        let pa = build_cached_file(&ref_a, &doc_a, 0);
        cache.write_file("file-a", &pa).unwrap();
        cache
            .write_meta(&FileMeta::from_success(&ref_a, &pa, 0, 0))
            .unwrap();

        // File B: identical 0:0/0:1 shape (so 0:0 collides), unique 9:9.
        let doc_b = json!({
            "id": "0:0", "name": "doc", "type": "DOCUMENT",
            "children": [{
                "id": "0:1", "name": "Sheet", "type": "CANVAS",
                "children": [{ "id": "9:9", "name": "Banner", "type": "FRAME" }]
            }]
        });
        let ref_b = FileRef {
            file_key: "file-b".into(),
            name: "B".into(),
            last_modified: "2024-01-01".into(),
            project_id: "p1".into(),
            project_name: "P".into(),
        };
        let pb = build_cached_file(&ref_b, &doc_b, 0);
        cache.write_file("file-b", &pb).unwrap();
        cache
            .write_meta(&FileMeta::from_success(&ref_b, &pb, 0, 0))
            .unwrap();

        // Seed synth state.
        crate::synth::with_lock(&cache, |s| {
            s.intern_project("p1");
            s.intern_file("file-a");
            s.intern_file("file-b");
        })
        .unwrap();

        let resolver = Resolver::from_cache(CacheDir::new(tmp.path()), true).unwrap();
        (tmp, resolver)
    }

    fn dummy_cfg() -> Configuration {
        Configuration::new()
    }

    #[tokio::test]
    async fn resolves_project_by_synth() {
        let (_g, r) = fixture_with_two_files();
        let id: Id = "proj:1".parse().unwrap();
        let target = r.resolve(&dummy_cfg(), &id).await.unwrap();
        match target {
            ResolvedTarget::Project { synth, project_id } => {
                assert_eq!(synth, 1);
                assert_eq!(project_id, "p1");
            }
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolves_file_by_synth() {
        let (_g, r) = fixture_with_two_files();
        let id: Id = "file:1".parse().unwrap();
        let target = r.resolve(&dummy_cfg(), &id).await.unwrap();
        match target {
            ResolvedTarget::File { synth, meta, .. } => {
                assert_eq!(synth, 1);
                assert_eq!(meta.file_key, "file-a");
            }
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolves_qualified_node() {
        let (_g, r) = fixture_with_two_files();
        let id: Id = "file:1:1:2".parse().unwrap();
        let target = r.resolve(&dummy_cfg(), &id).await.unwrap();
        match target {
            ResolvedTarget::Node {
                file_synth, node, ..
            } => {
                assert_eq!(file_synth, 1);
                assert_eq!(node.id, "1:2");
                assert_eq!(node.name, "Header");
            }
            other => panic!("expected Node, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bare_node_unique_match_resolves() {
        let (_g, r) = fixture_with_two_files();
        let id: Id = "1:2".parse().unwrap();
        let target = r.resolve(&dummy_cfg(), &id).await.unwrap();
        match target {
            ResolvedTarget::Node {
                file_synth, node, ..
            } => {
                assert_eq!(file_synth, 1);
                assert_eq!(node.id, "1:2");
            }
            other => panic!("expected Node, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bare_node_ambiguous_yields_candidates() {
        let (_g, r) = fixture_with_two_files();
        let id: Id = "0:0".parse().unwrap();
        let err = r.resolve(&dummy_cfg(), &id).await.unwrap_err();
        match err {
            ResolveError::Ambiguous {
                node_id,
                candidates,
            } => {
                assert_eq!(node_id, "0:0");
                assert_eq!(candidates.len(), 2);
                let synths: Vec<u32> = candidates.iter().map(|(s, _)| *s).collect();
                assert_eq!(synths, vec![1, 2]);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bare_node_not_found_returns_not_cached() {
        let (_g, r) = fixture_with_two_files();
        let id: Id = "999:999".parse().unwrap();
        let err = r.resolve(&dummy_cfg(), &id).await.unwrap_err();
        assert!(matches!(err, ResolveError::NotCached(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn unknown_synth_errors() {
        let (_g, r) = fixture_with_two_files();
        let id: Id = "file:42".parse().unwrap();
        let err = r.resolve(&dummy_cfg(), &id).await.unwrap_err();
        assert!(matches!(err, ResolveError::NotCached(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn url_to_uncached_in_cache_only_errors() {
        let (_g, r) = fixture_with_two_files();
        let id: Id = "https://www.figma.com/design/UNCACHED-KEY/Foo"
            .parse()
            .unwrap();
        let err = r.resolve(&dummy_cfg(), &id).await.unwrap_err();
        assert!(matches!(err, ResolveError::CacheOnlyMiss(_)), "got {err:?}");
    }

    /// A payload written under an older `CACHE_SCHEMA_VERSION` (the situation
    /// after a schema bump) must resolve as a cache miss, not a hard internal
    /// error — so live lanes refetch and `--cache-only` reports a clean miss.
    #[tokio::test]
    async fn schema_mismatched_payload_is_a_miss_not_internal_error() {
        let (g, r) = fixture_with_two_files(); // cache_only resolver
                                               // Overwrite file-a's payload with a valid magic + a future version so
                                               // decode returns `VersionMismatch`; the meta stays Ok.
        let mut bytes = crate::cache::CACHE_MAGIC.to_vec();
        bytes.extend_from_slice(&999u32.to_le_bytes());
        bytes.extend_from_slice(b"stale body");
        std::fs::write(CacheDir::new(g.path()).file_path("file-a"), bytes).unwrap();

        let id: Id = "file:1".parse().unwrap();
        let err = r.resolve(&dummy_cfg(), &id).await.unwrap_err();
        assert!(
            matches!(err, ResolveError::CacheOnlyMiss(_)),
            "schema drift must be a miss, got {err:?}"
        );
    }

    #[tokio::test]
    async fn evicted_synth_file_cache_only_yields_cache_only_miss() {
        let (g, r) = fixture_with_two_files(); // fixture resolver is cache_only
        CacheDir::new(g.path()).delete_entry("file-a").unwrap();

        let id: Id = "file:1".parse().unwrap();
        let err = r.resolve(&dummy_cfg(), &id).await.unwrap_err();
        match &err {
            ResolveError::CacheOnlyMiss(what) => assert_eq!(what, "file:1"),
            other => panic!("expected CacheOnlyMiss, got {other:?}"),
        }

        let id: Id = "file:1:1:2".parse().unwrap();
        let err = r.resolve(&dummy_cfg(), &id).await.unwrap_err();
        match &err {
            ResolveError::CacheOnlyMiss(what) => assert_eq!(what, "file:1:1:2"),
            other => panic!("expected CacheOnlyMiss, got {other:?}"),
        }
    }

    /// Without `--cache-only`, an evicted synth-file target must attempt a
    /// live refetch *into the resolver's own cache dir* — the point of
    /// injecting the CacheDir into `cache::load_file`. The unroutable
    /// base_path makes the fetch fail fast; the failure marker meta landing
    /// in the tempdir proves both that the fallback fired and where it wrote.
    #[tokio::test]
    async fn evicted_synth_file_attempts_refetch_into_injected_cache() {
        let (g, _) = fixture_with_two_files();
        let cache_dir = CacheDir::new(g.path());
        cache_dir.delete_entry("file-a").unwrap();

        let r = Resolver::from_cache(CacheDir::new(g.path()), false).unwrap();
        let mut cfg = Configuration::new();
        cfg.base_path = "http://127.0.0.1:9".into();

        let id: Id = "file:1".parse().unwrap();
        let err = r.resolve(&cfg, &id).await.unwrap_err();
        match &err {
            ResolveError::Internal(msg) => {
                assert!(msg.contains("fetching"), "got: {msg}")
            }
            other => panic!("expected Internal, got {other:?}"),
        }

        let marker = cache_dir.read_meta("file-a").unwrap();
        assert!(
            matches!(&marker, Some(m) if m.status == EntryStatus::Failed),
            "expected a Failed marker meta in the injected cache dir, got {marker:?}"
        );
    }

    #[tokio::test]
    async fn url_with_known_synth_but_evicted_entry_cache_only_miss() {
        let (g, r) = fixture_with_two_files();
        CacheDir::new(g.path()).delete_entry("file-a").unwrap();

        let id: Id = "https://www.figma.com/design/file-a/Foo".parse().unwrap();
        let err = r.resolve(&dummy_cfg(), &id).await.unwrap_err();
        match &err {
            ResolveError::CacheOnlyMiss(what) => assert_eq!(what, "url:file-a"),
            other => panic!("expected CacheOnlyMiss, got {other:?}"),
        }
    }

    /// `file:N:comm:M` resolves to `ResolvedTarget::Comment` when the sidecar
    /// has the corresponding entry. The previously-stubbed
    /// `NotResolvableYet` path is exercised in the *failure* branches —
    /// missing sidecar, missing synth — but the happy path is what callers
    /// (the `node-info` command) depend on.
    #[tokio::test]
    async fn resolves_comment_by_synth() {
        use crate::comment_assoc::{
            Anchor, AnchorKind, AssociatedComment, AssociationMethod, NodeRef,
        };
        let (g, _) = fixture_with_two_files();

        // Seed a comments sidecar + comment synth for file-a *before*
        // constructing the resolver — the resolver loads SynthState once at
        // construction and doesn't re-read it for comment lookups. This
        // mirrors the real usage pattern (prefetch runs in one invocation,
        // node-info runs in a later one with a fresh resolver).
        let cache_dir = CacheDir::new(g.path());
        let comment = AssociatedComment {
            comment_id: "42".into(),
            message: "Sample".into(),
            author: "henrik@akesson.mobi".into(),
            created_at: "2026-05-12T00:00:00Z".into(),
            resolved_at: None,
            parent_id: None,
            order_id: Some("1".into()),
            reactions: 0,
            anchor: Anchor {
                kind: AnchorKind::Vector,
                explicit_node_id: None,
                canvas_point: Some([0.0, 0.0]),
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
        cache_dir.write_comments("file-a", &[comment]).unwrap();
        crate::synth::with_lock(&cache_dir, |s| s.intern_comment(1, "42")).unwrap();

        let r = Resolver::from_cache(CacheDir::new(g.path()), true).unwrap();
        let id: Id = "file:1:comm:1".parse().unwrap();
        let target = r.resolve(&dummy_cfg(), &id).await.unwrap();
        match target {
            ResolvedTarget::Comment {
                file_synth,
                comm_synth,
                comment,
                ..
            } => {
                assert_eq!(file_synth, 1);
                assert_eq!(comm_synth, 1);
                assert_eq!(comment.comment_id, "42");
                assert_eq!(comment.message, "Sample");
            }
            other => panic!("expected Comment, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_comm_synth_errors() {
        let (_g, r) = fixture_with_two_files();
        // File exists but no comment synth has been interned for index 99.
        let id: Id = "file:1:comm:99".parse().unwrap();
        let err = r.resolve(&dummy_cfg(), &id).await.unwrap_err();
        assert!(matches!(err, ResolveError::NotCached(_)), "got {err:?}");
    }

    // ── mark resolution ──────────────────────────────────────────────────

    /// Seed a mark into the resolver's cache dir. Must run before the first
    /// `mark:` resolution so the resolver's OnceCell picks it up.
    fn seed_mark(tmp: &tempfile::TempDir, key: &str, nodes: Vec<crate::marks::MarkNode>) {
        let cache = CacheDir::new(tmp.path());
        crate::marks::with_lock(&cache, |store| {
            store.upsert(crate::marks::Mark {
                key: key.into(),
                aliases: vec![],
                nodes,
                note: None,
            });
            Ok(())
        })
        .unwrap();
    }

    fn mark_node(file_key: &str, node_id: &str, name: &str) -> crate::marks::MarkNode {
        crate::marks::MarkNode {
            file_key: file_key.into(),
            node_id: node_id.into(),
            stamp: crate::marks::Stamp {
                name: name.into(),
                path: vec![],
                at_epoch: 0,
            },
        }
    }

    #[tokio::test]
    async fn single_node_mark_resolves_to_the_node() {
        let (g, r) = fixture_with_two_files();
        seed_mark(&g, "hdr", vec![mark_node("file-a", "1:2", "Header")]);
        let id: Id = "mark:hdr".parse().unwrap();
        match r.resolve(&dummy_cfg(), &id).await.unwrap() {
            ResolvedTarget::Node {
                file_synth, node, ..
            } => {
                assert_eq!(file_synth, 1, "file-a's synth");
                assert_eq!(node.id, "1:2");
                assert_eq!(node.name, "Header");
            }
            other => panic!("expected Node, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn multi_node_mark_is_not_resolvable_yet_with_candidate_ids() {
        let (g, r) = fixture_with_two_files();
        seed_mark(
            &g,
            "both",
            vec![
                mark_node("file-a", "1:2", "Header"),
                mark_node("file-b", "9:9", "Banner"),
            ],
        );
        let id: Id = "mark:both".parse().unwrap();
        match r.resolve(&dummy_cfg(), &id).await.unwrap_err() {
            ResolveError::NotResolvableYet { id, hint } => {
                assert_eq!(id, "mark:both");
                // Both nodes' paste-ready synth ids appear in the hint.
                assert!(hint.contains("file:1:1:2"), "hint: {hint}");
                assert!(hint.contains("file:2:9:9"), "hint: {hint}");
            }
            other => panic!("expected NotResolvableYet, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_mark_key_lists_known_keys() {
        let (g, r) = fixture_with_two_files();
        seed_mark(&g, "alpha", vec![mark_node("file-a", "1:2", "Header")]);
        let id: Id = "mark:nope".parse().unwrap();
        match r.resolve(&dummy_cfg(), &id).await.unwrap_err() {
            ResolveError::NotCached(msg) => {
                assert!(msg.contains("unknown key"), "msg: {msg}");
                assert!(msg.contains("alpha"), "lists known keys: {msg}");
            }
            other => panic!("expected NotCached, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_pointing_at_deleted_node_gives_useful_error() {
        let (g, r) = fixture_with_two_files();
        // Node 7:7 doesn't exist in file-a.
        seed_mark(&g, "ghost", vec![mark_node("file-a", "7:7", "Was Here")]);
        let id: Id = "mark:ghost".parse().unwrap();
        match r.resolve(&dummy_cfg(), &id).await.unwrap_err() {
            ResolveError::NotCached(msg) => {
                assert!(msg.contains("7:7"), "names the node id: {msg}");
                assert!(msg.contains("Was Here"), "names the stamped name: {msg}");
            }
            other => panic!("expected NotCached, got {other:?}"),
        }
    }
}

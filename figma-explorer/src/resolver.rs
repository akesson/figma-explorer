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
    /// command currently knows what to do with it. Today only `file:N:comm:M`
    /// hits this — comment ids are output-only until the future `info` command.
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

    /// Resolve an [`Id`] to a concrete target.
    ///
    /// `cfg` is consulted only when the id is a `Url` *and* `cache_only` is
    /// false — that's the one path that can trigger a live fetch.
    pub async fn resolve(
        &self,
        cfg: &Configuration,
        id: &Id,
    ) -> Result<ResolvedTarget, ResolveError> {
        match id {
            Id::Project(n) => self.resolve_project(*n),
            Id::File(n) => self.resolve_file(*n),
            Id::Node { file, node } => self.resolve_node(*file, node),
            Id::Comment { file, comm } => self.resolve_comment(*file, *comm),
            Id::BareNode(node_id) => self.resolve_bare(node_id),
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

    fn resolve_file(&self, synth: u32) -> Result<ResolvedTarget, ResolveError> {
        let file_key = self
            .synth
            .file_key(synth)
            .ok_or_else(|| ResolveError::NotCached(format!("file:{synth}")))?
            .to_owned();
        self.load_file_target(synth, &file_key)
    }

    fn resolve_node(&self, file_synth: u32, node_id: &str) -> Result<ResolvedTarget, ResolveError> {
        let file_key = self
            .synth
            .file_key(file_synth)
            .ok_or_else(|| ResolveError::NotCached(format!("file:{file_synth}")))?
            .to_owned();
        let (meta, payload) = self.read_file(&file_key)?;
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

    fn resolve_bare(&self, node_id: &str) -> Result<ResolvedTarget, ResolveError> {
        let index = self.node_index()?;
        let candidates = index.lookup(node_id);
        match candidates.len() {
            0 => Err(ResolveError::NotCached(format!(
                "{node_id} (no cached file contains this node id; paste a Figma URL if the file isn't cached)"
            ))),
            1 => self.resolve_node(candidates[0], node_id),
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

    async fn resolve_url(
        &self,
        cfg: &Configuration,
        url: &ParsedUrl,
    ) -> Result<ResolvedTarget, ResolveError> {
        // Try the cache first — a URL whose file_key we've already cached
        // becomes a no-op disk read.
        if let Some(synth) = self.synth.file_synth(&url.file_key) {
            let target = self.load_file_target(synth, &url.file_key)?;
            return narrow_to_node_if_requested(target, url.node_id.as_deref());
        }

        if self.cache_only {
            return Err(ResolveError::CacheOnlyMiss(format!("url:{}", url.file_key)));
        }

        // Cold path: live fetch. `cache::load_file` writes meta+payload,
        // interns the file synth, and hands it back so we don't have to
        // reload SynthState from disk to learn what it just assigned.
        let (_payload, synth) = cache::load_file(cfg, &url.file_key)
            .await
            .map_err(|e| ResolveError::Internal(format!("fetching {}: {e:#}", url.file_key)))?;
        // `self.synth` is intentionally not updated here. The current
        // invocation needs the synth for this one resolution, which we hold,
        // and resolve_url is always terminal in a command's flow — no later
        // step queries `self.synth` for `url.file_key`.
        let target = self.load_file_target(synth, &url.file_key)?;
        narrow_to_node_if_requested(target, url.node_id.as_deref())
    }

    fn load_file_target(&self, synth: u32, file_key: &str) -> Result<ResolvedTarget, ResolveError> {
        let (meta, payload) = self.read_file(file_key)?;
        Ok(ResolvedTarget::File {
            synth,
            meta,
            document: payload,
        })
    }

    /// Disk-only read of meta + payload. No TTL refresh, no live fetch — that
    /// path is handled by the URL resolver above. Use for the tagged-id paths
    /// where we already know what we want and a missing cache entry is an
    /// error (rather than a refresh trigger).
    fn read_file(&self, file_key: &str) -> Result<(FileMeta, CachedFile), ResolveError> {
        let meta = self
            .cache
            .read_meta(file_key)
            .map_err(ResolveError::internal)?
            .ok_or_else(|| {
                ResolveError::NotCached(format!("file_key {file_key} (no meta on disk)"))
            })?;
        if meta.status != EntryStatus::Ok {
            return Err(ResolveError::NotCached(format!(
                "file_key {file_key} is cached with status {:?}; run `cache prefetch` to refresh",
                meta.status
            )));
        }
        let payload = self
            .cache
            .read_file(file_key)
            .map_err(ResolveError::internal)?
            .ok_or_else(|| {
                ResolveError::NotCached(format!("file_key {file_key} payload missing"))
            })?;
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
}

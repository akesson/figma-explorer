//! Curated keyword → node "marks": a durable, searchable map from the words an
//! agent actually searches for ("leave tooltip", "wallchart cell") to the
//! Figma nodes they name.
//!
//! ## Why this exists
//!
//! Discovery is the expensive part of navigating a Figma file: user vocabulary
//! ("the leave hover card") rarely matches designer layer names ("Wall chart
//! cell dropdown"), so the first hunt for any entity costs many queries. Once
//! an agent *has* positively identified a node, that mapping is worth keeping —
//! but it evaporates at the end of a session. A mark writes it down.
//!
//! `find` and `library search` fold matching marks in ahead of their own hits,
//! so a marked entity is a single fuzzy query away forever after.
//!
//! ## Storage
//!
//! `<cache-root>/marks.json` + `marks.lock`, mirroring [`crate::synth`]
//! structurally (atomic tempfile save, fs2 advisory lock via [`with_lock`],
//! versioned schema). It lives beside `synth.json` in the cache root — *not*
//! under `files/` — because a mark spans (file_key, node_id) pairs and must
//! survive `cache clear` (which only sweeps `files/` and `teams/`). Like
//! synth ids, marks are machine-local: [`MarkNode`] stores **native** Figma
//! ids (`file_key` + `node_id`), never the machine-specific `file:N` synths.
//!
//! ## Staleness
//!
//! A [`Stamp`] records what the node looked like when marked — its name and
//! ancestor-name path. [`check_freshness`] compares that against the current
//! cached document so a mark can flag itself `[renamed]` / `[moved]` / `[gone]`
//! rather than silently pointing at a node the design has moved out from under.
//! This is the "auto-deprecate on path change" idea, made explicit and cheap.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::cache::{CacheDir, CacheNode};

pub const MARKS_SCHEMA_VERSION: u32 = 1;

const MARKS_FILENAME: &str = "marks.json";
const MARKS_LOCKFILE: &str = "marks.lock";

/// The whole mark table. `marks` is kept sorted by `key` so `mark list` and the
/// on-disk file are stable/diffable regardless of insertion order.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MarkStore {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub marks: Vec<Mark>,
}

/// One named entity. `key` is the primary handle (`mark:<key>`); `aliases` and
/// `note` widen what a fuzzy search will match it on. A mark can point at more
/// than one node (e.g. the same component instanced on several screens).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mark {
    pub key: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub nodes: Vec<MarkNode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One node a mark points at, in machine-portable native ids plus the staleness
/// [`Stamp`] captured when it was added.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MarkNode {
    pub file_key: String,
    pub node_id: String,
    pub stamp: Stamp,
}

/// What a node looked like when marked, for drift detection. `path` is the
/// ancestor names root → parent (excluding the node itself), so a moved or
/// renamed-ancestor node reads differently from a `Fresh` one.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stamp {
    pub name: String,
    pub path: Vec<String>,
    pub at_epoch: u64,
}

/// Result of comparing a stamp against the current cached document. Ordered by
/// how much intervention the mark needs, worst first, so `mark list` can flag
/// the single most-significant drift per node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// Name and ancestor path both still match.
    Fresh,
    /// Node still exists at the same place but was renamed.
    Renamed { current: String },
    /// Node still exists (same id, same name) but its ancestor path changed.
    Moved,
    /// No node with this id in the current cached document.
    Gone,
    /// The file isn't cached, so freshness can't be determined.
    Uncached,
}

impl Freshness {
    /// Short bracketed flag for list output, or `None` when `Fresh`.
    pub fn flag(&self) -> Option<String> {
        match self {
            Freshness::Fresh => None,
            Freshness::Renamed { current } => Some(format!("[renamed → \"{current}\"]")),
            Freshness::Moved => Some("[moved]".to_owned()),
            Freshness::Gone => Some("[gone]".to_owned()),
            Freshness::Uncached => Some("[uncached]".to_owned()),
        }
    }

    /// Machine-readable tag for JSON output.
    pub fn tag(&self) -> &'static str {
        match self {
            Freshness::Fresh => "fresh",
            Freshness::Renamed { .. } => "renamed",
            Freshness::Moved => "moved",
            Freshness::Gone => "gone",
            Freshness::Uncached => "uncached",
        }
    }
}

fn default_version() -> u32 {
    MARKS_SCHEMA_VERSION
}

impl MarkStore {
    /// Read from `<cache-root>/marks.json`. Missing file → empty store (first
    /// run). Schema mismatch is hard — refuse rather than misinterpret.
    pub fn load(cache_dir: &CacheDir) -> Result<Self> {
        let path = marks_path(cache_dir);
        if !path.exists() {
            return Ok(Self::default_versioned());
        }
        let s = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let store: MarkStore =
            serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))?;
        if store.version != MARKS_SCHEMA_VERSION {
            anyhow::bail!(
                "marks schema mismatch: file is v{}, build supports v{}. \
                 Delete {} to reset or migrate manually.",
                store.version,
                MARKS_SCHEMA_VERSION,
                path.display()
            );
        }
        Ok(store)
    }

    /// Best-effort load for read-only consumers (`find`, `library search`): a
    /// missing or corrupt store degrades to "no marks" with a stderr warning
    /// rather than aborting the host command.
    pub fn load_lenient(cache_dir: &CacheDir) -> Self {
        match Self::load(cache_dir) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("marks: ignoring unreadable marks.json ({e:#})");
                Self::default_versioned()
            }
        }
    }

    fn default_versioned() -> Self {
        Self {
            version: MARKS_SCHEMA_VERSION,
            marks: Vec::new(),
        }
    }

    /// Atomic write via tempfile+rename. Callers must hold `marks.lock`; use
    /// [`with_lock`] to get that for free.
    pub fn save(&self, cache_dir: &CacheDir) -> Result<()> {
        cache_dir.ensure()?;
        let path = marks_path(cache_dir);
        let bytes = serde_json::to_vec_pretty(self)?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)
            .with_context(|| format!("creating tempfile in {}", parent.display()))?;
        tmp.write_all(&bytes)
            .with_context(|| format!("writing tempfile for {}", path.display()))?;
        tmp.persist(&path)
            .map_err(|e| anyhow::anyhow!("persisting {}: {}", path.display(), e))?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&Mark> {
        self.marks.iter().find(|m| m.key == key)
    }

    /// Insert a new mark or replace the existing one with the same key, keeping
    /// `marks` sorted by key.
    pub fn upsert(&mut self, mark: Mark) {
        match self.marks.binary_search_by(|m| m.key.cmp(&mark.key)) {
            Ok(i) => self.marks[i] = mark,
            Err(i) => self.marks.insert(i, mark),
        }
    }

    /// Remove a mark by key. Returns whether one was removed.
    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.marks.len();
        self.marks.retain(|m| m.key != key);
        self.marks.len() != before
    }

    /// All keys, for "did you mean" hints on an unknown-key lookup.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.marks.iter().map(|m| m.key.as_str())
    }
}

fn marks_path(cache_dir: &CacheDir) -> PathBuf {
    cache_dir.root.join(MARKS_FILENAME)
}

fn lock_path(cache_dir: &CacheDir) -> PathBuf {
    cache_dir.root.join(MARKS_LOCKFILE)
}

/// Acquire `marks.lock` exclusively, load the store, run `f`, and — only if `f`
/// returns `Ok` — save. A failing mutation (e.g. `mark add` hitting a key
/// collision) leaves the file untouched instead of rewriting it. Mirrors
/// [`crate::synth::with_lock`] but threads a `Result` through so the save is
/// conditional.
pub fn with_lock<T, F>(cache_dir: &CacheDir, f: F) -> Result<T>
where
    F: FnOnce(&mut MarkStore) -> Result<T>,
{
    cache_dir.ensure()?;
    let lock_file = open_lockfile(&lock_path(cache_dir))?;
    lock_file
        .lock_exclusive()
        .context("acquiring exclusive lock on marks.lock")?;
    let result = (|| -> Result<T> {
        let mut store = MarkStore::load(cache_dir)?;
        let r = f(&mut store)?;
        store.save(cache_dir)?;
        Ok(r)
    })();
    let _ = fs2::FileExt::unlock(&lock_file);
    result
}

fn open_lockfile(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("opening lockfile {}", path.display()))
}

/// Validate a mark key: non-empty, and only `[A-Za-z0-9._-]`. No `:` (so
/// `mark:<key>` round-trips through the id grammar) and no whitespace.
pub fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Depth-first search for `node_id`, returning the node and the ancestor-name
/// path (root → parent, excluding the node itself, empty names skipped). One
/// walker shared by stamp capture and [`check_freshness`] so a `[moved]` flag
/// can never fire from a formatting difference between the two.
pub fn find_node_with_path<'a>(
    root: &'a CacheNode,
    node_id: &str,
) -> Option<(&'a CacheNode, Vec<String>)> {
    fn walk<'a>(
        node: &'a CacheNode,
        target: &str,
        path: &mut Vec<String>,
    ) -> Option<(&'a CacheNode, Vec<String>)> {
        if node.id == target {
            return Some((node, path.clone()));
        }
        if !node.name.is_empty() {
            path.push(node.name.clone());
        } else {
            // Keep the push/pop balanced without polluting the path with the
            // empty names Figma gives auto-generated wrappers.
            path.push(String::new());
        }
        for child in &node.children {
            if let Some(hit) = walk(child, target, path) {
                return Some(hit);
            }
        }
        path.pop();
        None
    }
    let mut path = Vec::new();
    walk(root, node_id, &mut path)
        .map(|(n, p)| (n, p.into_iter().filter(|s| !s.is_empty()).collect()))
}

/// Compare a stamp against the current cached document. `document` is the file's
/// root [`CacheNode`], or `None` when the file isn't cached (→ [`Freshness::Uncached`]).
pub fn check_freshness(document: Option<&CacheNode>, node_id: &str, stamp: &Stamp) -> Freshness {
    let Some(root) = document else {
        return Freshness::Uncached;
    };
    let Some((node, path)) = find_node_with_path(root, node_id) else {
        return Freshness::Gone;
    };
    if node.name != stamp.name {
        return Freshness::Renamed {
            current: node.name.clone(),
        };
    }
    if path != stamp.path {
        return Freshness::Moved;
    }
    Freshness::Fresh
}

// ─────────────────────────────────────────────────────────────────────────────
// Search + render — the payoff folded into `find` and `library search`
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Matcher, Utf32Str,
};
use serde_json::{json, Value};

use crate::synth::SynthState;
use crate::tree::{truncate_display, NAME_DISPLAY_MAX};

/// A mark that matched a query, with per-node freshness resolved against the
/// current cache. Ordered best-score-first by [`search_marks`].
#[derive(Clone, Debug)]
pub struct ScoredMarkView {
    pub key: String,
    pub score: u32,
    pub note: Option<String>,
    pub nodes: Vec<MarkNodeView>,
}

/// One node of a matched mark, resolved for display: the paste-ready `file:N`
/// id (when the source file is interned), the marked name, and drift status.
#[derive(Clone, Debug)]
pub struct MarkNodeView {
    pub file_key: String,
    pub node_id: String,
    pub name: String,
    pub freshness: Freshness,
    /// `file:N:node_id` when the source file has a synth; `None` otherwise.
    pub synth_id: Option<String>,
    /// When this node was stamped (from [`Stamp::at_epoch`]) — for the
    /// `added <age>` column in `mark list`.
    pub at_epoch: u64,
}

impl MarkNodeView {
    /// The paste-ready id: the synth `file:N:node_id` when known, else the raw
    /// `file_key:node_id` so the user can still chase it down.
    pub fn display_id(&self) -> String {
        self.synth_id
            .clone()
            .unwrap_or_else(|| format!("{}:{}", self.file_key, self.node_id))
    }
}

/// The text a mark is fuzzy-matched on: its key, every alias, and its note.
/// Concatenated so a query word hits whichever field carries the vocabulary.
fn mark_haystack(mark: &Mark) -> String {
    let mut h = mark.key.clone();
    for a in &mark.aliases {
        h.push(' ');
        h.push_str(a);
    }
    if let Some(note) = &mark.note {
        h.push(' ');
        h.push_str(note);
    }
    h
}

/// Fuzzy-rank marks against `query` over "key + aliases + note", resolving each
/// matched mark's nodes to a paste-ready id and freshness. I/O (reading cached
/// payloads for freshness) happens only for marks that matched, and payload
/// reads are memoized per file_key within the call.
///
/// Filtering to a scope is the caller's job: pass a `store` already narrowed to
/// one file's marks (`find --in file:N`), or the whole store.
pub fn search_marks(
    store: &MarkStore,
    query: &str,
    cache: &CacheDir,
    synth: &SynthState,
) -> Vec<ScoredMarkView> {
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf: Vec<char> = Vec::new();
    // Memoize the (expensive) rkyv payload read per file across all matched
    // marks and all of a mark's nodes.
    let mut docs: HashMap<String, Option<CacheNode>> = HashMap::new();

    let mut out: Vec<ScoredMarkView> = Vec::new();
    for mark in &store.marks {
        let haystack = mark_haystack(mark);
        buf.clear();
        let Some(score) = pattern.score(Utf32Str::new(&haystack, &mut buf), &mut matcher) else {
            continue;
        };
        out.push(resolve_mark_view(mark, score, &mut docs, cache, synth));
    }
    out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.key.cmp(&b.key)));
    out
}

/// Resolve *every* mark to a view (freshness + ids), preserving the store's
/// key-sorted order. Used by `mark list`; `score` is a meaningless 0 there.
pub fn all_views(store: &MarkStore, cache: &CacheDir, synth: &SynthState) -> Vec<ScoredMarkView> {
    let mut docs: HashMap<String, Option<CacheNode>> = HashMap::new();
    store
        .marks
        .iter()
        .map(|mark| resolve_mark_view(mark, 0, &mut docs, cache, synth))
        .collect()
}

/// Build one mark's view: resolve each node's freshness (against the memoized
/// per-file document) and its paste-ready id.
fn resolve_mark_view(
    mark: &Mark,
    score: u32,
    docs: &mut HashMap<String, Option<CacheNode>>,
    cache: &CacheDir,
    synth: &SynthState,
) -> ScoredMarkView {
    let nodes = mark
        .nodes
        .iter()
        .map(|mn| {
            let doc = docs.entry(mn.file_key.clone()).or_insert_with(|| {
                cache
                    .read_file(&mn.file_key)
                    .ok()
                    .flatten()
                    .map(|cf| cf.document)
            });
            let freshness = check_freshness(doc.as_ref(), &mn.node_id, &mn.stamp);
            let synth_id = synth
                .file_synth(&mn.file_key)
                .map(|n| format!("file:{n}:{}", mn.node_id));
            MarkNodeView {
                file_key: mn.file_key.clone(),
                node_id: mn.node_id.clone(),
                name: mn.stamp.name.clone(),
                freshness,
                synth_id,
                at_epoch: mn.stamp.at_epoch,
            }
        })
        .collect();
    ScoredMarkView {
        key: mark.key.clone(),
        score,
        note: mark.note.clone(),
        nodes,
    }
}

/// Render matched marks as ★-prefixed rows, one per (mark, node), for the YAML
/// output of `find` / `library search`. Emitted *before* the command's own
/// hits so a marked entity always leads. Returns `""` when there are none.
pub fn render_mark_rows(views: &[ScoredMarkView]) -> String {
    if views.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for v in views {
        for n in &v.nodes {
            let flag = n
                .freshness
                .flag()
                .map(|f| format!("  {f}"))
                .unwrap_or_default();
            let note = v
                .note
                .as_deref()
                .map(|s| format!("  — {}", truncate_display(s, NAME_DISPLAY_MAX)))
                .unwrap_or_default();
            out.push_str(&format!(
                "★ mark:{key}  {id}  | \"{name}\"{flag}{note}\n",
                key = v.key,
                id = n.display_id(),
                name = truncate_display(&n.name, NAME_DISPLAY_MAX),
                flag = flag,
                note = note,
            ));
        }
    }
    out
}

/// JSON projection of matched marks for the `--json` envelopes.
pub fn marks_json(views: &[ScoredMarkView]) -> Vec<Value> {
    views
        .iter()
        .map(|v| {
            json!({
                "key": v.key,
                "score": v.score,
                "note": v.note,
                "nodes": v.nodes.iter().map(|n| json!({
                    "id": n.display_id(),
                    "synth_id": n.synth_id,
                    "file_key": n.file_key,
                    "node_id": n.node_id,
                    "name": n.name,
                    "freshness": n.freshness.tag(),
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_cache_dir() -> (tempfile::TempDir, CacheDir) {
        let tmp = tempfile::tempdir().unwrap();
        let cache = CacheDir::new(tmp.path());
        cache.ensure().unwrap();
        (tmp, cache)
    }

    fn node(id: &str, name: &str, children: Vec<CacheNode>) -> CacheNode {
        CacheNode {
            id: id.into(),
            type_: "FRAME".into(),
            name: name.into(),
            visible: true,
            bounds: None,
            children,
        }
    }

    fn stamp(name: &str, path: &[&str]) -> Stamp {
        Stamp {
            name: name.into(),
            path: path.iter().map(|s| (*s).to_string()).collect(),
            at_epoch: 100,
        }
    }

    // ── store round-trip / schema ────────────────────────────────────────

    #[test]
    fn load_returns_empty_on_first_run() {
        let (_g, cache) = tmp_cache_dir();
        let s = MarkStore::load(&cache).unwrap();
        assert!(s.marks.is_empty());
        assert_eq!(s.version, MARKS_SCHEMA_VERSION);
    }

    #[test]
    fn round_trip_via_save_and_load() {
        let (_g, cache) = tmp_cache_dir();
        let mut s = MarkStore::default_versioned();
        s.upsert(Mark {
            key: "wallchart".into(),
            aliases: vec!["leave tooltip".into()],
            nodes: vec![MarkNode {
                file_key: "F".into(),
                node_id: "1:2".into(),
                stamp: stamp("Cell", &["Home", "Grid"]),
            }],
            note: Some("hover card".into()),
        });
        s.save(&cache).unwrap();
        assert_eq!(MarkStore::load(&cache).unwrap(), s);
    }

    #[test]
    fn schema_version_mismatch_errors() {
        let (_g, cache) = tmp_cache_dir();
        fs::write(marks_path(&cache), r#"{"version": 99, "marks": []}"#).unwrap();
        let err = MarkStore::load(&cache).unwrap_err().to_string();
        assert!(err.contains("schema mismatch"), "got: {err}");
    }

    #[test]
    fn load_lenient_swallows_corruption() {
        let (_g, cache) = tmp_cache_dir();
        fs::write(marks_path(&cache), "{ not json").unwrap();
        // Must not panic or error — degrades to empty.
        assert!(MarkStore::load_lenient(&cache).marks.is_empty());
    }

    // ── upsert / remove / ordering ───────────────────────────────────────

    #[test]
    fn upsert_keeps_keys_sorted_and_replaces() {
        let mut s = MarkStore::default_versioned();
        for k in ["zebra", "alpha", "mid"] {
            s.upsert(Mark {
                key: k.into(),
                aliases: vec![],
                nodes: vec![],
                note: None,
            });
        }
        assert_eq!(
            s.keys().collect::<Vec<_>>(),
            vec!["alpha", "mid", "zebra"],
            "kept sorted"
        );
        // Re-upsert replaces, doesn't duplicate.
        s.upsert(Mark {
            key: "mid".into(),
            aliases: vec!["x".into()],
            nodes: vec![],
            note: None,
        });
        assert_eq!(s.marks.len(), 3);
        assert_eq!(s.get("mid").unwrap().aliases, vec!["x".to_string()]);
    }

    #[test]
    fn remove_reports_whether_present() {
        let mut s = MarkStore::default_versioned();
        s.upsert(Mark {
            key: "a".into(),
            aliases: vec![],
            nodes: vec![],
            note: None,
        });
        assert!(s.remove("a"));
        assert!(!s.remove("a"));
    }

    // ── with_lock save-only-on-Ok ────────────────────────────────────────

    #[test]
    fn with_lock_saves_on_ok() {
        let (_g, cache) = tmp_cache_dir();
        with_lock(&cache, |s| {
            s.upsert(Mark {
                key: "k".into(),
                aliases: vec![],
                nodes: vec![],
                note: None,
            });
            Ok(())
        })
        .unwrap();
        assert!(MarkStore::load(&cache).unwrap().get("k").is_some());
    }

    #[test]
    fn with_lock_does_not_save_on_err() {
        let (_g, cache) = tmp_cache_dir();
        let r: Result<()> = with_lock(&cache, |s| {
            s.upsert(Mark {
                key: "k".into(),
                aliases: vec![],
                nodes: vec![],
                note: None,
            });
            anyhow::bail!("simulated collision")
        });
        assert!(r.is_err());
        // The mutation must not have been persisted.
        assert!(MarkStore::load(&cache).unwrap().get("k").is_none());
    }

    // ── key validation ───────────────────────────────────────────────────

    #[test]
    fn key_validation_table() {
        assert!(is_valid_key("wallchart-cell.dropdown_2"));
        assert!(is_valid_key("A1"));
        assert!(!is_valid_key(""), "empty rejected");
        assert!(
            !is_valid_key("has:colon"),
            "colon rejected (breaks mark:<key>)"
        );
        assert!(!is_valid_key("has space"), "whitespace rejected");
        assert!(!is_valid_key("emoji😀"), "non-ascii rejected");
    }

    // ── find_node_with_path ──────────────────────────────────────────────

    #[test]
    fn find_node_with_path_excludes_self_and_skips_empty_names() {
        let root = node(
            "0:0",
            "doc",
            vec![node(
                "1:0",
                "", // empty wrapper name — should be skipped in the path
                vec![node("1:1", "Grid", vec![node("1:2", "Cell", vec![])])],
            )],
        );
        let (found, path) = find_node_with_path(&root, "1:2").unwrap();
        assert_eq!(found.name, "Cell");
        // Path is doc → (skipped empty) → Grid, excluding "Cell" itself.
        assert_eq!(path, vec!["doc".to_string(), "Grid".to_string()]);
    }

    #[test]
    fn find_node_with_path_missing_is_none() {
        let root = node("0:0", "doc", vec![]);
        assert!(find_node_with_path(&root, "9:9").is_none());
    }

    // ── freshness, all five variants ─────────────────────────────────────

    fn doc() -> CacheNode {
        node(
            "0:0",
            "doc",
            vec![node("1:0", "Home", vec![node("1:1", "Cell", vec![])])],
        )
    }

    #[test]
    fn freshness_fresh_when_name_and_path_match() {
        let f = check_freshness(Some(&doc()), "1:1", &stamp("Cell", &["doc", "Home"]));
        assert_eq!(f, Freshness::Fresh);
    }

    #[test]
    fn freshness_renamed_when_name_differs() {
        let f = check_freshness(Some(&doc()), "1:1", &stamp("Old Name", &["doc", "Home"]));
        assert_eq!(
            f,
            Freshness::Renamed {
                current: "Cell".into()
            }
        );
    }

    #[test]
    fn freshness_moved_when_path_differs() {
        let f = check_freshness(Some(&doc()), "1:1", &stamp("Cell", &["doc", "Away"]));
        assert_eq!(f, Freshness::Moved);
    }

    #[test]
    fn freshness_gone_when_id_absent() {
        let f = check_freshness(Some(&doc()), "9:9", &stamp("Cell", &["doc", "Home"]));
        assert_eq!(f, Freshness::Gone);
    }

    #[test]
    fn freshness_uncached_when_no_document() {
        let f = check_freshness(None, "1:1", &stamp("Cell", &["doc", "Home"]));
        assert_eq!(f, Freshness::Uncached);
    }

    #[test]
    fn freshness_flags_and_tags() {
        assert_eq!(Freshness::Fresh.flag(), None);
        assert_eq!(Freshness::Moved.flag().as_deref(), Some("[moved]"));
        assert_eq!(Freshness::Gone.tag(), "gone");
        assert_eq!(
            Freshness::Renamed {
                current: "X".into()
            }
            .flag()
            .as_deref(),
            Some("[renamed → \"X\"]")
        );
    }

    // ── search_marks + render ────────────────────────────────────────────

    fn cached_file(cache: &CacheDir, file_key: &str, document: CacheNode) {
        let payload = crate::cache::CachedFile {
            file_key: file_key.into(),
            name: "F".into(),
            project_id: "P".into(),
            project_name: "PN".into(),
            last_modified: "2026-01-01T00:00:00Z".into(),
            cached_at_epoch: 0,
            node_count: 1,
            document,
        };
        cache.write_file(file_key, &payload).unwrap();
    }

    fn mark(key: &str, aliases: &[&str], note: Option<&str>, nodes: Vec<MarkNode>) -> Mark {
        Mark {
            key: key.into(),
            aliases: aliases.iter().map(|s| (*s).to_string()).collect(),
            nodes,
            note: note.map(|s| s.to_owned()),
        }
    }

    #[test]
    fn search_marks_matches_key_alias_and_note() {
        let (_g, cache) = tmp_cache_dir();
        let synth = SynthState::default();
        let mut store = MarkStore::default_versioned();
        store.upsert(mark(
            "wallchart-cell",
            &["leave tooltip"],
            Some("hover card on a cell"),
            vec![MarkNode {
                file_key: "F".into(),
                node_id: "1:1".into(),
                stamp: stamp("Cell", &["Home"]),
            }],
        ));
        // Matches on the alias vocabulary…
        assert_eq!(search_marks(&store, "tooltip", &cache, &synth).len(), 1);
        // …on the note…
        assert_eq!(search_marks(&store, "hover", &cache, &synth).len(), 1);
        // …on the key…
        assert_eq!(search_marks(&store, "wallchart", &cache, &synth).len(), 1);
        // …and misses unrelated queries.
        assert!(search_marks(&store, "zzunrelated", &cache, &synth).is_empty());
    }

    #[test]
    fn search_marks_orders_by_score_then_key() {
        let (_g, cache) = tmp_cache_dir();
        let synth = SynthState::default();
        let mut store = MarkStore::default_versioned();
        // Both contain "button"; exact-word "button" should outrank "button-bar".
        store.upsert(mark("button-bar", &[], None, vec![]));
        store.upsert(mark("button", &[], None, vec![]));
        let hits = search_marks(&store, "button", &cache, &synth);
        assert_eq!(hits[0].key, "button", "closer match ranks first");
    }

    #[test]
    fn search_marks_reports_uncached_then_fresh_after_prefetch() {
        let (_g, cache) = tmp_cache_dir();
        let mut synth = SynthState::default();
        synth.intern_file("F"); // so the view carries a paste-ready file:1 id
        let mut store = MarkStore::default_versioned();
        store.upsert(mark(
            "cell",
            &[],
            None,
            vec![MarkNode {
                file_key: "F".into(),
                node_id: "1:1".into(),
                stamp: stamp("Cell", &["doc", "Home"]),
            }],
        ));

        // No file cached yet → Uncached, but the synth id is still resolved.
        let before = search_marks(&store, "cell", &cache, &synth);
        assert_eq!(before[0].nodes[0].freshness, Freshness::Uncached);
        assert_eq!(before[0].nodes[0].synth_id.as_deref(), Some("file:1:1:1"));

        // Cache the file with a matching document → Fresh.
        cached_file(
            &cache,
            "F",
            node(
                "0:0",
                "doc",
                vec![node("1:0", "Home", vec![node("1:1", "Cell", vec![])])],
            ),
        );
        let after = search_marks(&store, "cell", &cache, &synth);
        assert_eq!(after[0].nodes[0].freshness, Freshness::Fresh);
    }

    #[test]
    fn render_mark_rows_prefixes_star_and_shows_flag_and_note() {
        let views = vec![ScoredMarkView {
            key: "cell".into(),
            score: 100,
            note: Some("hover card".into()),
            nodes: vec![MarkNodeView {
                file_key: "F".into(),
                node_id: "1:1".into(),
                name: "Cell".into(),
                freshness: Freshness::Moved,
                synth_id: Some("file:1:1:1".into()),
                at_epoch: 0,
            }],
        }];
        let out = render_mark_rows(&views);
        assert!(out.starts_with("★ mark:cell  file:1:1:1"), "got: {out}");
        assert!(out.contains("[moved]"));
        assert!(out.contains("— hover card"));
        assert!(render_mark_rows(&[]).is_empty());
    }

    #[test]
    fn display_id_falls_back_to_raw_key() {
        let n = MarkNodeView {
            file_key: "FK".into(),
            node_id: "9:9".into(),
            name: "X".into(),
            freshness: Freshness::Fresh,
            synth_id: None,
            at_epoch: 0,
        };
        assert_eq!(n.display_id(), "FK:9:9");
        let synthed = MarkNodeView {
            synth_id: Some("file:2:9:9".into()),
            ..n
        };
        assert_eq!(synthed.display_id(), "file:2:9:9");
    }
}

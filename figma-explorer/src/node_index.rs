//! Reverse lookup: native Figma node id → file synth(s) containing it.
//!
//! Used by the resolver when a bare `x:y` ID is passed to a command (no file
//! scope). We scan every cached file once at build time, walk its archived
//! `CacheNode` tree, and record which file synths each node id appears in.
//!
//! Collisions are inherent — Figma node IDs are file-scoped, so e.g. `0:0`
//! (the DOCUMENT root) appears in every cached file. The resolver consults
//! this map: 1 hit → use it; N hits → ambiguity error with candidates; 0
//! hits → "not found, try a URL".
//!
//! Persisted as a sidecar (`node_index.bin`) keyed by a fingerprint of
//! `(file_synth, file_key, last_modified)` across every `EntryStatus::Ok`
//! meta. If the fingerprint matches on load, we return the persisted index
//! without re-walking any payloads; otherwise we rebuild from scratch and
//! overwrite the sidecar. Sidecar I/O is best-effort — a failure to read or
//! write the sidecar falls back to the in-memory build.
//!
//! On-disk layout mirrors the structural cache:
//! `[4-byte magic "FXN\0"][u32 LE schema version][rkyv body]`.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rkyv::rancor;

use crate::cache::{self, CacheDir, CacheNode, EntryStatus, FileMeta};
use crate::synth::SynthState;

const NODE_INDEX_MAGIC: [u8; 4] = *b"FXN\0";
const NODE_INDEX_SCHEMA_VERSION: u32 = 1;
const NODE_INDEX_HEADER_LEN: usize = 8;
const NODE_INDEX_FILENAME: &str = "node_index.bin";

/// `node_id` → list of `file_synth` values containing that id (in unspecified
/// order). Stored as `Vec<u32>` because most node ids occur in exactly one
/// file; only a small handful (`0:0`, low-x canvases) appear in many.
#[derive(Clone, Debug, Default)]
pub struct NodeIndex {
    by_node_id: HashMap<String, Vec<u32>>,
}

/// rkyv-archivable storage shape for the sidecar. `HashMap` would require
/// rkyv's hashbrown feature; the `Vec<(String, Vec<u32>)>` form serializes
/// cleanly and round-trips into a HashMap on load.
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Debug)]
struct PersistedNodeIndex {
    /// Hash of `(file_synth, file_key, last_modified)` tuples sorted by
    /// file_key, plus the schema version. Any drift triggers a rebuild.
    fingerprint: u64,
    entries: Vec<PersistedEntry>,
}

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Debug)]
struct PersistedEntry {
    node_id: String,
    file_synths: Vec<u32>,
}

impl NodeIndex {
    /// Load the index from `<cache>/node_index.bin` if its fingerprint matches
    /// the current cache state; otherwise rebuild from scratch and persist.
    ///
    /// Sidecar persistence is best-effort: read errors → silent rebuild;
    /// write errors → log to stderr and keep going (the in-memory index is
    /// still correct for this invocation).
    pub fn load_or_build(cache_dir: &CacheDir, synth: &SynthState) -> Result<Self> {
        let metas = cache_dir
            .list_metas()
            .context("listing cache metas for index build")?;
        let fingerprint = compute_fingerprint(&metas, synth);

        let path = node_index_path(cache_dir);
        if path.exists() {
            if let Ok(Some(persisted)) = read_sidecar(&path) {
                if persisted.fingerprint == fingerprint {
                    return Ok(Self::from_persisted(persisted));
                }
            }
        }

        let index = Self::build_from_metas(cache_dir, synth, &metas);
        if let Err(e) = write_sidecar(&path, fingerprint, &index) {
            eprintln!(
                "node_index: failed to persist sidecar at {} ({e:#}); keeping in-memory only",
                path.display()
            );
        }
        Ok(index)
    }

    /// In-memory build, used by `load_or_build` on cache miss and by tests.
    fn build_from_metas(cache_dir: &CacheDir, synth: &SynthState, metas: &[FileMeta]) -> Self {
        let mut by_node_id: HashMap<String, Vec<u32>> = HashMap::new();
        for m in metas {
            if m.status != EntryStatus::Ok {
                continue;
            }
            let Some(file_synth) = synth.file_synth(&m.file_key) else {
                continue;
            };
            let payload = match cache_dir.read_file(&m.file_key) {
                Ok(Some(p)) => p,
                Ok(None) => continue,
                Err(e) => {
                    eprintln!("node_index: skipping {} ({}): {e}", m.file_key, m.name);
                    continue;
                }
            };
            walk_collect(&payload.document, file_synth, &mut by_node_id);
        }
        Self { by_node_id }
    }

    fn from_persisted(p: PersistedNodeIndex) -> Self {
        let by_node_id = p
            .entries
            .into_iter()
            .map(|e| (e.node_id, e.file_synths))
            .collect();
        Self { by_node_id }
    }

    /// Look up which cached files contain a given native node id.
    pub fn lookup(&self, node_id: &str) -> &[u32] {
        self.by_node_id
            .get(node_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Total number of distinct node IDs in the index.
    pub fn len(&self) -> usize {
        self.by_node_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_node_id.is_empty()
    }
}

fn walk_collect(node: &CacheNode, file_synth: u32, out: &mut HashMap<String, Vec<u32>>) {
    if !node.id.is_empty() {
        let entry = out.entry(node.id.clone()).or_default();
        // Avoid recording the same (id, file) twice in the unlikely case a
        // file references the same node id in multiple branches.
        if !entry.contains(&file_synth) {
            entry.push(file_synth);
        }
    }
    for c in &node.children {
        walk_collect(c, file_synth, out);
    }
}

fn node_index_path(cache_dir: &CacheDir) -> PathBuf {
    cache_dir.root.join(NODE_INDEX_FILENAME)
}

fn compute_fingerprint(metas: &[FileMeta], synth: &SynthState) -> u64 {
    let mut entries: Vec<(&str, u32, &str)> = metas
        .iter()
        .filter(|m| m.status == EntryStatus::Ok)
        .filter_map(|m| {
            synth
                .file_synth(&m.file_key)
                .map(|s| (m.file_key.as_str(), s, m.last_modified.as_str()))
        })
        .collect();
    entries.sort();
    let mut hasher = DefaultHasher::new();
    NODE_INDEX_SCHEMA_VERSION.hash(&mut hasher);
    for (k, s, lm) in entries {
        k.hash(&mut hasher);
        s.hash(&mut hasher);
        lm.hash(&mut hasher);
    }
    hasher.finish()
}

fn read_sidecar(path: &Path) -> Result<Option<PersistedNodeIndex>> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() < NODE_INDEX_HEADER_LEN {
        return Ok(None);
    }
    if bytes[..4] != NODE_INDEX_MAGIC {
        return Ok(None);
    }
    let mut ver_bytes = [0u8; 4];
    ver_bytes.copy_from_slice(&bytes[4..8]);
    let ver = u32::from_le_bytes(ver_bytes);
    if ver != NODE_INDEX_SCHEMA_VERSION {
        return Ok(None);
    }
    match rkyv::from_bytes::<PersistedNodeIndex, rancor::Error>(&bytes[NODE_INDEX_HEADER_LEN..]) {
        Ok(p) => Ok(Some(p)),
        Err(_) => Ok(None),
    }
}

fn write_sidecar(path: &Path, fingerprint: u64, idx: &NodeIndex) -> Result<()> {
    let persisted = PersistedNodeIndex {
        fingerprint,
        entries: idx
            .by_node_id
            .iter()
            .map(|(k, v)| PersistedEntry {
                node_id: k.clone(),
                file_synths: v.clone(),
            })
            .collect(),
    };
    let body = rkyv::to_bytes::<rancor::Error>(&persisted)
        .map_err(|e| anyhow::anyhow!("rkyv serialize node_index: {e}"))?;
    let mut out = Vec::with_capacity(NODE_INDEX_HEADER_LEN + body.len());
    out.extend_from_slice(&NODE_INDEX_MAGIC);
    out.extend_from_slice(&NODE_INDEX_SCHEMA_VERSION.to_le_bytes());
    out.extend_from_slice(&body);
    cache::atomic_write(path, &out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{build_cached_file, CacheDir, EntryStatus, FileMeta, FileRef};
    use serde_json::json;

    fn fixture_cache_with_two_files() -> (tempfile::TempDir, CacheDir, SynthState) {
        let tmp = tempfile::tempdir().unwrap();
        let cache = CacheDir::new(tmp.path());
        cache.ensure().unwrap();

        // File A: DOCUMENT 0:0, page 0:1 "Cover", frame 1:2 "Header".
        let doc_a = json!({
            "id": "0:0", "name": "doc", "type": "DOCUMENT",
            "children": [{
                "id": "0:1", "name": "Cover", "type": "CANVAS",
                "children": [{
                    "id": "1:2", "name": "Header", "type": "FRAME"
                }]
            }]
        });
        let ref_a = FileRef {
            file_key: "file-a".into(),
            name: "A".into(),
            last_modified: "2024-01-01".into(),
            project_id: "p1".into(),
            project_name: "P".into(),
        };
        let payload_a = build_cached_file(&ref_a, &doc_a, 0);
        cache.write_file("file-a", &payload_a).unwrap();
        cache
            .write_meta(&FileMeta::from_success(&ref_a, &payload_a, 0, 0))
            .unwrap();

        // File B: DOCUMENT 0:0, page 0:1 "Sheet" (id collision with A's 0:1!),
        // unique frame 9:9 "Banner".
        let doc_b = json!({
            "id": "0:0", "name": "doc", "type": "DOCUMENT",
            "children": [{
                "id": "0:1", "name": "Sheet", "type": "CANVAS",
                "children": [{
                    "id": "9:9", "name": "Banner", "type": "FRAME"
                }]
            }]
        });
        let ref_b = FileRef {
            file_key: "file-b".into(),
            name: "B".into(),
            last_modified: "2024-01-01".into(),
            project_id: "p1".into(),
            project_name: "P".into(),
        };
        let payload_b = build_cached_file(&ref_b, &doc_b, 0);
        cache.write_file("file-b", &payload_b).unwrap();
        cache
            .write_meta(&FileMeta::from_success(&ref_b, &payload_b, 0, 0))
            .unwrap();

        let mut synth = SynthState::default();
        let a = synth.intern_file("file-a");
        let b = synth.intern_file("file-b");
        assert_eq!(a, 1);
        assert_eq!(b, 2);

        (tmp, cache, synth)
    }

    #[test]
    fn unique_id_resolves_to_single_file() {
        let (_g, cache, synth) = fixture_cache_with_two_files();
        let idx = NodeIndex::load_or_build(&cache, &synth).unwrap();
        assert_eq!(idx.lookup("1:2"), &[1]);
        assert_eq!(idx.lookup("9:9"), &[2]);
    }

    #[test]
    fn shared_id_returns_all_files() {
        let (_g, cache, synth) = fixture_cache_with_two_files();
        let idx = NodeIndex::load_or_build(&cache, &synth).unwrap();
        let mut got_zero_zero = idx.lookup("0:0").to_vec();
        got_zero_zero.sort_unstable();
        assert_eq!(got_zero_zero, vec![1, 2]);
        let mut got_zero_one = idx.lookup("0:1").to_vec();
        got_zero_one.sort_unstable();
        assert_eq!(got_zero_one, vec![1, 2]);
    }

    #[test]
    fn unknown_id_returns_empty() {
        let (_g, cache, synth) = fixture_cache_with_two_files();
        let idx = NodeIndex::load_or_build(&cache, &synth).unwrap();
        assert!(idx.lookup("999:999").is_empty());
    }

    #[test]
    fn skips_files_without_synth() {
        let (_g, cache, _synth) = fixture_cache_with_two_files();
        // Empty synth state — no files have synths assigned.
        let empty = SynthState::default();
        let idx = NodeIndex::load_or_build(&cache, &empty).unwrap();
        assert!(idx.is_empty());
    }

    #[test]
    fn skips_files_with_failed_status() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = CacheDir::new(tmp.path());
        cache.ensure().unwrap();
        // Write a meta with status:Failed and no payload. The index must
        // not blow up trying to read the missing file.
        let meta = FileMeta {
            file_key: "fail-key".into(),
            name: "broken".into(),
            project_id: "p".into(),
            project_name: "P".into(),
            last_modified: "2024-01-01".into(),
            cached_at_epoch: 0,
            last_listed_at_epoch: 0,
            status: EntryStatus::Failed,
            error: Some("transient".into()),
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
        cache.write_meta(&meta).unwrap();

        let mut synth = SynthState::default();
        synth.intern_file("fail-key");
        let idx = NodeIndex::load_or_build(&cache, &synth).unwrap();
        assert!(idx.is_empty());
    }

    #[test]
    fn first_build_writes_sidecar() {
        let (_g, cache, synth) = fixture_cache_with_two_files();
        let _ = NodeIndex::load_or_build(&cache, &synth).unwrap();
        let path = node_index_path(&cache);
        assert!(path.exists(), "expected node_index.bin to be written");
        let bytes = fs::read(&path).unwrap();
        assert!(
            bytes.starts_with(&NODE_INDEX_MAGIC),
            "sidecar should carry the FXN magic"
        );
    }

    #[test]
    fn second_load_uses_persisted_sidecar() {
        let (_g, cache, synth) = fixture_cache_with_two_files();
        // First build persists.
        let _ = NodeIndex::load_or_build(&cache, &synth).unwrap();

        // Mutate the payload on disk in a way that *should* change the index
        // — but leave the meta's last_modified alone. The fingerprint matches
        // → we should get the stale persisted index, not a rebuild.
        // (This is the load-from-sidecar evidence: a rebuild would pick up
        // the new node id.)
        let mutant_doc = json!({
            "id": "0:0", "name": "doc", "type": "DOCUMENT",
            "children": [{
                "id": "0:1", "name": "Cover", "type": "CANVAS",
                "children": [{
                    "id": "77:77", "name": "Brand-new node", "type": "FRAME"
                }]
            }]
        });
        let ref_a = FileRef {
            file_key: "file-a".into(),
            name: "A".into(),
            last_modified: "2024-01-01".into(),
            project_id: "p1".into(),
            project_name: "P".into(),
        };
        let mutant_payload = build_cached_file(&ref_a, &mutant_doc, 0);
        cache.write_file("file-a", &mutant_payload).unwrap();

        let idx2 = NodeIndex::load_or_build(&cache, &synth).unwrap();
        // 1:2 is in the persisted index but NOT in the mutant payload. If we
        // loaded from sidecar, we see 1:2. If we rebuilt, we wouldn't.
        assert_eq!(
            idx2.lookup("1:2"),
            &[1],
            "expected persisted index (1:2 lookup) to be loaded, not rebuild"
        );
        assert!(
            idx2.lookup("77:77").is_empty(),
            "rebuild signal — 77:77 leaked from the new payload"
        );
    }

    #[test]
    fn last_modified_change_triggers_rebuild() {
        let (_g, cache, synth) = fixture_cache_with_two_files();
        // Build once + persist.
        let _ = NodeIndex::load_or_build(&cache, &synth).unwrap();

        // Bump file-a's last_modified. Fingerprint mismatch → rebuild from
        // current payloads on disk.
        let mut meta = cache.read_meta("file-a").unwrap().unwrap();
        meta.last_modified = "2024-02-02".into();
        cache.write_meta(&meta).unwrap();

        let idx = NodeIndex::load_or_build(&cache, &synth).unwrap();
        // The rebuild walks the actual payload, which still contains 1:2.
        assert_eq!(idx.lookup("1:2"), &[1]);
    }

    #[test]
    fn corrupt_sidecar_falls_back_to_rebuild() {
        let (_g, cache, synth) = fixture_cache_with_two_files();
        let path = node_index_path(&cache);
        fs::write(&path, b"definitely not a node index").unwrap();

        let idx = NodeIndex::load_or_build(&cache, &synth).unwrap();
        // Rebuild succeeded.
        assert_eq!(idx.lookup("1:2"), &[1]);
        assert_eq!(idx.lookup("9:9"), &[2]);
        // And the corrupt file was overwritten with a valid sidecar.
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.starts_with(&NODE_INDEX_MAGIC));
    }
}

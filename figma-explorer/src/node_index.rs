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
//! This v1 builds the index in memory on demand. With ~1M nodes across a
//! typical cache, the build is O(N) reads + O(N) hashmap inserts. If the
//! ~1s startup cost proves painful, v2 can persist this to a sidecar.

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::cache::{CacheDir, CacheNode, EntryStatus};
use crate::synth::SynthState;

/// `node_id` → list of `file_synth` values containing that id (in unspecified
/// order). Stored as `Vec<u32>` because most node ids occur in exactly one
/// file; only a small handful (`0:0`, low-x canvases) appear in many.
#[derive(Clone, Debug, Default)]
pub struct NodeIndex {
    by_node_id: HashMap<String, Vec<u32>>,
}

impl NodeIndex {
    /// Walk every `status: Ok` cached file in `cache_dir`. For each, look up
    /// its synth from `synth` (skip files with no synth — they won't be
    /// addressable as `file:N` anyway, so indexing them is pointless).
    pub fn build(cache_dir: &CacheDir, synth: &SynthState) -> Result<Self> {
        let mut by_node_id: HashMap<String, Vec<u32>> = HashMap::new();
        let metas = cache_dir
            .list_metas()
            .context("listing cache metas for index build")?;
        for m in &metas {
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
        Ok(Self { by_node_id })
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
        let idx = NodeIndex::build(&cache, &synth).unwrap();
        assert_eq!(idx.lookup("1:2"), &[1]);
        assert_eq!(idx.lookup("9:9"), &[2]);
    }

    #[test]
    fn shared_id_returns_all_files() {
        let (_g, cache, synth) = fixture_cache_with_two_files();
        let idx = NodeIndex::build(&cache, &synth).unwrap();
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
        let idx = NodeIndex::build(&cache, &synth).unwrap();
        assert!(idx.lookup("999:999").is_empty());
    }

    #[test]
    fn skips_files_without_synth() {
        let (_g, cache, _synth) = fixture_cache_with_two_files();
        // Empty synth state — no files have synths assigned.
        let empty = SynthState::default();
        let idx = NodeIndex::build(&cache, &empty).unwrap();
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
        let idx = NodeIndex::build(&cache, &synth).unwrap();
        assert!(idx.is_empty());
    }
}

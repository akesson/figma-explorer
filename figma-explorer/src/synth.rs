//! Persistent synthetic-id assignments for projects and files.
//!
//! The CLI's tagged-ID grammar uses small sequential integers (`proj:N`,
//! `file:N`) so that root-level entities have short, readable IDs. Figma's own
//! identifiers (`project_id` is a long number, `file_key` is a 22-char
//! alphanumeric string) are unwieldy as CLI input. We mint a synth `u32` for
//! each Figma project and file we see, and persist the mapping in
//! `<cache-root>/synth.json` so the assignments stay stable across runs.
//!
//! Why a separate file (instead of embedding `file_synth` in each
//! `.meta.json`): the per-file meta design lets two `cache prefetch`
//! invocations from different shells touch disjoint files concurrently without
//! sharing a write path. If synth assignment lived inside each meta we'd
//! reintroduce that global decision — two writers might both pick `N=7` for
//! different files. Keeping it in `synth.json` plus a `synth.lock` file means
//! the only contended resource is the synth state itself.
//!
//! Stability rules:
//! - Existing assignments are never reused. Deleting a file leaves its synth
//!   number as a gap; subsequent interns claim `max(existing) + 1`.
//! - Schema is versioned. Bump `SYNTH_SCHEMA_VERSION` when adding new entity
//!   tables (e.g. comments) so loaders can refuse files they don't understand
//!   instead of corrupting them.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::cache::CacheDir;

pub const SYNTH_SCHEMA_VERSION: u32 = 1;

const SYNTH_FILENAME: &str = "synth.json";
const SYNTH_LOCKFILE: &str = "synth.lock";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SynthState {
    /// Schema version on disk. Mismatch returns an error from `load`.
    #[serde(default = "default_version")]
    pub version: u32,
    /// `project_id` → synth integer. Canonical direction; reverse lookup is
    /// computed on demand (synth numbers are small, the lookup is O(N)).
    pub projects: BTreeMap<String, u32>,
    /// `file_key` → synth integer.
    pub files: BTreeMap<String, u32>,
}

fn default_version() -> u32 {
    SYNTH_SCHEMA_VERSION
}

impl Default for SynthState {
    fn default() -> Self {
        Self {
            version: SYNTH_SCHEMA_VERSION,
            projects: BTreeMap::new(),
            files: BTreeMap::new(),
        }
    }
}

impl SynthState {
    /// Read state from `<cache-root>/synth.json`. Returns an empty state when
    /// the file doesn't exist (first run). Schema version mismatch is hard —
    /// we refuse to load rather than silently misinterpret old data.
    pub fn load(cache_dir: &CacheDir) -> Result<Self> {
        let path = synth_path(cache_dir);
        if !path.exists() {
            return Ok(Self {
                version: SYNTH_SCHEMA_VERSION,
                projects: BTreeMap::new(),
                files: BTreeMap::new(),
            });
        }
        let s = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let state: SynthState = serde_json::from_str(&s)
            .with_context(|| format!("parsing {}", path.display()))?;
        if state.version != SYNTH_SCHEMA_VERSION {
            anyhow::bail!(
                "synth schema mismatch: file is v{}, build supports v{}. \
                 Delete {} to reset or migrate manually.",
                state.version,
                SYNTH_SCHEMA_VERSION,
                path.display()
            );
        }
        Ok(state)
    }

    /// Atomic write to disk via tempfile+rename. Callers must hold the
    /// `synth.lock` for the read-mutate-write window; use [`with_lock`] to
    /// get that for free.
    pub fn save(&self, cache_dir: &CacheDir) -> Result<()> {
        cache_dir.ensure()?;
        let path = synth_path(cache_dir);
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

    /// Return the synth for `file_key`, minting a new one if needed. New
    /// numbers are `max(existing) + 1` so we never reuse a deleted synth.
    pub fn intern_file(&mut self, file_key: &str) -> u32 {
        if let Some(&n) = self.files.get(file_key) {
            return n;
        }
        let next = next_free(&self.files);
        self.files.insert(file_key.to_owned(), next);
        next
    }

    /// Return the synth for `project_id`, minting if needed.
    pub fn intern_project(&mut self, project_id: &str) -> u32 {
        if let Some(&n) = self.projects.get(project_id) {
            return n;
        }
        let next = next_free(&self.projects);
        self.projects.insert(project_id.to_owned(), next);
        next
    }

    pub fn file_synth(&self, file_key: &str) -> Option<u32> {
        self.files.get(file_key).copied()
    }

    pub fn project_synth(&self, project_id: &str) -> Option<u32> {
        self.projects.get(project_id).copied()
    }

    /// Reverse lookup: synth → file_key. O(N) over the file table; N is
    /// bounded by the number of cached files (small).
    pub fn file_key(&self, synth: u32) -> Option<&str> {
        self.files
            .iter()
            .find_map(|(k, v)| (*v == synth).then_some(k.as_str()))
    }

    pub fn project_id(&self, synth: u32) -> Option<&str> {
        self.projects
            .iter()
            .find_map(|(k, v)| (*v == synth).then_some(k.as_str()))
    }
}

fn next_free(map: &BTreeMap<String, u32>) -> u32 {
    map.values().copied().max().map(|m| m + 1).unwrap_or(1)
}

fn synth_path(cache_dir: &CacheDir) -> PathBuf {
    cache_dir.root.join(SYNTH_FILENAME)
}

fn lock_path(cache_dir: &CacheDir) -> PathBuf {
    cache_dir.root.join(SYNTH_LOCKFILE)
}

/// Acquire `synth.lock` exclusively, load the current state, run `f`, save,
/// release the lock. All mutations of `synth.json` should go through this so
/// concurrent `cache prefetch` invocations don't race on synth assignment.
///
/// Blocks if another process is holding the lock. The lock window is small
/// (load + mutate + save of a tiny JSON file) so contention is rare in
/// practice.
pub fn with_lock<R, F>(cache_dir: &CacheDir, f: F) -> Result<R>
where
    F: FnOnce(&mut SynthState) -> R,
{
    cache_dir.ensure()?;
    let lock_file = open_lockfile(&lock_path(cache_dir))?;
    lock_file
        .lock_exclusive()
        .context("acquiring exclusive lock on synth.lock")?;
    let result = (|| -> Result<R> {
        let mut state = SynthState::load(cache_dir)?;
        let r = f(&mut state);
        state.save(cache_dir)?;
        Ok(r)
    })();
    // Best-effort unlock — file drop releases the lock anyway.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_cache_dir() -> (tempfile::TempDir, CacheDir) {
        let tmp = tempfile::tempdir().unwrap();
        let cache = CacheDir::new(tmp.path());
        cache.ensure().unwrap();
        (tmp, cache)
    }

    #[test]
    fn load_returns_empty_on_first_run() {
        let (_g, cache) = tmp_cache_dir();
        let state = SynthState::load(&cache).unwrap();
        assert!(state.files.is_empty());
        assert!(state.projects.is_empty());
        assert_eq!(state.version, SYNTH_SCHEMA_VERSION);
    }

    #[test]
    fn intern_is_idempotent() {
        let mut s = SynthState::default();
        let a = s.intern_file("file-a");
        let a_again = s.intern_file("file-a");
        let b = s.intern_file("file-b");
        assert_eq!(a, a_again);
        assert_ne!(a, b);
    }

    #[test]
    fn intern_starts_at_one() {
        let mut s = SynthState::default();
        assert_eq!(s.intern_file("a"), 1);
        assert_eq!(s.intern_file("b"), 2);
        assert_eq!(s.intern_project("p1"), 1);
    }

    #[test]
    fn intern_preserves_gaps_after_deletion() {
        let mut s = SynthState::default();
        s.intern_file("a"); // 1
        s.intern_file("b"); // 2
        s.intern_file("c"); // 3
        // Simulate deletion of file b.
        s.files.remove("b");
        // New file should claim 4, not reuse 2.
        assert_eq!(s.intern_file("d"), 4);
    }

    #[test]
    fn round_trip_via_save_and_load() {
        let (_g, cache) = tmp_cache_dir();
        let mut s = SynthState::default();
        s.intern_project("77195660");
        s.intern_file("file-a");
        s.intern_file("file-b");
        s.save(&cache).unwrap();

        let loaded = SynthState::load(&cache).unwrap();
        assert_eq!(loaded, s);
    }

    #[test]
    fn schema_version_mismatch_errors() {
        let (_g, cache) = tmp_cache_dir();
        let path = synth_path(&cache);
        // Hand-craft a file with a bogus version.
        fs::write(&path, r#"{"version": 99, "projects": {}, "files": {}}"#).unwrap();
        let err = SynthState::load(&cache).unwrap_err().to_string();
        assert!(err.contains("schema mismatch"), "got: {err}");
    }

    #[test]
    fn reverse_lookup_works() {
        let mut s = SynthState::default();
        let a = s.intern_file("file-a");
        let b = s.intern_project("proj-1");
        assert_eq!(s.file_key(a), Some("file-a"));
        assert_eq!(s.project_id(b), Some("proj-1"));
        assert_eq!(s.file_key(999), None);
    }

    #[test]
    fn with_lock_serializes_concurrent_interns() {
        use std::sync::Arc;
        use std::thread;

        let (_g, cache) = tmp_cache_dir();
        let cache = Arc::new(cache);

        // Two threads each intern 10 distinct file keys via with_lock. Without
        // the lock they'd race on `next_free` and both pick the same N for
        // different keys. With it, every key gets a unique synth.
        let handles: Vec<_> = (0..2)
            .map(|t| {
                let cache = Arc::clone(&cache);
                thread::spawn(move || {
                    let mut synths = Vec::with_capacity(10);
                    for i in 0..10 {
                        let key = format!("thread{t}-file{i}");
                        let n = with_lock(&cache, |s| s.intern_file(&key)).unwrap();
                        synths.push(n);
                    }
                    synths
                })
            })
            .collect();

        let mut all = Vec::new();
        for h in handles {
            all.extend(h.join().unwrap());
        }
        all.sort_unstable();
        let unique_count = all.iter().collect::<std::collections::HashSet<_>>().len();
        assert_eq!(unique_count, all.len(), "duplicate synth assignments: {all:?}");

        // Final state on disk should have all 20 files.
        let state = SynthState::load(&cache).unwrap();
        assert_eq!(state.files.len(), 20);
    }
}

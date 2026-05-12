//! Sidecars for the full, raw API responses.
//!
//! Two files per cached Figma file_key:
//!
//! - `{file_key}.full.json.gz` — the entire `/v1/files/{file_key}` body, gzipped.
//!   The structural cache (`.rkyv`) only keeps a slim projection (id/type/name/
//!   visible/bounds/children) so navigation (`ls`, `find`) stays cheap. The
//!   `node-info` command needs everything else — fills, strokes, effects,
//!   text style, layout, component data, prototype, bound variables — so we
//!   keep the full JSON here and parse on demand. Gzip is "good enough" (~4–5×
//!   on Figma JSON) and is in the standard Rust ecosystem.
//!
//! - `{file_key}.variables.json` — the `/v1/files/{file_key}/variables/local`
//!   body. Plaintext (variables JSON is small). Optional: only present when
//!   the account has Variables REST API access (Enterprise).
//!
//! Both sidecars are versioned via `[FULL|VARIABLES]_SCHEMA_VERSION` and
//! stamped on `FileMeta`. Mismatch is treated as "missing" so the cache
//! refetches on next access.
//!
//! Both reads are tolerant: a corrupted / unreadable sidecar surfaces as
//! `Ok(None)` so the caller falls through to refetch instead of erroring out.

use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::Value;

use crate::cache::CacheDir;

/// Read the gzipped full-JSON sidecar. `Ok(None)` when the file is absent or
/// unreadable (corrupted gzip, malformed JSON) — the caller falls through to
/// refetch in that case.
pub fn read_full(cache: &CacheDir, file_key: &str) -> Result<Option<Value>> {
    let p = cache.full_path(file_key);
    if !p.exists() {
        return Ok(None);
    }
    let bytes = match fs::read(&p) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("full_cache: read failed for {}: {e}", p.display());
            return Ok(None);
        }
    };
    let mut decoder = GzDecoder::new(&bytes[..]);
    let mut decompressed = Vec::with_capacity(bytes.len() * 5);
    if let Err(e) = decoder.read_to_end(&mut decompressed) {
        eprintln!("full_cache: gzip decode failed for {}: {e}", p.display());
        return Ok(None);
    }
    match serde_json::from_slice(&decompressed) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            eprintln!("full_cache: JSON parse failed for {}: {e}", p.display());
            Ok(None)
        }
    }
}

/// Write the gzipped full-JSON sidecar atomically. Returns the (compressed)
/// byte count for `FileMeta::full_bytes`.
pub fn write_full(cache: &CacheDir, file_key: &str, v: &Value) -> Result<u64> {
    let p = cache.full_path(file_key);
    let raw = serde_json::to_vec(v).context("serializing full JSON")?;
    let mut encoder = GzEncoder::new(Vec::with_capacity(raw.len() / 4), Compression::default());
    encoder.write_all(&raw).context("gzip encode")?;
    let compressed = encoder.finish().context("gzip finalize")?;
    let n = compressed.len() as u64;
    atomic_write(&p, &compressed)?;
    Ok(n)
}

/// Read the plaintext variables sidecar. `Ok(None)` when absent / unreadable.
pub fn read_variables(cache: &CacheDir, file_key: &str) -> Result<Option<Value>> {
    let p = cache.variables_path(file_key);
    if !p.exists() {
        return Ok(None);
    }
    let s = match fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("full_cache: read failed for {}: {e}", p.display());
            return Ok(None);
        }
    };
    match serde_json::from_str(&s) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            eprintln!("full_cache: JSON parse failed for {}: {e}", p.display());
            Ok(None)
        }
    }
}

/// Write the variables sidecar atomically. Returns byte count.
pub fn write_variables(cache: &CacheDir, file_key: &str, v: &Value) -> Result<u64> {
    let p = cache.variables_path(file_key);
    let bytes = serde_json::to_vec_pretty(v).context("serializing variables JSON")?;
    let n = bytes.len() as u64;
    atomic_write(&p, &bytes)?;
    Ok(n)
}

/// Remove both sidecars for `file_key`. Idempotent. Used on fetch failure
/// to drop stale sidecars next to the failed entry; `CacheDir::delete_entry`
/// handles the cache-clear path itself.
pub fn delete_sidecars(cache: &CacheDir, file_key: &str) -> Result<()> {
    let full = cache.full_path(file_key);
    if full.exists() {
        fs::remove_file(&full).with_context(|| format!("removing {}", full.display()))?;
    }
    let vars = cache.variables_path(file_key);
    if vars.exists() {
        fs::remove_file(&vars).with_context(|| format!("removing {}", vars.display()))?;
    }
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn tmp() -> (TempDir, CacheDir) {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        (td, cache)
    }

    #[test]
    fn full_roundtrip_via_gzip() {
        let (_g, cache) = tmp();
        let value = json!({
            "document": { "id": "0:0", "type": "DOCUMENT", "children": [] },
            "name": "demo",
            "lastModified": "2026-01-01",
        });
        let bytes = write_full(&cache, "K", &value).unwrap();
        assert!(bytes > 0);
        let back = read_full(&cache, "K").unwrap().expect("present");
        assert_eq!(back, value);
    }

    #[test]
    fn read_full_missing_returns_none() {
        let (_g, cache) = tmp();
        let v = read_full(&cache, "absent").unwrap();
        assert!(v.is_none());
    }

    #[test]
    fn read_full_corrupted_returns_none_not_err() {
        let (_g, cache) = tmp();
        // Write a non-gzip file at the expected path.
        let p = cache.full_path("BAD");
        std::fs::write(&p, b"not really gzip").unwrap();
        // Corrupt sidecar surfaces as "not cached" so the caller refetches.
        let v = read_full(&cache, "BAD").unwrap();
        assert!(v.is_none());
    }

    #[test]
    fn variables_roundtrip_plaintext() {
        let (_g, cache) = tmp();
        let value = json!({ "variables": {}, "variableCollections": {} });
        write_variables(&cache, "K", &value).unwrap();
        let back = read_variables(&cache, "K").unwrap().expect("present");
        assert_eq!(back, value);
    }

    #[test]
    fn delete_sidecars_is_idempotent() {
        let (_g, cache) = tmp();
        // Nothing to delete — no error.
        delete_sidecars(&cache, "nothing").unwrap();

        write_full(&cache, "K", &json!({})).unwrap();
        write_variables(&cache, "K", &json!({})).unwrap();
        assert!(cache.full_path("K").exists());
        assert!(cache.variables_path("K").exists());

        delete_sidecars(&cache, "K").unwrap();
        assert!(!cache.full_path("K").exists());
        assert!(!cache.variables_path("K").exists());
    }
}

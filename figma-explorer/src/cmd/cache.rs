use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use futures::stream::{self, StreamExt};
use serde_json::json;

use crate::cache::{self, build_cached_file, CacheDir, EntryStatus, FileRef, Manifest, ManifestEntry};
use crate::cmd::fetch_file_json;
use crate::{print, Output};

/// Prime / refresh the local file cache.
///
/// Walks every project in `--project-ids`, downloads each file whose
/// `lastModified` differs from the manifest, projects each document tree
/// down to its structural fields, and writes one JSON per file plus an
/// updated `manifest.json`. Re-running is cheap: only files whose
/// `lastModified` changed are refetched.
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Comma-separated list of Figma project IDs. Falls back to FIGMA_PROJECTS_IDS.
    #[arg(long, env = "FIGMA_PROJECTS_IDS", value_delimiter = ',', required = true)]
    pub project_ids: Vec<String>,

    /// Cache directory (created if missing).
    #[arg(long, default_value = cache::DEFAULT_DIR)]
    pub dir: PathBuf,

    /// Re-fetch every file, ignoring cached `last_modified`.
    #[arg(long)]
    pub force: bool,

    /// Max concurrent file fetches. Figma's per-minute limit for
    /// `GET /v1/files/{key}` is 20 on Org plans; 3 keeps us comfortably under
    /// at typical fetch latencies (5–30 s per file).
    #[arg(long, default_value_t = 3)]
    pub concurrency: usize,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, format: Output) -> Result<()> {
        let cache_dir = CacheDir::new(&self.dir);
        cache_dir.ensure()?;

        let prev = cache_dir.read_manifest()?;
        let mut by_key: HashMap<String, ManifestEntry> = prev
            .files
            .iter()
            .cloned()
            .map(|e| (e.file_key.clone(), e))
            .collect();

        // Enumerate all files in the named projects, preserving order.
        let started = Instant::now();
        let listing: Vec<FileRef> = cache::list_project_files(cfg, &self.project_ids).await?;

        // Decide what to fetch. Skip entries we already have under the same
        // last_modified, unless --force. Failed entries are always retried;
        // not-exportable entries are skipped until last_modified changes.
        // Also refetch if the manifest says OK but the payload file is gone.
        let force = self.force;
        let to_fetch: Vec<FileRef> = listing
            .iter()
            .filter(|f| {
                if force {
                    return true;
                }
                match by_key.get(&f.file_key) {
                    Some(e) if e.last_modified == f.last_modified => match e.status {
                        EntryStatus::Failed => true,
                        EntryStatus::Ok => !cache_dir.file_path(&f.file_key).exists(),
                        EntryStatus::NotExportable => false,
                    },
                    _ => true,
                }
            })
            .cloned()
            .collect();

        let total = listing.len();
        let n_to_fetch = to_fetch.len();
        let n_skipped = total - n_to_fetch;
        eprintln!(
            "cache: {total} files in projects, {n_skipped} up-to-date, {n_to_fetch} to fetch (concurrency={})",
            self.concurrency
        );

        let cache_root = cache_dir.root.clone();
        let fetched: Vec<ManifestEntry> = stream::iter(to_fetch.into_iter().map(|f| {
            let cfg = cfg;
            let cache_root = cache_root.clone();
            async move {
                let started = Instant::now();
                match fetch_file_json(cfg, &f.file_key, None).await {
                    Ok(file) => {
                        let now = cache::now_epoch();
                        let payload = build_cached_file(&f, &file["document"], now);
                        let node_count = payload.node_count as usize;
                        let cache = CacheDir::new(&cache_root);
                        let bytes = cache.write_file(&f.file_key, &payload)?;
                        let secs = started.elapsed().as_secs_f64();
                        eprintln!(
                            "  ok    {:<25}  {:<35}  {} nodes, {} KB in {:.1}s",
                            f.file_key,
                            truncate(&f.name, 35),
                            node_count,
                            bytes / 1024,
                            secs
                        );
                        Ok::<ManifestEntry, anyhow::Error>(ManifestEntry {
                            file_key: f.file_key,
                            name: f.name,
                            project_id: f.project_id,
                            project_name: f.project_name,
                            last_modified: f.last_modified,
                            cached_at_epoch: now,
                            status: EntryStatus::Ok,
                            error: None,
                            node_count: Some(node_count),
                            bytes: Some(bytes),
                        })
                    }
                    Err(e) => {
                        let msg = format!("{:#}", e);
                        let status = if cache::is_not_exportable_error(&msg) {
                            EntryStatus::NotExportable
                        } else {
                            EntryStatus::Failed
                        };
                        let tag = match status {
                            EntryStatus::NotExportable => "skip",
                            EntryStatus::Failed => "fail",
                            EntryStatus::Ok => unreachable!(),
                        };
                        eprintln!(
                            "  {:<5} {:<25}  {:<35}  {}",
                            tag,
                            f.file_key,
                            truncate(&f.name, 35),
                            msg
                        );
                        Ok(ManifestEntry {
                            file_key: f.file_key,
                            name: f.name,
                            project_id: f.project_id,
                            project_name: f.project_name,
                            last_modified: f.last_modified,
                            cached_at_epoch: cache::now_epoch(),
                            status,
                            error: Some(msg),
                            node_count: None,
                            bytes: None,
                        })
                    }
                }
            }
        }))
        .buffer_unordered(self.concurrency)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

        for entry in fetched {
            by_key.insert(entry.file_key.clone(), entry);
        }

        // Drop entries no longer in the project listing (file deleted/moved).
        // Also delete their on-disk payloads so the cache stays tidy.
        let live_keys: std::collections::HashSet<String> =
            listing.iter().map(|f| f.file_key.clone()).collect();
        let stale_keys: Vec<String> = by_key
            .keys()
            .filter(|k| !live_keys.contains(*k))
            .cloned()
            .collect();
        for k in &stale_keys {
            let _ = std::fs::remove_file(cache_dir.file_path(k));
            by_key.remove(k);
        }
        if !stale_keys.is_empty() {
            eprintln!(
                "cache: pruned {} stale entries no longer in any project",
                stale_keys.len()
            );
        }

        // Rebuild the manifest in project-listing order.
        let mut files: Vec<ManifestEntry> = Vec::with_capacity(listing.len());
        for f in &listing {
            if let Some(e) = by_key.get(&f.file_key) {
                files.push(e.clone());
            }
        }

        let manifest = Manifest {
            updated_at_epoch: cache::now_epoch(),
            files,
        };
        cache_dir.write_manifest(&manifest)?;

        let elapsed = started.elapsed();
        let ok = manifest
            .files
            .iter()
            .filter(|e| e.status == EntryStatus::Ok)
            .count();
        let not_exportable = manifest
            .files
            .iter()
            .filter(|e| e.status == EntryStatus::NotExportable)
            .count();
        let failed = manifest
            .files
            .iter()
            .filter(|e| e.status == EntryStatus::Failed)
            .count();
        let cache_bytes: u64 = manifest.files.iter().filter_map(|e| e.bytes).sum();
        let total_nodes: usize = manifest.files.iter().filter_map(|e| e.node_count).sum();

        let summary = json!({
            "cache_dir": cache_dir.root.display().to_string(),
            "elapsed_seconds": elapsed.as_secs_f64(),
            "fetched": n_to_fetch,
            "skipped_up_to_date": n_skipped,
            "pruned_stale": stale_keys.len(),
            "total_in_projects": total,
            "ok": ok,
            "not_exportable": not_exportable,
            "failed": failed,
            "cache_bytes": cache_bytes,
            "total_nodes": total_nodes,
        });
        print(&summary, format)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

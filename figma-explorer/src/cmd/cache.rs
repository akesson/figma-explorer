use std::collections::HashSet;
use std::time::Instant;

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};
use figma_api::apis::configuration::Configuration;
use futures::stream::{self, StreamExt};
use serde_json::json;

use crate::cache::{
    self, build_cached_file, default_dir, CacheDir, EntryStatus, FileMeta, FileRef,
};
use crate::cmd::fetch_file_json;
use crate::{print, Output};

/// Cache maintenance commands.
///
/// The cache itself is populated lazily by structural commands (`tree`,
/// `find`, `search`, `pages`, `frames`) — you don't need to run any of these
/// in the common case. They exist for two scenarios: pre-warming before
/// offline work (`prefetch`) and surgical invalidation (`clear`).
#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: CacheCommand,
}

#[derive(Subcommand, Debug)]
pub enum CacheCommand {
    /// Pre-fetch every file in the configured projects, invalidating stale
    /// entries first. Opt-in; structural commands do not require this.
    Prefetch(PrefetchArgs),
    /// Delete cached entries. Without --file-key, wipes the cache directory.
    Clear(ClearArgs),
}

#[derive(ClapArgs, Debug)]
pub struct PrefetchArgs {
    /// Comma-separated list of Figma project IDs. Falls back to FIGMA_PROJECTS_IDS.
    #[arg(long, env = "FIGMA_PROJECTS_IDS", value_delimiter = ',', required = true)]
    pub project_ids: Vec<String>,

    /// Re-fetch every file, ignoring cached `last_modified`.
    #[arg(long)]
    pub force: bool,

    /// Max concurrent file fetches. Figma's per-minute limit for
    /// `GET /v1/files/{key}` is 20 on Org plans; 3 keeps us comfortably under
    /// at typical fetch latencies (5–30 s per file).
    #[arg(long, default_value_t = 3)]
    pub concurrency: usize,
}

#[derive(ClapArgs, Debug)]
pub struct ClearArgs {
    /// Clear only this file_key. Omit to clear the entire cache directory.
    #[arg(long)]
    pub file_key: Option<String>,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, format: Output) -> Result<()> {
        match self.command {
            CacheCommand::Prefetch(a) => a.run(cfg, format).await,
            CacheCommand::Clear(a) => a.run(format),
        }
    }
}

impl PrefetchArgs {
    pub async fn run(self, cfg: &Configuration, format: Output) -> Result<()> {
        let cache_dir = CacheDir::new(default_dir());
        cache_dir.ensure()?;

        let configured: HashSet<String> = self.project_ids.iter().cloned().collect();
        let started = Instant::now();

        // (1) Pull the listing for every configured project.
        let listing: Vec<FileRef> = cache::list_project_files(cfg, &self.project_ids).await?;
        let listing_keys: HashSet<String> = listing.iter().map(|f| f.file_key.clone()).collect();
        let listing_by_key: std::collections::HashMap<String, &FileRef> =
            listing.iter().map(|f| (f.file_key.clone(), f)).collect();

        // (2) Walk existing metas. Three cases:
        //     - project_id non-empty AND not in configured: another scope's
        //       data — leave alone.
        //     - project_id empty (cold-loaded via direct URL): upgrade to
        //       the listing's project_id if this listing claims the file.
        //     - project_id in configured: standard jurisdiction — confirm,
        //       drop, or refresh.
        let metas_on_disk = cache_dir.list_metas()?;
        let now_pre = cache::now_epoch();
        let mut pruned = 0usize;
        let mut upgraded = 0usize;
        for m in &metas_on_disk {
            let in_jurisdiction = configured.contains(&m.project_id);
            let upgradable = m.project_id.is_empty() && listing_keys.contains(&m.file_key);
            if !in_jurisdiction && !upgradable {
                continue;
            }

            let current = listing_by_key.get(&m.file_key).copied();
            let unchanged = current.is_some_and(|c| c.last_modified == m.last_modified);
            let payload_ok =
                m.status == EntryStatus::Ok && cache_dir.file_path(&m.file_key).exists();

            if self.force && in_jurisdiction {
                let _ = cache_dir.delete_entry(&m.file_key);
                continue;
            }

            match current {
                None => {
                    // in_jurisdiction must hold (upgradable requires presence).
                    let _ = cache_dir.delete_entry(&m.file_key);
                    pruned += 1;
                }
                Some(current) if unchanged && payload_ok => {
                    let mut updated = m.clone();
                    if upgradable {
                        updated.project_id = current.project_id.clone();
                        upgraded += 1;
                    }
                    updated.last_listed_at_epoch = now_pre;
                    updated.project_name = current.project_name.clone();
                    updated.name = current.name.clone();
                    let _ = cache_dir.write_meta(&updated);
                }
                Some(_) if unchanged && m.status == EntryStatus::NotExportable => {
                    let mut updated = m.clone();
                    updated.last_listed_at_epoch = now_pre;
                    let _ = cache_dir.write_meta(&updated);
                }
                Some(_) => {
                    // Stale or transient-failed — drop and refetch below.
                    let _ = cache_dir.delete_entry(&m.file_key);
                }
            }
        }

        // (3) Decide which files in the listing need a (re)fetch.
        let to_fetch: Vec<FileRef> = listing
            .iter()
            .filter(|f| {
                if self.force {
                    return true;
                }
                match cache_dir.read_meta(&f.file_key).ok().flatten() {
                    Some(m) if m.last_modified == f.last_modified => match m.status {
                        EntryStatus::Ok => !cache_dir.file_path(&f.file_key).exists(),
                        EntryStatus::NotExportable => false,
                        EntryStatus::Failed => true,
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
            "cache: {total} files in projects, {n_skipped} up-to-date, {n_to_fetch} to fetch, {pruned} pruned, {upgraded} upgraded (concurrency={})",
            self.concurrency
        );

        let cache_root = cache_dir.root.clone();
        let fetched_count = stream::iter(to_fetch.into_iter().map(|f| {
                let cache_root = cache_root.clone();
                async move {
                    let started = Instant::now();
                    let cache = CacheDir::new(&cache_root);
                    let now = cache::now_epoch();
                    match fetch_file_json(cfg, &f.file_key, None).await {
                        Ok(file) => {
                            let payload = build_cached_file(&f, &file["document"], now);
                            let node_count = payload.node_count as usize;
                            let bytes = match cache.write_file(&f.file_key, &payload) {
                                Ok(b) => b,
                                Err(e) => {
                                    eprintln!(
                                        "  fail  {:<25}  {:<35}  write_file: {e:#}",
                                        f.file_key,
                                        truncate(&f.name, 35),
                                    );
                                    return;
                                }
                            };
                            let meta = FileMeta {
                                file_key: f.file_key.clone(),
                                name: f.name.clone(),
                                project_id: f.project_id.clone(),
                                project_name: f.project_name.clone(),
                                last_modified: f.last_modified.clone(),
                                cached_at_epoch: now,
                                last_listed_at_epoch: now,
                                status: EntryStatus::Ok,
                                error: None,
                                node_count: Some(node_count),
                                bytes: Some(bytes),
                            };
                            if let Err(e) = cache.write_meta(&meta) {
                                eprintln!("cache: write_meta failed for {}: {e:#}", f.file_key);
                            }
                            let secs = started.elapsed().as_secs_f64();
                            eprintln!(
                                "  ok    {:<25}  {:<35}  {} nodes, {} KB in {:.1}s",
                                f.file_key,
                                truncate(&f.name, 35),
                                node_count,
                                bytes / 1024,
                                secs
                            );
                        }
                        Err(e) => {
                            let msg = format!("{e:#}");
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
                            // Record the failure marker so subsequent loads
                            // don't keep retrying. Drop any stale payload.
                            let _ = cache.delete_entry(&f.file_key);
                            let marker = FileMeta {
                                file_key: f.file_key.clone(),
                                name: f.name.clone(),
                                project_id: f.project_id.clone(),
                                project_name: f.project_name.clone(),
                                last_modified: f.last_modified.clone(),
                                cached_at_epoch: now,
                                last_listed_at_epoch: now,
                                status,
                                error: Some(msg.clone()),
                                node_count: None,
                                bytes: None,
                            };
                            let _ = cache.write_meta(&marker);
                        }
                    }
                }
            }))
            .buffer_unordered(self.concurrency)
            .fold(0usize, |n, _| async move { n + 1 })
            .await;

        // (4) Tally summary from disk so we capture both newly-fetched and
        //     previously-cached entries within jurisdiction.
        let metas_after = cache_dir.list_metas()?;
        let ok = metas_after
            .iter()
            .filter(|m| configured.contains(&m.project_id) && m.status == EntryStatus::Ok)
            .count();
        let not_exportable = metas_after
            .iter()
            .filter(|m| {
                configured.contains(&m.project_id) && m.status == EntryStatus::NotExportable
            })
            .count();
        let failed = metas_after
            .iter()
            .filter(|m| configured.contains(&m.project_id) && m.status == EntryStatus::Failed)
            .count();
        let cache_bytes: u64 = metas_after
            .iter()
            .filter(|m| configured.contains(&m.project_id))
            .filter_map(|m| m.bytes)
            .sum();
        let total_nodes: usize = metas_after
            .iter()
            .filter(|m| configured.contains(&m.project_id))
            .filter_map(|m| m.node_count)
            .sum();

        let elapsed = started.elapsed();
        let summary = json!({
            "cache_dir": cache_dir.root.display().to_string(),
            "elapsed_seconds": elapsed.as_secs_f64(),
            "fetched": fetched_count,
            "skipped_up_to_date": n_skipped,
            "pruned_stale": pruned,
            "upgraded_project_id": upgraded,
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

impl ClearArgs {
    pub fn run(self, format: Output) -> Result<()> {
        let cache_dir = CacheDir::new(default_dir());
        let files_dir = cache_dir.files_dir();

        let mut deleted = 0usize;
        let mut errors = 0usize;

        if let Some(key) = &self.file_key {
            if cache_dir.meta_path(key).exists() || cache_dir.file_path(key).exists() {
                cache_dir.delete_entry(key)?;
                deleted = 1;
            }
            eprintln!(
                "cache: cleared {} entr{} for {key}",
                deleted,
                if deleted == 1 { "y" } else { "ies" }
            );
        } else if files_dir.exists() {
            // Walk and delete every meta + payload (and any orphans).
            for entry in std::fs::read_dir(&files_dir)? {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => {
                        errors += 1;
                        continue;
                    }
                };
                let path = entry.path();
                if path.is_file() {
                    if let Err(e) = std::fs::remove_file(&path) {
                        eprintln!("cache: failed to remove {}: {e}", path.display());
                        errors += 1;
                    } else {
                        deleted += 1;
                    }
                }
            }
            eprintln!(
                "cache: cleared {deleted} files under {} ({errors} errors)",
                files_dir.display()
            );
        } else {
            eprintln!("cache: nothing to clear (no cache directory at {})", files_dir.display());
        }

        let summary = json!({
            "cache_dir": cache_dir.root.display().to_string(),
            "scope": self.file_key.unwrap_or_else(|| "all".to_owned()),
            "deleted": deleted,
            "errors": errors,
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

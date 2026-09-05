use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};
use figma_api::apis::configuration::Configuration;
use futures::stream::{self, StreamExt};
use serde_json::{json, Value};

use crate::cache::{
    self, build_cached_file, default_dir, fetch_comments_into_meta, CacheDir, EntryStatus,
    FileMeta, FileRef,
};
use crate::cmd::{
    fetch_file_json, fetch_local_variables, is_variables_forbidden_error, require_document,
};
use crate::full_cache;
use crate::team_catalog;
use crate::{print, Globals, Output};

/// After this many consecutive 403s on `/variables/local`, disable further
/// variables fetches for the rest of the prefetch run. Non-Enterprise
/// accounts get 403 on every file, so without this gate a single prefetch
/// would burn one quota slot per file for no useful data.
const VARIABLES_403_DISABLE_THRESHOLD: usize = 3;

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
    /// Delete cached entries. Without --file-key, clears all cached files
    /// and team catalogs (synth IDs are preserved).
    Clear(ClearArgs),
    /// Report what the cache holds: per-file payload age and sidecar
    /// presence (full/comments/variables), team catalogs, marks. Offline —
    /// never touches the network.
    Status(StatusArgs),
}

#[derive(ClapArgs, Debug)]
pub struct PrefetchArgs {
    /// Comma-separated list of Figma folder IDs (Figma renamed "projects" to
    /// "folders" in Aug 2026 — the numeric ids are unchanged). Falls back to
    /// FIGMA_PROJECTS_IDS.
    #[arg(
        long,
        visible_alias = "folder-ids",
        env = "FIGMA_PROJECTS_IDS",
        value_delimiter = ',',
        required = true
    )]
    pub project_ids: Vec<String>,

    /// Re-fetch every file, ignoring cached `last_modified`.
    #[arg(long)]
    pub force: bool,

    /// Max concurrent file fetches. Figma's per-minute limit for
    /// `GET /v1/files/{key}` is 20 on Org plans; 3 keeps us comfortably under
    /// at typical fetch latencies (5–30 s per file).
    #[arg(long, default_value_t = 3)]
    pub concurrency: usize,

    /// Skip writing the `.full.json.gz` sidecar. Default: write it (needed by
    /// `node-info`). Useful only when disk pressure outweighs offline support.
    #[arg(long)]
    pub no_full: bool,

    /// Skip fetching `/variables/local`. Default: attempt the fetch with an
    /// adaptive 403-disable safeguard. Also disabled by env
    /// `FIGMA_EXPLORER_FETCH_VARIABLES=0`. Non-Enterprise accounts get 403 on
    /// every file, so the adaptive guard kicks in after 3 strikes anyway.
    #[arg(long)]
    pub no_variables: bool,

    /// Figma team id for warming the team-library catalog. Falls back to
    /// FIGMA_TEAM_ID. When no team id is available the catalog warm is
    /// skipped silently — prefetch's core contract is project-based.
    #[arg(long, env = "FIGMA_TEAM_ID", hide_env_values = true)]
    pub team_id: Option<String>,

    /// Skip warming the team-library catalog sidecar. Default: warm it when a
    /// team id is available, so `library search --cache-only` works offline.
    #[arg(long)]
    pub no_catalog: bool,
}

#[derive(ClapArgs, Debug)]
pub struct ClearArgs {
    /// Clear only this file_key. Omit to clear all cached files and team
    /// catalogs (synth IDs are preserved).
    #[arg(long)]
    pub file_key: Option<String>,
}

#[derive(ClapArgs, Debug)]
pub struct StatusArgs {}

impl Args {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        let format = globals.output;
        match self.command {
            CacheCommand::Prefetch(a) => a.run(cfg, format).await,
            CacheCommand::Clear(a) => a.run(format),
            CacheCommand::Status(a) => a.run(format),
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
        // Files whose document is unchanged + Ok — we still want to poll their
        // comments because Figma's `lastModified` doesn't tick for comment
        // activity. Refreshed concurrently after the main fetch pass.
        let mut to_refresh_comments: Vec<(FileMeta, FileRef)> = Vec::new();
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
                    let mut staged = m.clone();
                    if upgradable {
                        staged.project_id = current.project_id.clone();
                        upgraded += 1;
                    }
                    staged.last_listed_at_epoch = now_pre;
                    staged.project_name = current.project_name.clone();
                    staged.name = current.name.clone();
                    // Defer the write until after the comments fetch so we
                    // capture comments_fetched_at_epoch + fingerprint in a
                    // single meta update. This branch also serves as the
                    // sidecar-migration path: any meta with no
                    // comments_schema_version (or an older one) gets its
                    // comments refetched on this pass, which rewrites the
                    // sidecar in the current shape and stamps the version.
                    to_refresh_comments.push((staged, current.clone()));
                }
                Some(_) if unchanged && m.status == EntryStatus::NotExportable => {
                    let mut updated = m.clone();
                    updated.last_listed_at_epoch = now_pre;
                    if let Err(e) = cache_dir.write_meta(&updated) {
                        eprintln!("cache: write_meta failed for {}: {e:#}", m.file_key);
                    }
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

        // Env opt-out: `FIGMA_EXPLORER_FETCH_VARIABLES=0` disables variables
        // fetches without requiring the flag. Honors any value other than "0"
        // as "enabled" — typical bool-y env conventions.
        let env_no_variables = std::env::var("FIGMA_EXPLORER_FETCH_VARIABLES")
            .map(|s| s.trim() == "0")
            .unwrap_or(false);
        let no_variables = self.no_variables || env_no_variables;
        let no_full = self.no_full;

        // Adaptive-disable state for variables: shared across the worker pool.
        let variables_disabled = Arc::new(AtomicBool::new(no_variables));
        let consecutive_403 = Arc::new(AtomicUsize::new(0));
        // Tallies for the final summary.
        let variables_ok = Arc::new(AtomicUsize::new(0));
        let variables_403 = Arc::new(AtomicUsize::new(0));
        let full_written = Arc::new(AtomicUsize::new(0));

        let cache_root = cache_dir.root.clone();
        let fetched_count = stream::iter(to_fetch.into_iter().map(|f| {
                let cache_root = cache_root.clone();
                let variables_disabled = Arc::clone(&variables_disabled);
                let consecutive_403 = Arc::clone(&consecutive_403);
                let variables_ok = Arc::clone(&variables_ok);
                let variables_403 = Arc::clone(&variables_403);
                let full_written = Arc::clone(&full_written);
                async move {
                    let started = Instant::now();
                    let cache = CacheDir::new(&cache_root);
                    let now = cache::now_epoch();
                    // Validate `document` as part of the fetch so a malformed
                    // 200 (missing/null `document`) flows into the failure arm
                    // below — a marker, not an empty cached payload.
                    let fetched = fetch_file_json(cfg, &f.file_key, None).await.and_then(|file| {
                        require_document(&file, &f.file_key)?;
                        Ok(file)
                    });
                    match fetched {
                        Ok(file) => {
                            // `document` validated present in the fetch chain above.
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
                            let mut meta = FileMeta::from_success(&f, &payload, bytes, now);
                            // Fetch comments alongside the document. Best-effort:
                            // failures flip `meta.comments_error` but don't fail
                            // the entry.
                            fetch_comments_into_meta(cfg, &cache, &f.file_key, now, &mut meta).await;

                            // Full-JSON sidecar — keep the raw response so
                            // `node-info` (and future migrations of tokens/
                            // assets/context) can work offline.
                            if !no_full {
                                match full_cache::write_full(&cache, &f.file_key, &file) {
                                    Ok(n) => {
                                        meta.full_fetched_at_epoch = Some(now);
                                        meta.full_bytes = Some(n);
                                        meta.full_schema_version = Some(cache::FULL_SCHEMA_VERSION);
                                        full_written.fetch_add(1, Ordering::Relaxed);
                                    }
                                    Err(e) => eprintln!(
                                        "cache: write_full failed for {}: {e:#}",
                                        f.file_key
                                    ),
                                }
                            }

                            // Local-variables sidecar — paid-tier endpoint.
                            // Skip when disabled (flag, env, or adaptive 403).
                            if !variables_disabled.load(Ordering::Relaxed) {
                                match fetch_local_variables(cfg, &f.file_key).await {
                                    Ok(vars) => {
                                        match full_cache::write_variables(&cache, &f.file_key, &vars) {
                                            Ok(n) => {
                                                meta.variables_fetched_at_epoch = Some(now);
                                                meta.variables_bytes = Some(n);
                                                meta.variables_schema_version =
                                                    Some(cache::VARIABLES_SCHEMA_VERSION);
                                                meta.variables_error = None;
                                                variables_ok.fetch_add(1, Ordering::Relaxed);
                                                // Reset the 403 streak on success.
                                                consecutive_403.store(0, Ordering::Relaxed);
                                            }
                                            Err(e) => {
                                                let msg = format!("write_variables: {e:#}");
                                                eprintln!(
                                                    "cache: {} variables: {msg}",
                                                    f.file_key
                                                );
                                                meta.variables_error = Some(msg);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        let msg = format!("{e:#}");
                                        meta.variables_error = Some(msg.clone());
                                        if is_variables_forbidden_error(&msg) {
                                            variables_403.fetch_add(1, Ordering::Relaxed);
                                            let n = consecutive_403
                                                .fetch_add(1, Ordering::Relaxed)
                                                + 1;
                                            if n >= VARIABLES_403_DISABLE_THRESHOLD
                                                && !variables_disabled
                                                    .swap(true, Ordering::Relaxed)
                                            {
                                                eprintln!(
                                                    "cache: {VARIABLES_403_DISABLE_THRESHOLD} consecutive 403s on /variables/local — disabling for the rest of this run (account likely lacks Variables REST API access)"
                                                );
                                            }
                                        }
                                    }
                                }
                            }

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
                            // don't keep retrying. Drop any stale payload
                            // *and* sidecars: a fetch failure invalidates the
                            // whole entry, full + variables included.
                            let _ = cache.delete_entry(&f.file_key);
                            let _ = full_cache::delete_sidecars(&cache, &f.file_key);
                            let marker = FileMeta::failure_marker(
                                f.file_key.clone(),
                                f.name.clone(),
                                f.project_id.clone(),
                                f.project_name.clone(),
                                f.last_modified.clone(),
                                status,
                                msg,
                                now,
                            );
                            if let Err(e) = cache.write_meta(&marker) {
                                eprintln!("cache: write_meta failed for {}: {e:#}", f.file_key);
                            }
                        }
                    }
                }
            }))
            .buffer_unordered(self.concurrency)
            .fold(0usize, |n, _| async move { n + 1 })
            .await;

        // (3b) Refresh comments for files whose document was unchanged. Figma's
        // `lastModified` doesn't tick for comment activity, so without this
        // step prefetch would never observe new comments on stable files.
        let n_comments_only = to_refresh_comments.len();
        let cache_root = cache_dir.root.clone();
        let comments_refreshed =
            stream::iter(to_refresh_comments.into_iter().map(|(staged, _current)| {
                let cache_root = cache_root.clone();
                async move {
                    let cache = CacheDir::new(&cache_root);
                    let now = cache::now_epoch();
                    let mut updated = staged;
                    let file_key = updated.file_key.clone();
                    fetch_comments_into_meta(cfg, &cache, &file_key, now, &mut updated).await;
                    if let Err(e) = cache.write_meta(&updated) {
                        eprintln!("cache: write_meta failed for {file_key}: {e:#}");
                    }
                }
            }))
            .buffer_unordered(self.concurrency)
            .fold(0usize, |n, _| async move { n + 1 })
            .await;
        if n_comments_only > 0 {
            eprintln!(
                "cache: comments-only refresh complete: {comments_refreshed}/{n_comments_only}"
            );
        }

        // (4a) Intern synth IDs for every project + every successfully-cached
        //      file, plus every comment id under each file. One lock-protected
        //      pass at the end so prefetch's worker loop doesn't contend on
        //      the synth mutex.
        let metas_after = cache_dir.list_metas()?;
        // Pre-load sidecars for every Ok file under jurisdiction so the
        // synth-intern closure stays I/O-free while holding the lock.
        let comments_by_file: Vec<(String, Vec<crate::comment_assoc::AssociatedComment>)> =
            metas_after
                .iter()
                .filter(|m| m.status == EntryStatus::Ok && configured.contains(&m.project_id))
                .filter_map(|m| {
                    cache_dir
                        .read_comments(&m.file_key)
                        .ok()
                        .flatten()
                        .map(|c| (m.file_key.clone(), c))
                })
                .collect();
        if let Err(e) = crate::synth::with_lock(&cache_dir, |state| {
            for pid in &self.project_ids {
                state.intern_project(pid);
            }
            for m in &metas_after {
                if m.status == EntryStatus::Ok && configured.contains(&m.project_id) {
                    if !m.project_id.is_empty() {
                        state.intern_project(&m.project_id);
                    }
                    state.intern_file(&m.file_key);
                }
            }
            // Comments: scoped under their file synth. Run after the file
            // intern above so every `intern_comment` call has a valid synth.
            for (file_key, comments) in &comments_by_file {
                if let Some(file_synth) = state.file_synth(file_key) {
                    for c in comments {
                        state.intern_comment(file_synth, &c.comment_id);
                    }
                }
            }
        }) {
            eprintln!("cache: synth intern failed: {e:#}");
        }

        // (4b) Warm the team-library catalog so `library search --cache-only`
        //      works offline. Best-effort — a failure here never aborts the
        //      prefetch; the file work above is the core contract.
        let team_catalog_summary =
            warm_team_catalog(cfg, &cache_dir, self.team_id.as_deref(), self.no_catalog).await;

        // (4c) Tally summary from disk so we capture both newly-fetched and
        //      previously-cached entries within jurisdiction.
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
        let full_bytes_total: u64 = metas_after
            .iter()
            .filter(|m| configured.contains(&m.project_id))
            .filter_map(|m| m.full_bytes)
            .sum();
        let variables_bytes_total: u64 = metas_after
            .iter()
            .filter(|m| configured.contains(&m.project_id))
            .filter_map(|m| m.variables_bytes)
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
            "full_bytes": full_bytes_total,
            "variables_bytes": variables_bytes_total,
            "full_written": full_written.load(Ordering::Relaxed),
            "variables_ok": variables_ok.load(Ordering::Relaxed),
            "variables_403": variables_403.load(Ordering::Relaxed),
            "variables_disabled": variables_disabled.load(Ordering::Relaxed),
            "total_nodes": total_nodes,
            "comments_refreshed": comments_refreshed,
            "team_catalog": team_catalog_summary,
        });
        print(&summary, format)
    }
}

impl ClearArgs {
    pub fn run(self, format: Output) -> Result<()> {
        let cache_dir = CacheDir::new(default_dir());

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
        } else {
            // Full clear: sweep every per-file sidecar AND every team-scoped
            // catalog. `synth.json` is left intact so synth IDs stay stable.
            let files_dir = cache_dir.files_dir();
            let teams_dir = cache_dir.teams_dir();
            if files_dir.exists() || teams_dir.exists() {
                let (d, e) = clear_all_sidecars(&cache_dir)?;
                deleted = d;
                errors = e;
                eprintln!(
                    "cache: cleared {deleted} files under {} + {} ({errors} errors)",
                    files_dir.display(),
                    teams_dir.display(),
                );
            } else {
                eprintln!(
                    "cache: nothing to clear (no cache directory at {})",
                    cache_dir.root.display()
                );
            }
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

impl StatusArgs {
    pub fn run(self, format: Output) -> Result<()> {
        let cache_dir = CacheDir::new(default_dir());
        let now = cache::now_epoch();
        let synth_state = crate::synth::SynthState::load(&cache_dir)?;
        let metas = cache_dir.list_metas()?;

        // Per-file rows, ordered by synth id (unassigned sink to the end).
        let mut keyed: Vec<(u32, Value)> = metas
            .iter()
            .map(|m| {
                let synth = synth_state.file_synth(&m.file_key);
                let status = match m.status {
                    EntryStatus::Ok => "ok",
                    EntryStatus::NotExportable => "not_exportable",
                    EntryStatus::Failed => "failed",
                };
                // Sidecar presence is meta ∧ on-disk: a meta claiming a
                // sidecar that was deleted out-of-band must not report it.
                let full_age = m
                    .full_fetched_at_epoch
                    .filter(|_| cache_dir.full_path(&m.file_key).exists())
                    .map(|t| age(now, t));
                let variables_age = m
                    .variables_fetched_at_epoch
                    .filter(|_| cache_dir.variables_path(&m.file_key).exists())
                    .map(|t| age(now, t));
                let comments_age = m.comments_fetched_at_epoch.map(|t| age(now, t));
                let mut row = serde_json::Map::new();
                row.insert(
                    "id".into(),
                    match synth {
                        Some(s) => json!(format!("file:{s}")),
                        None => Value::Null,
                    },
                );
                row.insert("name".into(), json!(m.name));
                row.insert("key".into(), json!(m.file_key));
                row.insert("project".into(), json!(m.project_name));
                row.insert("status".into(), json!(status));
                row.insert("fetched".into(), json!(age(now, m.cached_at_epoch)));
                if let Some(n) = m.node_count {
                    row.insert("nodes".into(), json!(n));
                }
                row.insert(
                    "sidecars".into(),
                    json!({
                        "full": full_age,
                        "comments": comments_age,
                        "variables": variables_age,
                    }),
                );
                if let Some(e) = &m.error {
                    row.insert("error".into(), json!(e));
                }
                if let Some(e) = &m.variables_error {
                    let short = if e.contains("403") {
                        "no Variables REST API access (403)"
                    } else {
                        e.as_str()
                    };
                    row.insert("variables_error".into(), json!(short));
                }
                (synth.unwrap_or(u32::MAX), Value::Object(row))
            })
            .collect();
        keyed.sort_by_key(|(s, _)| *s);
        let files: Vec<Value> = keyed.into_iter().map(|(_, v)| v).collect();

        let count = |f: &dyn Fn(&FileMeta) -> bool| metas.iter().filter(|m| f(m)).count();
        let disk_bytes: u64 = metas
            .iter()
            .flat_map(|m| [m.bytes, m.full_bytes, m.variables_bytes])
            .flatten()
            .sum();
        let totals = json!({
            "files": metas.len(),
            "ok": count(&|m| m.status == EntryStatus::Ok),
            "failed": count(&|m| m.status == EntryStatus::Failed),
            "not_exportable": count(&|m| m.status == EntryStatus::NotExportable),
            "with_full": count(&|m| m.full_fetched_at_epoch.is_some()
                && cache_dir.full_path(&m.file_key).exists()),
            "with_comments": count(&|m| m.comments_fetched_at_epoch.is_some()),
            "with_variables": count(&|m| m.variables_fetched_at_epoch.is_some()
                && cache_dir.variables_path(&m.file_key).exists()),
            "disk_mb": disk_bytes / (1024 * 1024),
        });

        let team_catalogs = list_team_catalogs(&cache_dir, now);
        let marks = crate::marks::MarkStore::load_lenient(&cache_dir)
            .marks
            .len();

        let summary = json!({
            "cache_dir": cache_dir.root.display().to_string(),
            "totals": totals,
            "files": files,
            "team_catalogs": team_catalogs,
            "marks": marks,
        });
        print(&summary, format)
    }
}

/// Enumerate `teams/*.catalog.json.gz` and describe each catalog. Tolerant:
/// an unreadable sidecar becomes an `error` row rather than failing status.
fn list_team_catalogs(cache_dir: &CacheDir, now: u64) -> Vec<Value> {
    const SUFFIX: &str = ".catalog.json.gz";
    let dir = cache_dir.teams_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(team_id) = name.strip_suffix(SUFFIX) else {
            continue;
        };
        match team_catalog::read_catalog(cache_dir, team_id) {
            Ok(Some(c)) => out.push(json!({
                "team_id": c.team_id,
                "components": c.components.len(),
                "component_sets": c.component_sets.len(),
                "styles": c.styles.len(),
                "fetched": age(now, c.fetched_at_epoch),
                "stale": team_catalog::is_stale(c.fetched_at_epoch, now, cache::CATALOG_TTL_SECS),
            })),
            Ok(None) => out.push(json!({
                "team_id": team_id,
                "error": "unreadable or outdated schema — will refetch on next `library search`",
            })),
            Err(e) => out.push(json!({ "team_id": team_id, "error": format!("{e:#}") })),
        }
    }
    out
}

/// Compact "how long ago" for status rows: `42s`, `17m`, `5h`, `3d`.
/// Clock-skew safe — a timestamp in the future reads as `0s`.
fn age(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// Sweep every per-file sidecar and every team-scoped catalog sidecar from
/// the cache. `synth.json` is intentionally left intact so synth IDs stay
/// stable across a clear. Returns `(deleted, errors)`.
fn clear_all_sidecars(cache_dir: &CacheDir) -> Result<(usize, usize)> {
    let mut deleted = 0usize;
    let mut errors = 0usize;
    for dir in [cache_dir.files_dir(), cache_dir.teams_dir()] {
        if !dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&dir)? {
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
    }
    Ok((deleted, errors))
}

/// Best-effort warm of the team-library catalog during `cache prefetch`.
/// Returns a JSON value describing the outcome for the prefetch summary.
/// Never errors — a catalog failure must not abort the prefetch.
async fn warm_team_catalog(
    cfg: &Configuration,
    cache_dir: &CacheDir,
    team_id: Option<&str>,
    no_catalog: bool,
) -> serde_json::Value {
    if no_catalog {
        return json!("skipped (--no-catalog)");
    }
    let Some(team_id) = team_id else {
        return json!("skipped (no team id)");
    };
    match team_catalog::fetch_team_catalog(cfg, team_id).await {
        Ok(mut catalog) => match team_catalog::write_catalog(cache_dir, &mut catalog) {
            Ok(bytes) => {
                eprintln!(
                    "cache: team catalog warmed — {} entries ({} KB)",
                    catalog.total(),
                    bytes / 1024,
                );
                json!({
                    "components": catalog.components.len(),
                    "component_sets": catalog.component_sets.len(),
                    "styles": catalog.styles.len(),
                    "bytes": bytes,
                })
            }
            Err(e) => {
                let msg = format!("write failed: {e:#}");
                eprintln!("cache: team catalog {msg}");
                json!({ "error": msg })
            }
        },
        Err(e) => {
            let msg = format!("{e:#}");
            eprintln!("cache: team catalog fetch failed: {msg}");
            json!({ "error": msg })
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn age_buckets() {
        assert_eq!(age(100, 100), "0s");
        assert_eq!(age(100, 160), "0s", "future timestamp reads as fresh");
        assert_eq!(age(1000, 900), "1m");
        assert_eq!(age(10_000, 0), "2h");
        assert_eq!(age(200_000, 0), "2d");
    }

    #[test]
    fn clear_all_sidecars_sweeps_files_and_teams() {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        std::fs::write(cache.meta_path("abc"), b"{}").unwrap();
        std::fs::write(cache.file_path("abc"), b"payload").unwrap();
        std::fs::write(cache.catalog_path("99"), b"catalog").unwrap();

        let (deleted, errors) = clear_all_sidecars(&cache).unwrap();
        assert_eq!(errors, 0);
        assert_eq!(deleted, 3, "both per-file sidecars and the team catalog");
        assert!(!cache.meta_path("abc").exists());
        assert!(!cache.file_path("abc").exists());
        assert!(!cache.catalog_path("99").exists());
        // The directories themselves survive — only their contents go.
        assert!(cache.files_dir().exists());
        assert!(cache.teams_dir().exists());
    }

    #[test]
    fn clear_all_sidecars_preserves_root_state_files() {
        // `synth.json` and `marks.json` live in the cache root, not under
        // `files/`/`teams/`, so a clear must leave them intact — otherwise
        // stable synth ids and curated marks would evaporate on every clear.
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        std::fs::write(cache.file_path("abc"), b"payload").unwrap();
        let synth_json = cache.root.join("synth.json");
        let marks_json = cache.root.join("marks.json");
        std::fs::write(&synth_json, br#"{"version":1,"projects":{},"files":{}}"#).unwrap();
        std::fs::write(&marks_json, br#"{"version":1,"marks":[]}"#).unwrap();

        clear_all_sidecars(&cache).unwrap();

        assert!(!cache.file_path("abc").exists(), "file payload swept");
        assert!(synth_json.exists(), "synth.json preserved across clear");
        assert!(marks_json.exists(), "marks.json preserved across clear");
    }
}

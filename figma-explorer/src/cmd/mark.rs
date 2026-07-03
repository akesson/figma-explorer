//! `mark` — create, remove, and list curated keyword→node marks.
//!
//! A mark is a durable handle (`mark:<key>`) an agent writes down once it has
//! positively identified a Figma node, so the expensive discovery never has to
//! repeat. `find` and `library search` fold matching marks in ahead of their
//! own hits; `node-info mark:k` / `screenshot mark:k` resolve straight through.
//! See [`crate::marks`] for the storage and freshness model.

use std::collections::HashSet;

use anyhow::{anyhow, bail, Result};
use clap::{Args as ClapArgs, Subcommand};
use figma_api::apis::configuration::Configuration;
use serde_json::json;

use crate::cache::{self, CacheDir};
use crate::marks::{self, Mark, MarkNode, MarkStore, ScoredMarkView, Stamp};
use crate::resolver::{parse_id, render_resolve_error, ResolvedTarget, Resolver};
use crate::synth::SynthState;
use crate::tree::{truncate_display, NAME_DISPLAY_MAX};
use crate::{print, Globals, Output};

/// Maintain curated keyword→node marks.
#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: MarkCommand,
}

#[derive(Subcommand, Debug)]
pub enum MarkCommand {
    /// Create (or overwrite with `--force`) a mark pointing at one or more nodes.
    Add(AddArgs),
    /// Remove a mark by key.
    Rm(RmArgs),
    /// List every mark with its nodes and freshness.
    List(ListArgs),
}

#[derive(ClapArgs, Debug)]
pub struct AddArgs {
    /// Mark key: the handle you resolve later as `mark:<key>`. Only
    /// `[A-Za-z0-9._-]` — no `:` (would break the id grammar), no whitespace.
    pub key: String,

    /// One or more node targets to mark (tagged ids or Figma URLs). Each must
    /// resolve to a node; files/projects/comments are rejected.
    #[arg(required = true, num_args = 1..)]
    pub ids: Vec<String>,

    /// Extra search word that should surface this mark (repeatable). This is
    /// the vocabulary bridge — add the words you'd actually search for.
    #[arg(long)]
    pub alias: Vec<String>,

    /// Freeform note, shown in `mark list` and folded into search.
    #[arg(long)]
    pub note: Option<String>,

    /// Overwrite an existing mark with the same key instead of erroring.
    #[arg(long)]
    pub force: bool,
}

#[derive(ClapArgs, Debug)]
pub struct RmArgs {
    /// Key of the mark to remove.
    pub key: String,
}

#[derive(ClapArgs, Debug)]
pub struct ListArgs {}

impl Args {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        match self.command {
            MarkCommand::Add(a) => a.run(cfg, globals).await,
            MarkCommand::Rm(a) => a.run(globals),
            MarkCommand::List(a) => a.run(globals),
        }
    }
}

impl AddArgs {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        if !marks::is_valid_key(&self.key) {
            bail!(
                "invalid mark key {:?}: use only [A-Za-z0-9._-] (no ':' or whitespace)",
                self.key
            );
        }

        let resolver = Resolver::new(globals.cache_only)?;

        // Resolve every target to a node and stamp it. Dedup (file_key,node_id)
        // so `mark add k file:1:2 file:1:2` doesn't double-list.
        let mut nodes: Vec<MarkNode> = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for id_str in &self.ids {
            let id = parse_id(id_str).map_err(|e| anyhow!("{e}"))?;
            let target = resolver
                .resolve(cfg, &id)
                .await
                .map_err(|e| render_resolve_error(e, globals.output))?;
            let (node_id, node_name, file_key) = match target {
                ResolvedTarget::Node { meta, node, .. } => {
                    (node.id.clone(), node.name.clone(), meta.file_key.clone())
                }
                other => bail!(
                    "mark targets must be nodes; {id_str} resolved to {}",
                    target_kind(&other)
                ),
            };

            // Capture the ancestor path from the file's cached document (the
            // resolve above guaranteed it's on disk). The same walker is used
            // by `check_freshness`, so a later `[moved]` can't be a formatting
            // artifact.
            let document = resolver
                .cache()
                .read_file(&file_key)
                .map_err(|e| anyhow!("reading cached file {file_key}: {e}"))?
                .ok_or_else(|| anyhow!("file {file_key} is not cached; run `cache prefetch`"))?
                .document;
            let path = marks::find_node_with_path(&document, &node_id)
                .map(|(_, p)| p)
                .unwrap_or_default();

            if seen.insert((file_key.clone(), node_id.clone())) {
                nodes.push(MarkNode {
                    file_key,
                    node_id,
                    stamp: Stamp {
                        name: node_name,
                        path,
                        at_epoch: cache::now_epoch(),
                    },
                });
            }
        }

        let mark = Mark {
            key: self.key,
            aliases: self.alias,
            nodes,
            note: self.note,
        };
        let node_count = mark.nodes.len();
        let key = mark.key.clone();
        let force = self.force;

        // Collision check + write happen inside the lock window so two
        // concurrent `mark add`s can't both think the key is free.
        marks::with_lock(resolver.cache(), move |store| {
            if store.get(&mark.key).is_some() && !force {
                bail!(
                    "mark:{} already exists — pass --force to overwrite, or `mark rm {}` first",
                    mark.key,
                    mark.key
                );
            }
            store.upsert(mark);
            Ok(())
        })?;

        match globals.output {
            Output::Yaml => {
                println!("# marked mark:{key} → {node_count} node(s)");
            }
            Output::Json => {
                print(
                    &json!({ "key": key, "nodes": node_count, "ok": true }),
                    Output::Json,
                )?;
            }
        }
        Ok(())
    }
}

impl RmArgs {
    pub fn run(self, globals: &Globals) -> Result<()> {
        let cache_dir = CacheDir::new(cache::default_dir());
        // `with_lock` saves only when the closure returns Ok, so an unknown-key
        // error leaves the file untouched.
        marks::with_lock(&cache_dir, |store| {
            if !store.remove(&self.key) {
                let known: Vec<&str> = store.keys().take(10).collect();
                let hint = if known.is_empty() {
                    "no marks exist".to_owned()
                } else {
                    format!("known marks: {}", known.join(", "))
                };
                bail!("no mark:{} ({hint})", self.key);
            }
            Ok(())
        })?;
        match globals.output {
            Output::Yaml => println!("# removed mark:{}", self.key),
            Output::Json => print(&json!({ "key": self.key, "removed": true }), Output::Json)?,
        }
        Ok(())
    }
}

impl ListArgs {
    pub fn run(self, globals: &Globals) -> Result<()> {
        let cache_dir = CacheDir::new(cache::default_dir());
        let store = MarkStore::load(&cache_dir)?;
        // Read-only synth lookup for paste-ready ids; a schema mismatch just
        // means some ids render as raw file_key:node_id.
        let synth = SynthState::load(&cache_dir).unwrap_or_default();
        let views = marks::all_views(&store, &cache_dir, &synth);
        let now = cache::now_epoch();

        match globals.output {
            Output::Yaml => {
                let node_total: usize = views.iter().map(|v| v.nodes.len()).sum();
                println!("# {} marks ({} nodes)", views.len(), node_total);
                if views.is_empty() {
                    println!("# none yet — `mark add <key> <ID> [--alias …] [--note …]`");
                    return Ok(());
                }
                print!("{}", render_list(&views, now));
                Ok(())
            }
            Output::Json => print(
                &json!({
                    "marks": marks::marks_json(&views),
                    "count": views.len(),
                }),
                Output::Json,
            ),
        }
    }
}

/// `mark list` YAML body: one aligned row per (mark, node) with the marked
/// name, freshness flag, stamp age, and note.
fn render_list(views: &[ScoredMarkView], now: u64) -> String {
    // Width-align the id column across every row.
    let ids: Vec<String> = views
        .iter()
        .flat_map(|v| v.nodes.iter().map(|n| n.display_id()))
        .collect();
    let id_w = ids.iter().map(|s| s.chars().count()).max().unwrap_or(1);

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
                "mark:{key}  {id:<id_w$}  | \"{name}\"{flag}  added {age}{note}\n",
                key = v.key,
                id = n.display_id(),
                name = truncate_display(&n.name, NAME_DISPLAY_MAX),
                flag = flag,
                age = human_age(now.saturating_sub(n.at_epoch)),
                note = note,
                id_w = id_w,
            ));
        }
    }
    out
}

/// Human-facing name for a non-node resolve target, for the `mark add` error.
fn target_kind(t: &ResolvedTarget) -> &'static str {
    match t {
        ResolvedTarget::Root => "the root listing",
        ResolvedTarget::Project { .. } => "a project",
        ResolvedTarget::File { .. } => "a file",
        ResolvedTarget::Comment { .. } => "a comment",
        ResolvedTarget::Node { .. } => "a node",
    }
}

/// Coarse human-readable age, e.g. `just now`, `12m ago`, `3h ago`, `2d ago`.
fn human_age(secs: u64) -> String {
    if secs < 60 {
        "just now".to_owned()
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marks::{Freshness, MarkNodeView};

    fn view(
        key: &str,
        id: &str,
        name: &str,
        fresh: Freshness,
        note: Option<&str>,
    ) -> ScoredMarkView {
        ScoredMarkView {
            key: key.into(),
            score: 0,
            note: note.map(|s| s.to_owned()),
            nodes: vec![MarkNodeView {
                file_key: "F".into(),
                node_id: "1:1".into(),
                name: name.into(),
                freshness: fresh,
                synth_id: Some(id.into()),
                at_epoch: 0,
            }],
        }
    }

    #[test]
    fn render_list_shows_key_id_name_flag_and_age() {
        let views = vec![view(
            "cell",
            "file:1:1:1",
            "Cell",
            Freshness::Renamed {
                current: "New".into(),
            },
            Some("hover card"),
        )];
        let out = render_list(&views, 7_200);
        assert!(out.contains("mark:cell"));
        assert!(out.contains("file:1:1:1"));
        assert!(out.contains("\"Cell\""));
        assert!(out.contains("[renamed → \"New\"]"));
        assert!(out.contains("added 2h ago"));
        assert!(out.contains("— hover card"));
    }

    #[test]
    fn human_age_buckets() {
        assert_eq!(human_age(10), "just now");
        assert_eq!(human_age(120), "2m ago");
        assert_eq!(human_age(7_200), "2h ago");
        assert_eq!(human_age(172_800), "2d ago");
    }
}

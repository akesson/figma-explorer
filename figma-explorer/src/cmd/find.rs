//! `find` — multi-token ancestor-chain name search. Replaces the legacy
//! `search` command.
//!
//! Scopes:
//! - No `--in` → walk every cached file (status:Ok).
//! - `--in file:N` → walk inside that file.
//! - `--in file:N:x:y` → walk inside that subtree.
//!
//! Output uses the same flat pipe format as `ls`: every line's first token
//! is a paste-ready `file:N:x:y` ID. Hits are ranked by aggregate score
//! across the (possibly cross-file) result set.

use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::{json, Value};

use crate::cache::EntryStatus;
use crate::resolve::{multi_token_search, SearchHit};
use crate::resolver::{parse_id, render_resolve_error, ResolvedTarget, Resolver};
use crate::tree::{truncate_display, NAME_DISPLAY_MAX};
use crate::{print, Globals, Output};

/// Locate nodes by a multi-token ancestor-chain query. Each whitespace
/// token must fuzzy-match some ancestor name on the root→node path; leaf
/// hits rank highest. Top `--limit` hits returned.
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Query phrase (one or more words). Each whitespace-separated token must
    /// fuzzy-match some ancestor name for a node to be a hit.
    #[arg(required = true, num_args = 1..)]
    pub query: Vec<String>,

    /// Restrict to specific node types (e.g. `FRAME,INSTANCE`). Default: all.
    #[arg(long, value_delimiter = ',')]
    pub r#type: Vec<String>,

    /// Maximum number of hits to report.
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        let resolver = Resolver::new(globals.cache_only)?;
        let format = globals.output;
        // `--in <ID>` is the global scope override; `find` reads it from
        // there rather than reintroducing a per-command flag.
        let in_ = globals.scope.as_deref();

        let joined = self.query.join(" ");
        let tokens: Vec<&str> = joined.split_whitespace().collect();
        if tokens.is_empty() {
            anyhow::bail!("query is empty");
        }

        let type_refs: Vec<&str> = self.r#type.iter().map(String::as_str).collect();
        let type_filter = if type_refs.is_empty() { None } else { Some(type_refs.as_slice()) };

        // Collect hits across the requested scope. For each cached file we
        // run multi_token_search inside, tagging hits with the file's synth
        // so we can emit qualified IDs at render time.
        let mut all_hits: Vec<ScopedHit> = Vec::new();

        match in_ {
            Some(scope_str) => {
                let id = parse_id(scope_str).map_err(|e| anyhow!("{e}"))?;
                let target = resolver
                    .resolve(cfg, &id)
                    .await
                    .map_err(|e| render_resolve_error(e, format))?;
                match target {
                    ResolvedTarget::File { synth, document, .. } => {
                        let hits = multi_token_search(
                            &document.document,
                            &tokens,
                            type_filter,
                            self.limit,
                        );
                        for h in hits {
                            all_hits.push(scoped_from_hit(synth, &h));
                        }
                    }
                    ResolvedTarget::Node { file_synth, node, .. } => {
                        let hits = multi_token_search(&node, &tokens, type_filter, self.limit);
                        for h in hits {
                            all_hits.push(scoped_from_hit(file_synth, &h));
                        }
                    }
                    ResolvedTarget::Project { .. } | ResolvedTarget::Root => {
                        anyhow::bail!(
                            "--in must be a file or node scope (got {scope_str}); use no --in for cross-file search"
                        );
                    }
                }
            }
            None => {
                // No scope — search every cached file. Per-file results are
                // already capped at `limit`; we merge then re-cap.
                let synth = resolver.synth();
                let metas = resolver.cache().list_metas()?;
                for m in metas.iter().filter(|m| m.status == EntryStatus::Ok) {
                    let Some(file_synth) = synth.file_synth(&m.file_key) else { continue };
                    let payload = match resolver.cache().read_file(&m.file_key) {
                        Ok(Some(p)) => p,
                        _ => continue,
                    };
                    let hits =
                        multi_token_search(&payload.document, &tokens, type_filter, self.limit);
                    for h in hits {
                        all_hits.push(scoped_from_hit(file_synth, &h));
                    }
                }
                // Sort and truncate across files.
                all_hits.sort_by(|a, b| {
                    b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
                });
                all_hits.truncate(self.limit);
            }
        }

        // Render. Output format mirrors `ls` (id-first, qualified) so a
        // user can grab any line's first column and paste it into another
        // command. Score and path are tail columns.
        match format {
            Output::Yaml => {
                if all_hits.is_empty() {
                    return Ok(());
                }
                let max_id = all_hits.iter().map(|h| h.id.len()).max().unwrap_or(0);
                let max_b = all_hits.iter().map(|h| h.bounds.len()).max().unwrap_or(1);
                let mut out = String::new();
                for h in &all_hits {
                    // Truncate each path component independently so a single
                    // pathological auto-named TEXT node doesn't blow up the
                    // path string. JSON path (below) keeps full data.
                    let path_truncated: String = h
                        .path_components
                        .iter()
                        .map(|c| truncate_display(c, NAME_DISPLAY_MAX).into_owned())
                        .collect::<Vec<_>>()
                        .join(" > ");
                    out.push_str(&format!(
                        "{id:<id_w$}  {b:<b_w$}  | {kind}  {score:>4.1}  \"{name}\"  ({path})\n",
                        id = h.id,
                        b = h.bounds,
                        kind = h.kind,
                        score = h.score,
                        name = truncate_display(&h.name, NAME_DISPLAY_MAX),
                        path = path_truncated,
                        id_w = max_id,
                        b_w = max_b,
                    ));
                }
                print!("{out}");
                Ok(())
            }
            Output::Json => {
                let hits: Vec<Value> = all_hits
                    .iter()
                    .map(|h| {
                        json!({
                            "id": h.id,
                            "name": h.name,
                            "type": h.kind,
                            "score": h.score,
                            "path": h.path_components.join(" > "),
                            "path_components": h.path_components,
                        })
                    })
                    .collect();
                print(
                    &json!({
                        "query": joined,
                        "tokens": tokens,
                        "scope": in_,
                        "hits": hits,
                    }),
                    format,
                )
            }
        }
    }
}

/// Flattened, render-ready hit. We materialize names/bounds/path strings
/// here so cross-file ranking works without borrowed `CacheNode`s outliving
/// their `CachedFile` payloads.
///
/// `path_components` is the raw per-ancestor name list (joined with " > " at
/// render time). YAML rendering truncates each component independently so a
/// single 1000-char auto-named TEXT node doesn't drown the line; JSON output
/// keeps the unmodified path so machine consumers see the real data.
struct ScopedHit {
    id: String,
    bounds: String,
    kind: String,
    score: f64,
    name: String,
    path_components: Vec<String>,
}

fn scoped_from_hit(file_synth: u32, hit: &SearchHit<'_>) -> ScopedHit {
    let node = hit.node;
    let id = format!("file:{file_synth}:{}", node.id);
    let bounds = node.bounds.map(|b| b.compact()).unwrap_or_else(|| "-".to_owned());
    let path_components: Vec<String> = hit
        .path
        .iter()
        .map(|n| n.name.clone())
        .filter(|s| !s.is_empty())
        .collect();
    ScopedHit {
        id,
        bounds,
        kind: if node.type_.is_empty() { "?".to_owned() } else { node.type_.clone() },
        score: round_one(hit.score),
        name: node.name.clone(),
        path_components,
    }
}

fn round_one(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

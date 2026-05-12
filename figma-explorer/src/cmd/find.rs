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
    #[arg(long, default_value_t = 100)]
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
        let type_filter = if type_refs.is_empty() {
            None
        } else {
            Some(type_refs.as_slice())
        };

        // Collect hits across the requested scope. For each cached file we
        // run multi_token_search inside, tagging hits with the file's synth
        // so we can emit qualified IDs at render time. We pass `usize::MAX`
        // as the per-search cap so per-file truncation doesn't hide hits
        // that a tied score from another file might otherwise displace.
        // After the merge we run a global sort + `dedupe_descendants` pass;
        // `total_matches` then reports the retained-anchor count, with each
        // anchor carrying the count of descendants rolled up onto it.
        let mut all_hits: Vec<ScopedHit> = Vec::new();

        match in_ {
            Some(scope_str) => {
                let id = parse_id(scope_str).map_err(|e| anyhow!("{e}"))?;
                let target = resolver
                    .resolve(cfg, &id)
                    .await
                    .map_err(|e| render_resolve_error(e, format))?;
                match target {
                    ResolvedTarget::File {
                        synth, document, ..
                    } => {
                        let hits = multi_token_search(
                            &document.document,
                            &tokens,
                            type_filter,
                            usize::MAX,
                        );
                        for h in hits {
                            all_hits.push(scoped_from_hit(synth, &h));
                        }
                    }
                    ResolvedTarget::Node {
                        file_synth, node, ..
                    } => {
                        let hits = multi_token_search(&node, &tokens, type_filter, usize::MAX);
                        for h in hits {
                            all_hits.push(scoped_from_hit(file_synth, &h));
                        }
                    }
                    ResolvedTarget::Project { .. } | ResolvedTarget::Root => {
                        anyhow::bail!(
                            "--in must be a file or node scope (got {scope_str}); use no --in for cross-file search"
                        );
                    }
                    ResolvedTarget::Comment { .. } => {
                        anyhow::bail!(
                            "--in cannot scope to a comment ({scope_str}); use a file or node id"
                        );
                    }
                }
            }
            None => {
                // No scope — search every cached file. Per-file results are
                // unbounded here so a single file can't monopolize the global
                // top-N via score ties.
                let synth = resolver.synth();
                let metas = resolver.cache().list_metas()?;
                for m in metas.iter().filter(|m| m.status == EntryStatus::Ok) {
                    let Some(file_synth) = synth.file_synth(&m.file_key) else {
                        continue;
                    };
                    let payload = match resolver.cache().read_file(&m.file_key) {
                        Ok(Some(p)) => p,
                        _ => continue,
                    };
                    let hits =
                        multi_token_search(&payload.document, &tokens, type_filter, usize::MAX);
                    for h in hits {
                        all_hits.push(scoped_from_hit(file_synth, &h));
                    }
                }
            }
        }

        // Sort the merged hits by score descending, then collapse any hit
        // whose ancestor (in the same file) already sits above it in the
        // ranking. The dedup pass rolls the suppressed counts onto the
        // surviving anchors so `[+N hits]` can be displayed.
        all_hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all_hits = dedupe_descendants(all_hits);

        let total_matches = all_hits.len();
        all_hits.truncate(self.limit);

        // Render. Output format mirrors `ls` (id-first, qualified) so a
        // user can grab any line's first column and paste it into another
        // command. Score and path are tail columns.
        let truncated = total_matches > all_hits.len();
        match format {
            Output::Yaml => {
                if truncated {
                    println!(
                        "# showing {} of {} anchors — use --limit N to see more",
                        all_hits.len(),
                        total_matches
                    );
                }
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
                    let suffix = if h.suppressed_count > 0 {
                        format!("  [+{} hits]", h.suppressed_count)
                    } else {
                        String::new()
                    };
                    out.push_str(&format!(
                        "{id:<id_w$}  {b:<b_w$}  | {kind}  {score:>4.1}  \"{name}\"  ({path}){suffix}\n",
                        id = h.id,
                        b = h.bounds,
                        kind = h.kind,
                        score = h.score,
                        name = truncate_display(&h.name, NAME_DISPLAY_MAX),
                        path = path_truncated,
                        suffix = suffix,
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
                            "suppressed_descendants": h.suppressed_count,
                        })
                    })
                    .collect();
                print(
                    &json!({
                        "query": joined,
                        "tokens": tokens,
                        "scope": in_,
                        "total_matches": total_matches,
                        "shown": all_hits.len(),
                        "truncated": truncated,
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
///
/// `file_synth` + `node_id` identify the hit; `ancestor_node_ids` carries the
/// root→parent chain so `dedupe_descendants` can detect when a higher-ranked
/// hit is this one's ancestor in the same file. `suppressed_count` is the
/// running tally of descendants the dedup pass rolled up onto this anchor.
struct ScopedHit {
    id: String,
    bounds: String,
    kind: String,
    score: f64,
    name: String,
    path_components: Vec<String>,
    file_synth: u32,
    node_id: String,
    ancestor_node_ids: Vec<String>,
    suppressed_count: usize,
}

fn scoped_from_hit(file_synth: u32, hit: &SearchHit<'_>) -> ScopedHit {
    let node = hit.node;
    let id = format!("file:{file_synth}:{}", node.id);
    let bounds = node
        .bounds
        .map(|b| b.compact())
        .unwrap_or_else(|| "-".to_owned());
    // Trim the path for display: the trailing element is the matched node
    // itself (already in the "name" column), and the leading DOCUMENT node
    // is the same on every line. The canvas name and below are what carry
    // location information.
    let end = hit.path.len().saturating_sub(1);
    let start = if hit.path.first().is_some_and(|n| n.type_ == "DOCUMENT") {
        1
    } else {
        0
    };
    let path_components: Vec<String> = if start < end {
        hit.path[start..end]
            .iter()
            .map(|n| n.name.clone())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        Vec::new()
    };
    // For dedup we need the full ancestor id chain (root → parent, excluding
    // the hit itself). DOCUMENT id stays in — ancestor detection compares ids,
    // so it costs nothing and keeps `dedupe_descendants` purely topological.
    let ancestor_node_ids: Vec<String> = if hit.path.len() > 1 {
        hit.path[..hit.path.len() - 1]
            .iter()
            .map(|n| n.id.clone())
            .collect()
    } else {
        Vec::new()
    };
    ScopedHit {
        id,
        bounds,
        kind: if node.type_.is_empty() {
            "?".to_owned()
        } else {
            node.type_.clone()
        },
        score: round_one(hit.score),
        name: node.name.clone(),
        path_components,
        file_synth,
        node_id: node.id.clone(),
        ancestor_node_ids,
        suppressed_count: 0,
    }
}

fn round_one(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Collapse the result set so any hit whose ancestor (within the same file)
/// is already retained gets folded into that ancestor's `suppressed_count`.
/// Input must be sorted by score descending — the first hit on a given chain
/// is taken as the anchor and absorbs every later descendant.
///
/// The deepest retained ancestor wins the rollup: when a hit has two retained
/// ancestors on its chain, the count goes on the one closest to it in the
/// path, which is the most useful anchor for the user to drill into next.
fn dedupe_descendants(hits: Vec<ScopedHit>) -> Vec<ScopedHit> {
    use std::collections::HashMap;
    let mut kept: Vec<ScopedHit> = Vec::with_capacity(hits.len());
    let mut index: HashMap<(u32, String), usize> = HashMap::with_capacity(hits.len());
    for h in hits {
        // ancestor_node_ids is root → parent. Reverse so the closest ancestor
        // is tested first; whichever retained ancestor is deepest in the path
        // absorbs the count.
        let suppressor = h
            .ancestor_node_ids
            .iter()
            .rev()
            .find_map(|a| index.get(&(h.file_synth, a.clone())).copied());
        match suppressor {
            Some(idx) => kept[idx].suppressed_count += 1,
            None => {
                index.insert((h.file_synth, h.node_id.clone()), kept.len());
                kept.push(h);
            }
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `ScopedHit` with just the fields `dedupe_descendants` looks at —
    /// score, file, node id, and ancestor chain. The display fields stay empty
    /// because the dedup pass never reads them.
    fn hit(file: u32, node_id: &str, ancestors: &[&str], score: f64) -> ScopedHit {
        ScopedHit {
            id: format!("file:{file}:{node_id}"),
            bounds: "-".into(),
            kind: "FRAME".into(),
            score,
            name: node_id.into(),
            path_components: Vec::new(),
            file_synth: file,
            node_id: node_id.into(),
            ancestor_node_ids: ancestors.iter().map(|s| (*s).into()).collect(),
            suppressed_count: 0,
        }
    }

    /// Hits in disjoint subtrees should all survive untouched.
    #[test]
    fn dedupe_keeps_unrelated_hits() {
        let hits = vec![
            hit(1, "1:1", &["0:0", "0:1"], 100.0),
            hit(1, "1:2", &["0:0", "0:2"], 90.0),
            hit(1, "1:3", &["0:0", "0:3"], 80.0),
        ];
        let kept = dedupe_descendants(hits);
        assert_eq!(kept.len(), 3, "expected all three retained");
        assert!(kept.iter().all(|h| h.suppressed_count == 0));
    }

    /// One high-scoring anchor with three descendants beneath it: only the
    /// anchor survives, carrying a suppressed_count of 3.
    #[test]
    fn dedupe_suppresses_descendants_and_counts_them() {
        let hits = vec![
            hit(1, "1:1", &["0:0"], 100.0),              // anchor
            hit(1, "2:1", &["0:0", "1:1"], 50.0),        // child
            hit(1, "3:1", &["0:0", "1:1", "2:1"], 40.0), // grandchild
            hit(1, "3:2", &["0:0", "1:1", "2:1"], 30.0), // grandchild
        ];
        let kept = dedupe_descendants(hits);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].node_id, "1:1");
        assert_eq!(kept[0].suppressed_count, 3);
    }

    /// When a hit has two retained ancestors on its chain, the count must
    /// roll up onto the *deepest* one (closest to the hit) — it's the most
    /// useful anchor for the user to drill into next.
    #[test]
    fn dedupe_picks_deepest_retained_ancestor() {
        // A is an ancestor of B; both retained. Then C is a descendant of B
        // (and transitively of A). C must bump B's count, not A's.
        let hits = vec![
            hit(1, "A", &["0:0"], 100.0),     // retained
            hit(1, "B", &["0:0", "A"], 99.0), // retained (B not a descendant of A in the score order? — it IS, so it'll be suppressed under A)
            hit(1, "C", &["0:0", "A", "B"], 50.0),
        ];
        // With our rules, B is a descendant of A in the same file, so B is
        // suppressed under A. Then C is also suppressed — and since B is no
        // longer retained, C rolls up onto A. Both end up on A.
        let kept = dedupe_descendants(hits);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].node_id, "A");
        assert_eq!(kept[0].suppressed_count, 2);
    }

    /// A more interesting "deepest wins" case: two retained anchors that are
    /// in disjoint branches (so neither is the other's descendant). Then a
    /// hit lands beneath the second. Its count must go on that second anchor.
    #[test]
    fn dedupe_rolls_up_to_correct_anchor_among_siblings() {
        let hits = vec![
            hit(1, "A", &["0:0"], 100.0),     // retained, branch 1
            hit(1, "B", &["0:0"], 90.0),      // retained, branch 2 (sibling of A)
            hit(1, "C", &["0:0", "B"], 50.0), // descendant of B only
        ];
        let kept = dedupe_descendants(hits);
        assert_eq!(kept.len(), 2);
        let a = kept.iter().find(|h| h.node_id == "A").unwrap();
        let b = kept.iter().find(|h| h.node_id == "B").unwrap();
        assert_eq!(a.suppressed_count, 0);
        assert_eq!(b.suppressed_count, 1);
    }

    /// `dedupe_descendants` keys on (file_synth, node_id). The same node id
    /// in two different files must not collapse — files are independent
    /// search spaces.
    #[test]
    fn dedupe_cross_file_isolation() {
        let hits = vec![
            hit(1, "1:1", &["0:0"], 100.0),
            hit(2, "1:1", &["0:0"], 100.0),
        ];
        let kept = dedupe_descendants(hits);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|h| h.suppressed_count == 0));
    }
}

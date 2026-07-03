//! `library search` — fuzzy text search over the team-library catalog.
//!
//! Unlike `find` (which searches node names inside one cached file), this
//! searches the *published* team-library catalog: every component, component
//! set, and style across the team's libraries. The catalog is fetched from
//! Figma's team-library endpoints and cached team-scoped (see
//! [`crate::team_catalog`]); `search` refreshes it lazily.
//!
//! Variables are not indexed — the Variables REST API is Enterprise-gated, so
//! color tokens (which live in variables) are not searchable here.

use anyhow::{anyhow, Result};
use clap::{Args as ClapArgs, Subcommand};
use figma_api::apis::configuration::Configuration;
use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Matcher, Utf32Str,
};
use serde_json::{json, Value};

use crate::cache::{self, CacheDir};
use crate::synth::SynthState;
use crate::team_catalog::{self, CatalogEntry, EntryKind, TeamCatalog};
use crate::tree::{truncate_display, NAME_DISPLAY_MAX};
use crate::{print, Globals, Output};

/// Search the team-library catalog (components, component sets, styles).
#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: LibraryCommand,
}

#[derive(Subcommand, Debug)]
pub enum LibraryCommand {
    /// Fuzzy-search the team-library catalog by component/style name.
    ///
    /// The catalog is cached team-scoped and refreshed lazily (re-fetched
    /// when older than 24h, or on `--refresh`). Variables are not indexed —
    /// the Variables REST API is Enterprise-gated.
    Search(SearchArgs),
}

#[derive(ClapArgs, Debug)]
pub struct SearchArgs {
    /// Search phrase (one or more words). Each word is fuzzy-matched against
    /// asset names; all words must match.
    #[arg(required = true, num_args = 1..)]
    pub query: Vec<String>,

    /// Figma team id. Falls back to the FIGMA_TEAM_ID environment variable.
    /// Found in a team URL: figma.com/files/team/<TEAM_ID>/...
    #[arg(long, env = "FIGMA_TEAM_ID", hide_env_values = true)]
    pub team_id: Option<String>,

    /// Restrict to one asset kind. Default: all three.
    #[arg(long, value_name = "KIND")]
    pub r#type: Option<EntryKind>,

    /// Maximum number of hits to report.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,

    /// Re-fetch the catalog from Figma even when a fresh cached copy exists.
    #[arg(long)]
    pub refresh: bool,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        match self.command {
            LibraryCommand::Search(a) => a.run(cfg, globals).await,
        }
    }
}

impl SearchArgs {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        let format = globals.output;

        let team_id = self.team_id.clone().ok_or_else(|| {
            anyhow!(
                "no team id. Set FIGMA_TEAM_ID or pass --team-id. Find it in a \
                 team URL: figma.com/files/team/<TEAM_ID>/..."
            )
        })?;

        let joined = self.query.join(" ");
        let needle = joined.trim();
        if needle.is_empty() {
            anyhow::bail!("query is empty");
        }

        let cache_dir = CacheDir::new(cache::default_dir());
        let catalog =
            load_catalog(cfg, &cache_dir, &team_id, self.refresh, globals.cache_only).await?;

        // Read-only synth lookup so hits can carry a paste-ready `file:N:x:y`
        // id for any source file the cache already knows. A synth schema
        // mismatch degrades to "no resolvable ids" rather than aborting.
        let synth = SynthState::load(&cache_dir).unwrap_or_default();

        let (mut hits, self_score) = search_catalog(&catalog, needle, self.r#type);
        // When any hit clears the strong floor, drop the weak ones (with a
        // transparency line). When none do, keep a few weak leads, labeled.
        let any_strong = hits.iter().any(|h| h.strong);
        let no_strong_match = !hits.is_empty() && !any_strong;
        let weak_hidden = if any_strong {
            hits.iter().filter(|h| !h.strong).count()
        } else {
            0
        };
        if any_strong {
            hits.retain(|h| h.strong);
        }
        let total_matches = hits.len();
        let cap = if no_strong_match {
            WEAK_HITS_SHOWN.min(self.limit)
        } else {
            self.limit
        };
        hits.truncate(cap);

        let outcome = SearchOutcome {
            no_strong_match,
            self_score,
            weak_hidden,
        };
        render(
            &catalog,
            &hits,
            total_matches,
            &outcome,
            needle,
            &synth,
            format,
        )
    }
}

/// A hit counts as "strong" when its raw score reaches this fraction of the
/// query's self-score (the parsed pattern scored against the query text itself
/// — the achievable maximum for whitespace-separated alphanumeric queries; see
/// nucleo-matcher `score.rs`). Calibration on real catalog names: variant-name
/// junk for "textarea" (`"Text=False, Checkbox=True, …"`) lands at 0.66–0.80;
/// genuine word-boundary matches score ≥0.98; single-dropped-letter typos
/// ≈0.89. camelCase word-starts (≈0.79) fall below and surface as labeled weak
/// leads rather than confident hits. Tunable.
const STRONG_THRESHOLD_FRACTION: f64 = 0.85;

/// How many weak hits to surface (clearly labeled) when nothing is strong — an
/// honest miss still gives the user a few leads to chase.
const WEAK_HITS_SHOWN: usize = 3;

/// A catalog entry that matched the query, with its fuzzy score.
struct ScoredEntry<'a> {
    entry: &'a CatalogEntry,
    score: u32,
    /// Whether `score` clears [`STRONG_THRESHOLD_FRACTION`] of the query's
    /// self-score. Weak hits are hidden when any strong hit exists.
    strong: bool,
}

/// Strong/weak classification outcome threaded from `run` into `render`.
struct SearchOutcome {
    /// True when matches exist but none cleared the strong floor — the render
    /// switches to the "no strong match, here are weak leads" path.
    no_strong_match: bool,
    /// The query's self-score (score ceiling); surfaced in JSON for tuning.
    self_score: u32,
    /// Count of weak hits dropped because strong hits existed. Drives the
    /// `# N weaker matches hidden` transparency line.
    weak_hidden: usize,
}

/// Cache-first catalog load. Returns the cached catalog when it's present,
/// fresh, and `--refresh` wasn't given; otherwise fetches it live and
/// rewrites the sidecar. Under `--cache-only` a live fetch is refused: a
/// stale catalog is served with a warning, a missing one is a hard error.
async fn load_catalog(
    cfg: &Configuration,
    cache_dir: &CacheDir,
    team_id: &str,
    refresh: bool,
    cache_only: bool,
) -> Result<TeamCatalog> {
    let cached = team_catalog::read_catalog(cache_dir, team_id)?;
    let now = cache::now_epoch();

    let needs_fetch = match &cached {
        Some(c) => {
            refresh || team_catalog::is_stale(c.fetched_at_epoch, now, cache::CATALOG_TTL_SECS)
        }
        None => true,
    };
    if !needs_fetch {
        return Ok(cached.expect("needs_fetch is false only when cached is Some"));
    }

    if cache_only {
        return match cached {
            Some(c) => {
                eprintln!(
                    "library: cached catalog is {} and --cache-only is set; using it as-is",
                    human_age(now.saturating_sub(c.fetched_at_epoch)),
                );
                Ok(c)
            }
            None => Err(anyhow!(
                "no cached catalog for team {team_id} and --cache-only is set. \
                 Drop --cache-only (or run `cache prefetch`) to fetch it."
            )),
        };
    }

    eprintln!("library: fetching team-library catalog…");
    let mut catalog = team_catalog::fetch_team_catalog(cfg, team_id)
        .await
        .map_err(augment_fetch_error)?;
    let bytes = team_catalog::write_catalog(cache_dir, &mut catalog)?;
    eprintln!(
        "library: cached {} catalog entries ({} KB) for team {team_id}",
        catalog.total(),
        bytes / 1024,
    );
    Ok(catalog)
}

/// Add a remediation hint to a catalog fetch error when it looks like a
/// missing-scope / 403 failure.
fn augment_fetch_error(e: anyhow::Error) -> anyhow::Error {
    let msg = format!("{e:#}").to_lowercase();
    if msg.contains("403") || msg.contains("scope") {
        e.context(
            "the token may lack the team-library scopes — regenerate it with \
             team_library_content:read (https://www.figma.com/developers/api#access-tokens)",
        )
    } else {
        e
    }
}

/// Fuzzy-rank every catalog entry whose `name` matches `needle`. Higher score
/// first; ties broken by shorter name, then name ascending, then key — fully
/// deterministic. Returns *all* matches plus the query's self-score (the
/// achievable maximum, used to classify hits as strong/weak); the caller
/// truncates to `--limit`.
fn search_catalog<'a>(
    catalog: &'a TeamCatalog,
    needle: &str,
    kind: Option<EntryKind>,
) -> (Vec<ScoredEntry<'a>>, u32) {
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = Pattern::parse(needle, CaseMatching::Ignore, Normalization::Smart);
    let mut buf: Vec<char> = Vec::new();

    // The pattern scored against the query text itself: for whitespace-
    // separated alphanumeric queries this is the exact score ceiling, so every
    // real hit falls at or below it. Zero (unusual — e.g. fzf operator syntax)
    // disables the floor: everything classifies strong, i.e. today's behavior.
    let mut self_buf: Vec<char> = Vec::new();
    let self_score = pattern
        .score(Utf32Str::new(needle, &mut self_buf), &mut matcher)
        .unwrap_or(0);
    let strong_floor = STRONG_THRESHOLD_FRACTION * self_score as f64;

    let mut hits: Vec<ScoredEntry<'a>> = Vec::new();
    for k in [
        EntryKind::Component,
        EntryKind::ComponentSet,
        EntryKind::Style,
    ] {
        if kind.is_some_and(|want| want != k) {
            continue;
        }
        for entry in catalog.entries(k) {
            buf.clear();
            let haystack = Utf32Str::new(&entry.name, &mut buf);
            if let Some(score) = pattern.score(haystack, &mut matcher) {
                let strong = self_score == 0 || score as f64 >= strong_floor;
                hits.push(ScoredEntry {
                    entry,
                    score,
                    strong,
                });
            }
        }
    }

    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| {
                a.entry
                    .name
                    .chars()
                    .count()
                    .cmp(&b.entry.name.chars().count())
            })
            .then_with(|| a.entry.name.cmp(&b.entry.name))
            .then_with(|| a.entry.key.cmp(&b.entry.key))
    });
    (hits, self_score)
}

/// The paste-ready `file:N:node_id` for an entry's source file, when that file
/// has an interned synth. `None` when the file isn't in the synth table (it's
/// not part of any prefetched project).
fn resolvable_id(entry: &CatalogEntry, synth: &SynthState) -> Option<String> {
    synth
        .file_synth(&entry.file_key)
        .map(|n| format!("file:{n}:{}", entry.node_id))
}

/// Display label for the `kind` column. Styles append their style type
/// (`STYLE:EFFECT`) since that's what distinguishes them.
fn kind_label(entry: &CatalogEntry) -> String {
    match (entry.kind, &entry.style_type) {
        (EntryKind::Style, Some(st)) => format!("STYLE:{st}"),
        (kind, _) => kind.label().to_owned(),
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

#[allow(clippy::too_many_arguments)]
fn render(
    catalog: &TeamCatalog,
    hits: &[ScoredEntry<'_>],
    total_matches: usize,
    outcome: &SearchOutcome,
    needle: &str,
    synth: &SynthState,
    format: Output,
) -> Result<()> {
    let age = cache::now_epoch().saturating_sub(catalog.fetched_at_epoch);
    let truncated = total_matches > hits.len();

    match format {
        Output::Yaml => {
            println!(
                "# catalog: {} components · {} sets · {} styles · fetched {}",
                catalog.components.len(),
                catalog.component_sets.len(),
                catalog.styles.len(),
                human_age(age),
            );
            if outcome.no_strong_match {
                // Every match was weak — lead with an honest miss, then show a
                // few closest leads (labeled `(weak)` in the rows below).
                println!(
                    "# no strong match for \"{needle}\" — showing {} closest weak hit{} (of {total_matches})",
                    hits.len(),
                    if hits.len() == 1 { "" } else { "s" },
                );
            } else if truncated {
                println!(
                    "# showing {} of {} matches — use --limit N to see more",
                    hits.len(),
                    total_matches,
                );
            }
            if outcome.weak_hidden > 0 {
                let label = if outcome.weak_hidden == 1 {
                    "match"
                } else {
                    "matches"
                };
                println!("# {} weaker {label} hidden", outcome.weak_hidden);
            }
            if hits.is_empty() {
                // Distinguish a genuine zero-match search from `--limit 0`,
                // where the "showing 0 of N" line above already told the story.
                if total_matches == 0 {
                    println!("# no matches for \"{needle}\"");
                }
                return Ok(());
            }
            // Resolve ids first so the id column can be width-aligned.
            let ids: Vec<String> = hits
                .iter()
                .map(|h| resolvable_id(h.entry, synth).unwrap_or_else(|| "—".to_owned()))
                .collect();
            let id_w = ids.iter().map(|s| s.chars().count()).max().unwrap_or(1);
            let mut out = String::new();
            for (h, id) in hits.iter().zip(&ids) {
                let mut tail = format!("key={}", h.entry.key);
                if let Some(set) = &h.entry.component_set {
                    tail.push_str(&format!(
                        "  set=\"{}\"",
                        truncate_display(set, NAME_DISPLAY_MAX)
                    ));
                }
                if let Some(page) = &h.entry.page_name {
                    tail.push_str(&format!(
                        "  page=\"{}\"",
                        truncate_display(page, NAME_DISPLAY_MAX)
                    ));
                }
                // When the source file has no synth, the id column is `—`, so
                // surface the raw file_key:node_id the user needs to chase it.
                if id == "—" {
                    tail.push_str(&format!("  file={}:{}", h.entry.file_key, h.entry.node_id));
                }
                // In the no-strong-match path every shown hit is a weak lead.
                if outcome.no_strong_match {
                    tail.push_str("  (weak)");
                }
                out.push_str(&format!(
                    "{id:<id_w$}  | {kind:<13}  {score:>5}  \"{name}\"  {tail}\n",
                    id = id,
                    kind = kind_label(h.entry),
                    score = h.score,
                    name = truncate_display(&h.entry.name, NAME_DISPLAY_MAX),
                    tail = tail,
                    id_w = id_w,
                ));
            }
            print!("{out}");
            Ok(())
        }
        Output::Json => {
            let hit_values: Vec<Value> = hits
                .iter()
                .map(|h| {
                    json!({
                        "id": resolvable_id(h.entry, synth),
                        "name": h.entry.name,
                        "kind": kind_label(h.entry),
                        "score": h.score,
                        "key": h.entry.key,
                        "strong": h.strong,
                        "file_key": h.entry.file_key,
                        "node_id": h.entry.node_id,
                        "component_set": h.entry.component_set,
                        "page": h.entry.page_name,
                        "style_type": h.entry.style_type,
                        "description": h.entry.description,
                        "updated_at": h.entry.updated_at,
                    })
                })
                .collect();
            print(
                &json!({
                    "team_id": catalog.team_id,
                    "query": needle,
                    "catalog": {
                        "components": catalog.components.len(),
                        "component_sets": catalog.component_sets.len(),
                        "styles": catalog.styles.len(),
                        "total": catalog.total(),
                        "fetched_at_epoch": catalog.fetched_at_epoch,
                        "age_seconds": age,
                    },
                    "total_matches": total_matches,
                    "shown": hits.len(),
                    "truncated": truncated,
                    "no_strong_match": outcome.no_strong_match,
                    "self_score": outcome.self_score,
                    "hits": hit_values,
                }),
                format,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind) -> CatalogEntry {
        CatalogEntry {
            key: format!("key-{name}"),
            name: name.into(),
            description: String::new(),
            file_key: "FILE".into(),
            node_id: "1:1".into(),
            kind,
            style_type: None,
            page_name: None,
            component_set: None,
            updated_at: None,
        }
    }

    fn catalog(
        components: Vec<CatalogEntry>,
        component_sets: Vec<CatalogEntry>,
        styles: Vec<CatalogEntry>,
    ) -> TeamCatalog {
        TeamCatalog {
            team_id: "T".into(),
            schema_version: 1,
            fetched_at_epoch: 0,
            components,
            component_sets,
            styles,
        }
    }

    #[test]
    fn exact_name_match_ranks_first() {
        let cat = catalog(
            vec![
                entry("Button Group", EntryKind::Component),
                entry("Button", EntryKind::Component),
                entry("Icon Button Large", EntryKind::Component),
            ],
            vec![],
            vec![],
        );
        let hits = search(&cat, "Button", None);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].entry.name, "Button", "exact match ranks first");
        assert!(hits[0].strong, "an exact match is strong");
    }

    /// Thin wrapper dropping the self-score for tests that only assert on the
    /// hit set (ranking, filtering, counts).
    fn search<'a>(
        cat: &'a TeamCatalog,
        needle: &str,
        kind: Option<EntryKind>,
    ) -> Vec<ScoredEntry<'a>> {
        search_catalog(cat, needle, kind).0
    }

    #[test]
    fn fuzzy_match_tolerates_a_dropped_letter() {
        let cat = catalog(vec![entry("Button", EntryKind::Component)], vec![], vec![]);
        // "buttn" is a subsequence of "button" — the fuzzy matcher finds it.
        assert_eq!(search(&cat, "buttn", None).len(), 1);
    }

    #[test]
    fn multi_word_query_requires_all_words() {
        let cat = catalog(
            vec![
                entry("Primary Button", EntryKind::Component),
                entry("Primary Card", EntryKind::Component),
            ],
            vec![],
            vec![],
        );
        let hits = search(&cat, "primary button", None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.name, "Primary Button");
    }

    #[test]
    fn type_filter_restricts_kind() {
        let cat = catalog(
            vec![entry("Card", EntryKind::Component)],
            vec![entry("Card Variants", EntryKind::ComponentSet)],
            vec![entry("Card Shadow", EntryKind::Style)],
        );
        let only_styles = search(&cat, "card", Some(EntryKind::Style));
        assert_eq!(only_styles.len(), 1);
        assert_eq!(only_styles[0].entry.kind, EntryKind::Style);
        assert_eq!(search(&cat, "card", None).len(), 3);
    }

    #[test]
    fn no_match_returns_empty() {
        let cat = catalog(vec![entry("Button", EntryKind::Component)], vec![], vec![]);
        assert!(search(&cat, "zzzznomatchqx", None).is_empty());
    }

    #[test]
    fn search_is_case_insensitive() {
        let cat = catalog(vec![entry("Button", EntryKind::Component)], vec![], vec![]);
        assert_eq!(search(&cat, "BUTTON", None).len(), 1);
        assert_eq!(search(&cat, "button", None).len(), 1);
    }

    #[test]
    fn ranking_breaks_ties_deterministically() {
        // Identical names ⇒ identical score and length: the tie falls through
        // to `key` ascending.
        let mut a = entry("Same", EntryKind::Component);
        a.key = "key-b".into();
        let mut b = entry("Same", EntryKind::Component);
        b.key = "key-a".into();
        let cat = catalog(vec![a, b], vec![], vec![]);
        let hits = search(&cat, "Same", None);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].entry.key, "key-a", "ties break by key ascending");
    }

    /// Realistic design-system names for the noise-floor tests: a genuine
    /// component, the variant-property junk string, and a near-miss.
    fn noise_catalog() -> TeamCatalog {
        catalog(
            vec![
                entry("Text Input", EntryKind::Component),
                entry(
                    "Text=False, Checkbox=True, Color=Gray",
                    EntryKind::Component,
                ),
                entry("Tag Input", EntryKind::Component),
            ],
            vec![],
            vec![],
        )
    }

    #[test]
    fn textarea_query_hits_no_strong_match_path() {
        // "textarea" only subsequence-matches the variant-junk string (there's
        // no 'a' after "text" in "Text Input"/"Tag Input") — and that match is
        // weak, so the whole search has no strong hit.
        let cat = noise_catalog();
        let hits = search(&cat, "textarea", None);
        assert!(!hits.is_empty(), "the junk string does fuzzy-match");
        assert!(
            hits.iter().all(|h| !h.strong),
            "no hit should clear the strong floor: {:?}",
            hits.iter()
                .map(|h| (&h.entry.name, h.score))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn text_input_query_is_strong() {
        // "text input" needs both atoms; the junk lacks an 'i' for "input" and
        // "Tag Input" lacks an 'e' for "text", so only "Text Input" matches.
        let cat = noise_catalog();
        let hits = search(&cat, "text input", None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.name, "Text Input");
        assert!(hits[0].strong, "an exact word-boundary match is strong");
    }

    #[test]
    fn typo_inpt_is_strong_on_input() {
        // Single dropped letter: "inpt" ⊂ "input", ratio ≈0.886 ≥ 0.85. This
        // test pins the calibration floor — retuning below ~0.89 trips it.
        let cat = catalog(vec![entry("Input", EntryKind::Component)], vec![], vec![]);
        let hits = search(&cat, "inpt", None);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].strong, "a one-letter typo stays strong");
    }

    #[test]
    fn self_score_bounds_all_hits() {
        // The self-score is the achievable ceiling: no hit outscores it.
        let cat = noise_catalog();
        let (hits, self_score) = search_catalog(&cat, "textarea", None);
        assert!(!hits.is_empty());
        assert!(
            hits.iter().all(|h| h.score <= self_score),
            "self_score {self_score} must bound every hit"
        );
    }

    #[test]
    fn resolvable_id_uses_synth_when_interned() {
        let mut e = entry("Button", EntryKind::Component);
        e.file_key = "FK".into();
        e.node_id = "12:34".into();

        let mut synth = SynthState::default();
        synth.intern_file("FK"); // → 1
        assert_eq!(resolvable_id(&e, &synth).as_deref(), Some("file:1:12:34"));

        // A file the synth table doesn't know → no resolvable id.
        assert_eq!(resolvable_id(&e, &SynthState::default()), None);
    }

    #[test]
    fn kind_label_includes_style_type() {
        let mut s = entry("Shadow", EntryKind::Style);
        s.style_type = Some("EFFECT".into());
        assert_eq!(kind_label(&s), "STYLE:EFFECT");
        assert_eq!(kind_label(&entry("X", EntryKind::Component)), "COMPONENT");
        assert_eq!(
            kind_label(&entry("X", EntryKind::ComponentSet)),
            "COMPONENT_SET"
        );
    }

    #[test]
    fn human_age_buckets() {
        assert_eq!(human_age(10), "just now");
        assert_eq!(human_age(120), "2m ago");
        assert_eq!(human_age(7_200), "2h ago");
        assert_eq!(human_age(172_800), "2d ago");
    }
}

//! Team-library catalog: a searchable index of every published component,
//! component set, and style across a Figma team's libraries.
//!
//! Unlike the per-file sidecars in [`crate::full_cache`], the catalog is
//! *team*-scoped: one `teams/{team_id}.catalog.json.gz` file aggregates
//! `GET /v1/teams/{team_id}/{components,component_sets,styles}`. `library
//! search` reads it; `cache prefetch` can warm it.
//!
//! Variables are deliberately absent — the Variables REST API is
//! Enterprise-gated. The catalog covers components, component sets, and
//! text/effect styles, but not color tokens (which live in variables).
//!
//! The sidecar is gzipped JSON, framed like [`crate::full_cache`]: the read
//! is tolerant — missing / corrupt / parse-fail / schema-mismatch all surface
//! as `Ok(None)` so the caller refetches.

use std::fs;
use std::io::{Read, Write};

use anyhow::{Context, Result};
use figma_api::apis::configuration::Configuration;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cache::{atomic_write, CacheDir, CATALOG_SCHEMA_VERSION};

/// Which kind of published library asset an entry is. Doubles as the
/// `library search --type` filter value (clap kebab-cases the variants:
/// `component`, `component-set`, `style`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    Component,
    ComponentSet,
    Style,
}

impl EntryKind {
    /// URL path segment and `meta.*` array key — Figma uses the same word for
    /// both (`GET /v1/teams/{id}/components` ⇒ `meta.components`).
    pub fn slug(self) -> &'static str {
        match self {
            EntryKind::Component => "components",
            EntryKind::ComponentSet => "component_sets",
            EntryKind::Style => "styles",
        }
    }

    /// Uppercase label for the search output's `kind` column.
    pub fn label(self) -> &'static str {
        match self {
            EntryKind::Component => "COMPONENT",
            EntryKind::ComponentSet => "COMPONENT_SET",
            EntryKind::Style => "STYLE",
        }
    }
}

/// One published library asset, projected from a team-endpoint response.
///
/// Drops `thumbnail_url` (an expiring signed S3 URL), `description_rt`,
/// `user`, `created_at`, and `sort_position` — none are needed for search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Stable component/style key — the identifier Code Connect uses.
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Source library file the asset is published from.
    pub file_key: String,
    pub node_id: String,
    pub kind: EntryKind,
    /// For styles: `FILL` / `TEXT` / `EFFECT` / `GRID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_type: Option<String>,
    /// `containing_frame.pageName` — the page the asset lives on. Absent for
    /// styles and top-level components.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_name: Option<String>,
    /// `containing_frame.containingStateGroup.name` — the variant set this
    /// component is one variant of, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_set: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// The cached team-library catalog. Serialized (gzipped) to
/// `teams/{team_id}.catalog.json.gz`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamCatalog {
    pub team_id: String,
    /// On-disk schema version. A mismatch makes [`read_catalog`] treat the
    /// sidecar as missing. `#[serde(default)]` so an absent value
    /// deserializes to 0 (≠ current) rather than failing the parse.
    #[serde(default)]
    pub schema_version: u32,
    /// Epoch seconds when this catalog was fetched. Drives TTL staleness.
    pub fetched_at_epoch: u64,
    pub components: Vec<CatalogEntry>,
    pub component_sets: Vec<CatalogEntry>,
    pub styles: Vec<CatalogEntry>,
}

impl TeamCatalog {
    /// Total entry count across all three kinds.
    pub fn total(&self) -> usize {
        self.components.len() + self.component_sets.len() + self.styles.len()
    }

    /// All entries of one kind.
    pub fn entries(&self, kind: EntryKind) -> &[CatalogEntry] {
        match kind {
            EntryKind::Component => &self.components,
            EntryKind::ComponentSet => &self.component_sets,
            EntryKind::Style => &self.styles,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Pure helpers — no I/O, unit-testable with `json!()` fixtures.
// ─────────────────────────────────────────────────────────────────────────

/// Extract the entry array (`meta.{components|component_sets|styles}`) from a
/// raw team-endpoint response page. Returns an empty slice — never an error —
/// when the key is missing or not an array, so a malformed page degrades to
/// "no entries" rather than aborting the fetch.
pub fn page_entries(page: &Value, kind: EntryKind) -> &[Value] {
    page.get("meta")
        .and_then(|m| m.get(kind.slug()))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Read the pagination cursor from a team-endpoint response page. Figma puts
/// it at `meta.cursor.after` as a JSON number. `None` — meaning "this was the
/// last page" — when `cursor` is absent, `after` is absent, or `after` is null.
pub fn next_cursor(page: &Value) -> Option<u64> {
    let after = page.get("meta")?.get("cursor")?.get("after")?;
    after.as_u64().or_else(|| after.as_f64().map(|f| f as u64))
}

/// Whether a catalog fetched at `fetched_at` is older than `ttl` seconds as of
/// `now`. Clock-skew safe: a `fetched_at` in the future reads as fresh.
pub fn is_stale(fetched_at: u64, now: u64, ttl: u64) -> bool {
    now.saturating_sub(fetched_at) > ttl
}

/// Project a raw team-endpoint entry into a [`CatalogEntry`], keeping only the
/// fields search and output need. Missing fields degrade to empty/`None` —
/// styles and top-level components carry no `containing_frame`.
pub fn project_entry(raw: &Value, kind: EntryKind) -> CatalogEntry {
    let str_field = |k: &str| raw.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let opt_str = |k: &str| {
        raw.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let frame = raw.get("containing_frame");
    let page_name = frame
        .and_then(|f| f.get("pageName"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let component_set = frame
        .and_then(|f| f.get("containingStateGroup"))
        .and_then(|g| g.get("name"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    CatalogEntry {
        key: str_field("key"),
        name: str_field("name"),
        description: str_field("description"),
        file_key: str_field("file_key"),
        node_id: str_field("node_id"),
        kind,
        style_type: opt_str("style_type"),
        page_name,
        component_set,
        updated_at: opt_str("updated_at"),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Sidecar I/O — gzipped JSON, framed like `full_cache`.
// ─────────────────────────────────────────────────────────────────────────

/// Read the team catalog sidecar. `Ok(None)` when the file is absent,
/// unreadable, not valid gzip/JSON, or stamped with a different
/// `schema_version` — every "can't use this" case routes the caller to a
/// refetch instead of erroring out.
pub fn read_catalog(cache: &CacheDir, team_id: &str) -> Result<Option<TeamCatalog>> {
    let p = cache.catalog_path(team_id);
    if !p.exists() {
        return Ok(None);
    }
    let bytes = match fs::read(&p) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("team_catalog: read failed for {}: {e}", p.display());
            return Ok(None);
        }
    };
    let mut decoder = GzDecoder::new(&bytes[..]);
    let mut decompressed = Vec::with_capacity(bytes.len() * 5);
    if let Err(e) = decoder.read_to_end(&mut decompressed) {
        eprintln!("team_catalog: gzip decode failed for {}: {e}", p.display());
        return Ok(None);
    }
    match serde_json::from_slice::<TeamCatalog>(&decompressed) {
        Ok(c) if c.schema_version == CATALOG_SCHEMA_VERSION => Ok(Some(c)),
        Ok(c) => {
            eprintln!(
                "team_catalog: {} is schema v{}, build supports v{} — will refetch",
                p.display(),
                c.schema_version,
                CATALOG_SCHEMA_VERSION,
            );
            Ok(None)
        }
        Err(e) => {
            eprintln!("team_catalog: JSON parse failed for {}: {e}", p.display());
            Ok(None)
        }
    }
}

/// Write the team catalog to `teams/{team_id}.catalog.json.gz` atomically.
/// Entry lists are sorted by `key` and `schema_version` is stamped — both
/// in place — so re-fetches produce byte-stable sidecars. Returns the
/// compressed byte count.
pub fn write_catalog(cache: &CacheDir, catalog: &mut TeamCatalog) -> Result<u64> {
    catalog.schema_version = CATALOG_SCHEMA_VERSION;
    for list in [
        &mut catalog.components,
        &mut catalog.component_sets,
        &mut catalog.styles,
    ] {
        list.sort_by(|a, b| a.key.cmp(&b.key));
    }
    let raw = serde_json::to_vec(catalog).context("serializing team catalog")?;
    let mut encoder = GzEncoder::new(Vec::with_capacity(raw.len() / 4), Compression::default());
    encoder.write_all(&raw).context("gzip encode")?;
    let compressed = encoder.finish().context("gzip finalize")?;
    let n = compressed.len() as u64;
    atomic_write(&cache.catalog_path(&catalog.team_id), &compressed)?;
    Ok(n)
}

// ─────────────────────────────────────────────────────────────────────────
// Live fetch — paginated, sequential, all-or-nothing.
// ─────────────────────────────────────────────────────────────────────────

/// Hard cap on pages fetched per endpoint. Guards against a malformed cursor
/// that never advances. At `page_size=1000` this is a million entries — far
/// past any real team library.
pub const MAX_CATALOG_PAGES: usize = 1000;

/// Page size requested from each team endpoint. Figma's documented maximum;
/// a big team's catalog then takes only a handful of requests.
const CATALOG_PAGE_SIZE: u32 = 1000;

/// Fetch the full team-library catalog live: every published component,
/// component set, and style across the team's libraries.
///
/// All-or-nothing — any endpoint or page failure aborts with an error, so the
/// caller never persists a half-populated catalog stamped with a fresh
/// timestamp. The three endpoints are fetched sequentially; pagination within
/// each is inherently serial (every page needs the prior page's cursor).
pub async fn fetch_team_catalog(cfg: &Configuration, team_id: &str) -> Result<TeamCatalog> {
    let components = fetch_endpoint(cfg, team_id, EntryKind::Component).await?;
    let component_sets = fetch_endpoint(cfg, team_id, EntryKind::ComponentSet).await?;
    let styles = fetch_endpoint(cfg, team_id, EntryKind::Style).await?;
    Ok(TeamCatalog {
        team_id: team_id.to_owned(),
        schema_version: CATALOG_SCHEMA_VERSION,
        fetched_at_epoch: crate::cache::now_epoch(),
        components,
        component_sets,
        styles,
    })
}

/// Paginate one team endpoint to exhaustion, projecting each entry.
async fn fetch_endpoint(
    cfg: &Configuration,
    team_id: &str,
    kind: EntryKind,
) -> Result<Vec<CatalogEntry>> {
    let mut out: Vec<CatalogEntry> = Vec::new();
    let mut after: Option<u64> = None;
    for _ in 0..MAX_CATALOG_PAGES {
        let mut url = format!(
            "{}/v1/teams/{}/{}?page_size={}",
            cfg.base_path,
            team_id,
            kind.slug(),
            CATALOG_PAGE_SIZE,
        );
        if let Some(a) = after {
            url.push_str(&format!("&after={a}"));
        }
        let page = crate::cmd::get_json(cfg, &url)
            .await
            .with_context(|| format!("fetching team {}", kind.slug()))?;
        for raw in page_entries(&page, kind) {
            out.push(project_entry(raw, kind));
        }
        match next_cursor(&page) {
            Some(a) => after = Some(a),
            None => return Ok(out),
        }
    }
    anyhow::bail!(
        "team {} pagination exceeded {MAX_CATALOG_PAGES} pages — aborting",
        kind.slug()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn entry(key: &str, kind: EntryKind) -> CatalogEntry {
        CatalogEntry {
            key: key.into(),
            name: format!("name-{key}"),
            description: String::new(),
            file_key: "F".into(),
            node_id: "1:1".into(),
            kind,
            style_type: None,
            page_name: None,
            component_set: None,
            updated_at: None,
        }
    }

    #[test]
    fn project_entry_component_flattens_frame_and_drops_noise() {
        let raw = json!({
            "key": "K1",
            "file_key": "FILE",
            "node_id": "10:20",
            "thumbnail_url": "https://signed.example/expiring",
            "name": "Button/Primary",
            "description": "the primary button",
            "description_rt": "<rt-noise>",
            "created_at": "2023-01-01T00:00:00Z",
            "updated_at": "2024-02-02T00:00:00Z",
            "user": { "id": "u1" },
            "containing_frame": {
                "pageName": "Buttons",
                "containingStateGroup": { "name": "Button" }
            }
        });
        let e = project_entry(&raw, EntryKind::Component);
        assert_eq!(e.key, "K1");
        assert_eq!(e.name, "Button/Primary");
        assert_eq!(e.description, "the primary button");
        assert_eq!(e.file_key, "FILE");
        assert_eq!(e.node_id, "10:20");
        assert_eq!(e.kind, EntryKind::Component);
        assert_eq!(e.page_name.as_deref(), Some("Buttons"));
        assert_eq!(e.component_set.as_deref(), Some("Button"));
        assert_eq!(e.updated_at.as_deref(), Some("2024-02-02T00:00:00Z"));
        assert!(e.style_type.is_none());
        // The noise fields must not survive into the projected entry.
        let serialized = serde_json::to_string(&e).unwrap();
        assert!(!serialized.contains("thumbnail"));
        assert!(!serialized.contains("rt-noise"));
        assert!(!serialized.contains("created_at"));
    }

    #[test]
    fn project_entry_style_has_no_frame_fields() {
        let raw = json!({
            "key": "S1",
            "file_key": "FILE",
            "node_id": "5:5",
            "style_type": "EFFECT",
            "name": "Elevations/Depth 1",
            "description": ""
        });
        let e = project_entry(&raw, EntryKind::Style);
        assert_eq!(e.kind, EntryKind::Style);
        assert_eq!(e.style_type.as_deref(), Some("EFFECT"));
        assert!(e.page_name.is_none());
        assert!(e.component_set.is_none());
    }

    #[test]
    fn project_entry_tolerates_missing_frame() {
        let raw = json!({ "key": "K", "file_key": "F", "node_id": "1:1", "name": "Top" });
        let e = project_entry(&raw, EntryKind::Component);
        assert_eq!(e.name, "Top");
        assert_eq!(e.description, "");
        assert!(e.page_name.is_none());
        assert!(e.component_set.is_none());
    }

    #[test]
    fn page_entries_pulls_array_or_empty() {
        let page = json!({ "meta": { "components": [ {"key":"a"}, {"key":"b"} ] } });
        assert_eq!(page_entries(&page, EntryKind::Component).len(), 2);
        assert!(page_entries(&page, EntryKind::Style).is_empty());
        assert!(page_entries(&json!({}), EntryKind::Component).is_empty());
        let wrong_type = json!({ "meta": { "components": "not an array" } });
        assert!(page_entries(&wrong_type, EntryKind::Component).is_empty());
    }

    #[test]
    fn next_cursor_terminal_conditions() {
        assert_eq!(
            next_cursor(&json!({ "meta": { "cursor": { "after": 4680598397u64 } } })),
            Some(4680598397)
        );
        assert_eq!(next_cursor(&json!({ "meta": { "cursor": {} } })), None);
        assert_eq!(next_cursor(&json!({ "meta": {} })), None);
        assert_eq!(next_cursor(&json!({})), None);
        assert_eq!(
            next_cursor(&json!({ "meta": { "cursor": { "after": null } } })),
            None
        );
    }

    #[test]
    fn is_stale_respects_ttl_and_clock_skew() {
        assert!(!is_stale(1_000, 1_500, 3_600), "within ttl");
        assert!(is_stale(1_000, 1_000 + 3_601, 3_600), "past ttl");
        assert!(
            !is_stale(2_000, 1_000, 3_600),
            "fetched in the future → fresh"
        );
    }

    fn sample_catalog() -> TeamCatalog {
        TeamCatalog {
            team_id: "T".into(),
            schema_version: 0, // write_catalog stamps the real version
            fetched_at_epoch: 12_345,
            components: vec![
                entry("c2", EntryKind::Component),
                entry("c1", EntryKind::Component),
            ],
            component_sets: vec![entry("s1", EntryKind::ComponentSet)],
            styles: vec![entry("y1", EntryKind::Style)],
        }
    }

    fn tmp_cache() -> (TempDir, CacheDir) {
        let td = TempDir::new().unwrap();
        let cache = CacheDir::new(td.path());
        cache.ensure().unwrap();
        (td, cache)
    }

    #[test]
    fn write_then_read_roundtrip_and_sorts_by_key() {
        let (_g, cache) = tmp_cache();
        let mut cat = sample_catalog();
        let n = write_catalog(&cache, &mut cat).unwrap();
        assert!(n > 0);
        // write_catalog stamps the schema version and sorts in place.
        assert_eq!(cat.schema_version, CATALOG_SCHEMA_VERSION);
        assert_eq!(
            cat.components
                .iter()
                .map(|e| e.key.as_str())
                .collect::<Vec<_>>(),
            ["c1", "c2"]
        );
        let back = read_catalog(&cache, "T").unwrap().expect("present");
        assert_eq!(back, cat);
        assert_eq!(back.total(), 4);
    }

    #[test]
    fn read_catalog_missing_returns_none() {
        let (_g, cache) = tmp_cache();
        assert!(read_catalog(&cache, "absent").unwrap().is_none());
    }

    #[test]
    fn read_catalog_corrupt_gzip_returns_none() {
        let (_g, cache) = tmp_cache();
        fs::write(cache.catalog_path("T"), b"not gzip at all").unwrap();
        assert!(read_catalog(&cache, "T").unwrap().is_none());
    }

    #[test]
    fn read_catalog_valid_gzip_bad_json_returns_none() {
        let (_g, cache) = tmp_cache();
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(b"definitely not json").unwrap();
        fs::write(cache.catalog_path("T"), enc.finish().unwrap()).unwrap();
        assert!(read_catalog(&cache, "T").unwrap().is_none());
    }

    #[test]
    fn read_catalog_schema_mismatch_returns_none() {
        let (_g, cache) = tmp_cache();
        let body = json!({
            "team_id": "T",
            "schema_version": 999,
            "fetched_at_epoch": 1,
            "components": [], "component_sets": [], "styles": []
        });
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&serde_json::to_vec(&body).unwrap()).unwrap();
        fs::write(cache.catalog_path("T"), enc.finish().unwrap()).unwrap();
        assert!(read_catalog(&cache, "T").unwrap().is_none());
    }

    #[test]
    fn read_catalog_missing_schema_version_returns_none() {
        let (_g, cache) = tmp_cache();
        // No `schema_version` field at all → serde default 0 → mismatch.
        let body = json!({
            "team_id": "T",
            "fetched_at_epoch": 1,
            "components": [], "component_sets": [], "styles": []
        });
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&serde_json::to_vec(&body).unwrap()).unwrap();
        fs::write(cache.catalog_path("T"), enc.finish().unwrap()).unwrap();
        assert!(read_catalog(&cache, "T").unwrap().is_none());
    }
}

//! `library search`: the team-catalog plane. The catalog fixture is the
//! hand-authored overlay (`tests/fixtures/overlay/v1/teams/2002`).

use crate::harness::Harness;

const CATALOG: [&str; 3] = [
    "/v1/teams/2002/component_sets?page_size=1000",
    "/v1/teams/2002/components?page_size=1000",
    "/v1/teams/2002/styles?page_size=1000",
];

/// Lazily fetched on first use, then served from the sidecar until
/// `--refresh` (or the 24h TTL).
#[test]
fn fetches_lazily_then_serves_from_cache() {
    let h = Harness::new();
    let first = h.run(&["library", "search", "button"]).ok();
    crate::harness::assert_authenticated(&first);
    assert_eq!(first.paths(), CATALOG);

    let second = h.run(&["library", "search", "button"]).ok();
    assert!(second.requests.is_empty());
    assert_eq!(second.stdout, first.stdout);

    let refreshed = h.run(&["library", "search", "button", "--refresh"]).ok();
    assert_eq!(refreshed.paths(), CATALOG);
}

/// Hits span components, sets, and styles. An entry whose source file is
/// cached gets a paste-ready `file:N:x:y`; others show the raw file key.
#[test]
fn hits_across_kinds() {
    let h = Harness::new();
    h.prefetched();
    let mut out = String::new();
    for q in ["button", "notifications", "body", "avatar"] {
        let run = h.run(&["library", "search", q]).ok();
        assert!(run.requests.is_empty());
        out.push_str(&format!("$ library search {q}\n{}\n", run.stdout));
    }
    assert!(out.contains("file:2:1:7753  | COMPONENT"), "{out}");
    snap!(h, "search", out);
}

#[test]
fn follows_pagination_cursor() {
    let h = Harness::new();
    let page = |items: serde_json::Value, after: Option<u64>| {
        let mut meta = serde_json::json!({ "components": items });
        if let Some(a) = after {
            meta["cursor"] = serde_json::json!({ "after": a, "before": 1 });
        }
        serde_json::json!({ "error": false, "status": 200, "meta": meta }).to_string()
    };
    let entry = |key: &str, name: &str| serde_json::json!({ "key": key, "file_key": "FxLibraryFile000000003", "node_id": "30:1", "name": name, "description": "" });
    h.figma.set_route_exact(
        "/v1/teams/2002/components?page_size=1000",
        200,
        &page(serde_json::json!([entry("fxpage1", "Toggle")]), Some(42)),
    );
    h.figma.set_route_exact(
        "/v1/teams/2002/components?page_size=1000&after=42",
        200,
        &page(serde_json::json!([entry("fxpage2", "Tooltip")]), None),
    );
    let run = h.run(&["library", "search", "tooltip"]).ok();
    assert!(run
        .paths()
        .contains(&"/v1/teams/2002/components?page_size=1000&after=42"));
    assert!(
        run.stdout.contains("# catalog: 2 components"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains(r#""Tooltip""#), "{}", run.stdout);
}

#[test]
fn needs_a_team_id() {
    let mut h = Harness::new();
    h.unset("FIGMA_TEAM_ID");
    let run = h.run(&["library", "search", "button"]);
    assert!(!run.success);
    assert!(run.stderr.contains("FIGMA_TEAM_ID"), "{}", run.stderr);
    assert!(run.requests.is_empty());
}

#[test]
fn cache_only_without_catalog_fails_offline() {
    let h = Harness::new();
    let run = h.run(&["library", "search", "button", "--cache-only"]);
    assert!(!run.success);
    assert!(run.requests.is_empty());
}

//! `cache prefetch` / `status` / `clear`: what is fetched, what lands on
//! disk, and what survives a clear.

use crate::harness::{Harness, NOTIF, NO_ACCESS};

const CATALOG: [&str; 3] = [
    "/v1/teams/2002/component_sets?page_size=1000",
    "/v1/teams/2002/components?page_size=1000",
    "/v1/teams/2002/styles?page_size=1000",
];

#[test]
fn prefetch_fetches_the_folder_and_writes_every_sidecar() {
    let h = Harness::new();
    let run = h.prefetched();

    let mut expected = vec!["/v2/folders/1001/files".to_owned()];
    for key in [NOTIF, NO_ACCESS] {
        for suffix in ["", "/comments", "/variables/local"] {
            expected.push(format!("/v1/files/{key}{suffix}"));
        }
    }
    expected.extend(CATALOG.map(String::from));
    expected.sort();
    assert_eq!(run.paths(), expected);

    for key in [NOTIF, NO_ACCESS] {
        for ext in ["rkyv", "meta.json", "comments.json", "full.json.gz"] {
            assert!(h.entry(key, ext).is_file(), "{key}.{ext} missing");
        }
        // The fixture account has no Variables API access: recorded 403.
        assert!(!h.entry(key, "variables.json").exists());
        assert!(h.meta(key)["variables_error"].is_string());
    }
    assert!(h.cache_dir().join("teams/2002.catalog.json.gz").is_file());
    snap!(h, "prefetch", run.stdout);
}

/// A second prefetch with nothing changed skips the file bodies but still
/// refreshes comments and the team catalog.
#[test]
fn prefetch_again_skips_unchanged_files() {
    let h = Harness::new();
    h.prefetched();
    let run = h.prefetched();
    let mut expected = vec![
        "/v2/folders/1001/files",
        "/v1/files/FxFixtureFile000000001/comments",
        "/v1/files/FxFixtureFile000000002/comments",
    ];
    expected.extend(CATALOG);
    expected.sort();
    assert_eq!(run.paths(), expected);
    assert!(
        run.stdout.contains("skipped_up_to_date: 2"),
        "{}",
        run.stdout
    );
}

#[test]
fn prefetch_no_variables_skips_the_variables_endpoint() {
    let h = Harness::new();
    let run = h.run(&["cache", "prefetch", "--no-variables"]).ok();
    assert!(!run.paths().iter().any(|p| p.contains("/variables/")));
    assert!(run.paths().contains(&"/v1/files/FxFixtureFile000000001"));
}

#[test]
fn fetch_variables_env_off_skips_the_variables_endpoint() {
    let mut h = Harness::new();
    h.set("FIGMA_EXPLORER_FETCH_VARIABLES", "0");
    let run = h.prefetched();
    assert!(!run.paths().iter().any(|p| p.contains("/variables/")));
}

#[test]
fn prefetch_no_catalog_skips_the_team_endpoints() {
    let h = Harness::new();
    let run = h.run(&["cache", "prefetch", "--no-catalog"]).ok();
    assert!(!run.paths().iter().any(|p| p.starts_with("/v1/teams/")));
    assert!(!h.cache_dir().join("teams/2002.catalog.json.gz").exists());
}

/// Tokens minted before the folders rename lack the `folders:read` scope;
/// listing falls back to the deprecated v1 projects endpoint once.
#[test]
fn prefetch_falls_back_to_v1_projects_on_folder_403() {
    let h = Harness::new();
    let listing = h.figma.route_body("/v2/folders/1001/files");
    h.figma.set_route(
        "/v2/folders/1001/files",
        403,
        r#"{"error":true,"status":403,"message":"Invalid scope: [\"folders:read\"]."}"#,
    );
    h.figma.set_route("/v1/projects/1001/files", 200, &listing);
    let run = h.prefetched();
    assert!(run.paths().contains(&"/v2/folders/1001/files"));
    assert!(run.paths().contains(&"/v1/projects/1001/files"));
    assert!(run.stdout.contains("ok: 2"), "{}", run.stdout);
}

#[test]
fn status_reports_files_sidecars_and_catalog_offline() {
    let h = Harness::new();
    h.prefetched();
    let run = h.run(&["cache", "status"]).ok();
    assert!(run.requests.is_empty());
    snap!(h, "status", run.stdout);
}

/// `cache clear` wipes `files/` and `teams/` but keeps `synth.json` (so
/// `file:N` ids stay stable) and `marks.json`.
#[test]
fn clear_keeps_synth_ids_and_marks() {
    let h = Harness::new();
    h.prefetched();
    h.run(&["mark", "add", "notif", "file:2:1:7753"]).ok();

    let run = h.run(&["cache", "clear"]).ok();
    assert!(run.requests.is_empty());
    let left: Vec<_> = std::fs::read_dir(h.cache_dir().join("files"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");
    assert!(!h.cache_dir().join("teams/2002.catalog.json.gz").exists());
    assert!(h.cache_dir().join("synth.json").is_file());
    assert!(h.cache_dir().join("marks.json").is_file());

    let off = h.run(&["ls", "file:2", "--cache-only"]);
    assert!(!off.success);
    assert!(
        off.stderr.contains("not in the local cache"),
        "{}",
        off.stderr
    );

    h.prefetched();
    let ls = h.run(&["ls", "file:2", "--depth", "1"]).ok();
    assert!(
        ls.stdout.contains(r#"FILE  "Notifications""#),
        "{}",
        ls.stdout
    );
    let marks = h.run(&["mark", "list"]).ok();
    assert!(
        marks.stdout.contains("mark:notif  file:2:1:7753"),
        "{}",
        marks.stdout
    );
}

#[test]
fn clear_one_file_key_leaves_the_rest() {
    let h = Harness::new();
    h.prefetched();
    h.run(&["cache", "clear", "--file-key", NO_ACCESS]).ok();
    assert!(!h.entry(NO_ACCESS, "meta.json").exists());
    assert!(!h.entry(NO_ACCESS, "rkyv").exists());
    assert!(h.entry(NOTIF, "rkyv").is_file());
    assert!(h.cache_dir().join("teams/2002.catalog.json.gz").is_file());
    h.run(&["ls", "file:2", "--cache-only"]).ok();
}

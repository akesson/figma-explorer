//! The 5-minute version check (`cache::ensure_fresh`): reads are served from
//! the cache, but a file past its window is probed via `/meta` and
//! refetched if Figma's `version` moved.

use crate::harness::{Harness, NOTIF};

const META: &str = "/v1/files/FxFixtureFile000000001/meta";
const FILE: &str = "/v1/files/FxFixtureFile000000001";

fn prefetched() -> Harness {
    let h = Harness::new();
    h.prefetched();
    h
}

/// Publish a new version of "Notifications" on the fake server: bump
/// `version` in both the file body and `/meta`, and rename node `1:7757`.
fn publish_edit(h: &Harness, new_name: &str) {
    let mut file: serde_json::Value = serde_json::from_str(&h.figma.route_body(FILE)).unwrap();
    assert!(rename(&mut file["document"], "1:7757", new_name));
    file["version"] = "9999".into();
    h.figma.set_route(FILE, 200, &file.to_string());
    let mut meta: serde_json::Value = serde_json::from_str(&h.figma.route_body(META)).unwrap();
    meta["file"]["version"] = "9999".into();
    h.figma.set_route(META, 200, &meta.to_string());
}

fn rename(node: &mut serde_json::Value, id: &str, name: &str) -> bool {
    if node["id"] == id {
        node["name"] = name.into();
        return true;
    }
    node["children"]
        .as_array_mut()
        .is_some_and(|cs| cs.iter_mut().any(|c| rename(c, id, name)))
}

#[test]
fn within_the_window_nothing_is_probed() {
    let h = prefetched();
    publish_edit(&h, "Filter bar");
    let run = h.run(&["ls", "file:2"]).ok();
    assert!(run.requests.is_empty());
    assert!(run.stdout.contains(r#""Filters""#));
}

#[test]
fn unchanged_version_costs_one_meta_probe() {
    let h = prefetched();
    h.patch_meta(NOTIF, &[("version_checked_at_epoch", 0.into())]);
    let run = h.run(&["ls", "file:2"]).ok();
    crate::harness::assert_authenticated(&run);
    assert_eq!(run.paths(), [META]);
    assert!(h.meta(NOTIF)["version_checked_at_epoch"].as_u64().unwrap() > 0);

    // Restamped: the next read is offline again.
    assert!(h.run(&["ls", "file:2"]).ok().requests.is_empty());
}

#[test]
fn changed_version_refetches_before_serving() {
    let h = prefetched();
    h.run(&["mark", "add", "filters", "file:2:1:7757"]).ok();
    publish_edit(&h, "Filter bar");
    h.patch_meta(NOTIF, &[("version_checked_at_epoch", 0.into())]);

    let run = h.run(&["ls", "file:2"]).ok();
    assert!(run.paths().contains(&META));
    assert!(run.paths().contains(&FILE), "{:?}", run.paths());
    assert!(
        run.stdout.contains(r#"FRAME  "Filter bar""#),
        "{}",
        run.stdout
    );
    assert_eq!(h.meta(NOTIF)["version"], "9999");

    // The full sidecar was rewritten from the same response.
    let info = h.run(&["node-info", "file:2:1:7757", "--no-children"]).ok();
    assert!(info.requests.is_empty(), "{:?}", info.paths());
    assert!(
        info.stdout.contains("  name: Filter bar\n"),
        "{}",
        info.stdout
    );

    // Marks notice the rename against their stamp.
    let marks = h.run(&["mark", "list"]).ok();
    assert!(
        marks
            .stdout
            .contains(r#""Filters"  [renamed → "Filter bar"]"#),
        "{}",
        marks.stdout
    );
}

/// Multi-file sweeps (`find` without `--in`) freshen every swept file.
#[test]
fn find_freshens_every_swept_file() {
    let h = prefetched();
    h.age_version_checks();
    publish_edit(&h, "Filter bar");
    let run = h.run(&["find", r#""filter bar""#]).ok();
    let mut want = vec![META, FILE, "/v1/files/FxFixtureFile000000002/meta"];
    want.sort();
    let got: Vec<_> = run
        .paths()
        .into_iter()
        .filter(|p| !p.ends_with("/comments"))
        .collect();
    assert_eq!(got, want);
    assert!(
        run.stdout
            .contains(r#""Filter bar"  (Design > Notifications)"#),
        "{}",
        run.stdout
    );
}

/// A failed probe never makes things worse: the cached copy is served and
/// the next command past the window retries.
#[test]
fn failed_probe_serves_the_cached_copy() {
    let h = prefetched();
    let before = h.run(&["ls", "file:2"]).ok().stdout;
    h.figma
        .set_route(META, 500, r#"{"status":500,"err":"boom"}"#);
    h.patch_meta(NOTIF, &[("version_checked_at_epoch", 0.into())]);

    let run = h.run(&["ls", "file:2"]).ok();
    assert_eq!(run.paths(), [META]);
    assert_eq!(run.stdout, before);
    assert!(!run.stderr.is_empty());
    // Transient (5xx): not restamped, so it retries next time.
    assert_eq!(h.meta(NOTIF)["version_checked_at_epoch"], 0);
    assert_eq!(h.run(&["ls", "file:2"]).ok().paths(), [META]);
}

/// Tokens without the `file_metadata:read` scope get a 403 from `/meta`
/// but can still read the file: the probe falls back to `?depth=1`.
#[test]
fn meta_403_falls_back_to_a_shallow_file_read() {
    let h = prefetched();
    h.figma
        .set_route(META, 403, r#"{"status":403,"err":"Invalid scope"}"#);
    h.patch_meta(NOTIF, &[("version_checked_at_epoch", 0.into())]);
    let run = h.run(&["ls", "file:2"]).ok();
    assert_eq!(
        run.paths(),
        [META, "/v1/files/FxFixtureFile000000001?depth=1"]
    );
    // Same version: restamped, no refetch.
    assert!(h.run(&["ls", "file:2"]).ok().requests.is_empty());
}

/// No access at all (unshared/deleted, both reads 403) won't heal by
/// retrying: serve the cache and back off a full window.
#[test]
fn access_error_on_probe_backs_off() {
    let h = prefetched();
    let before = h.run(&["ls", "file:2"]).ok().stdout;
    h.figma
        .set_route(META, 403, r#"{"status":403,"err":"Forbidden"}"#);
    h.figma
        .set_route(FILE, 403, r#"{"status":403,"err":"Forbidden"}"#);
    h.patch_meta(NOTIF, &[("version_checked_at_epoch", 0.into())]);
    let run = h.run(&["ls", "file:2"]).ok();
    assert_eq!(
        run.paths(),
        [META, "/v1/files/FxFixtureFile000000001?depth=1"]
    );
    assert_eq!(run.stdout, before);
    assert!(h.run(&["ls", "file:2"]).ok().requests.is_empty());
}

/// Version moved but the refetch failed: serve the old copy, say it is out
/// of date, and don't retry for the rest of the window.
#[test]
fn failed_refetch_warns_and_backs_off() {
    let h = prefetched();
    let before = h.run(&["ls", "file:2"]).ok().stdout;
    publish_edit(&h, "Filter bar");
    h.figma
        .set_route(FILE, 500, r#"{"status":500,"err":"timeout"}"#);
    h.patch_meta(NOTIF, &[("version_checked_at_epoch", 0.into())]);

    let run = h.run(&["ls", "file:2"]).ok();
    assert_eq!(run.paths(), [FILE, META]);
    assert_eq!(run.stdout, before);
    assert!(h.meta(NOTIF)["refetch_failed_at_epoch"].as_u64().is_some());

    let again = h.run(&["ls", "file:2"]).ok();
    assert!(again.requests.is_empty(), "{:?}", again.paths());
    assert!(again.stderr.contains("out of date"), "{}", again.stderr);
    assert_eq!(again.stdout, before);
}

/// Comment activity doesn't bump Figma's `version`, so comments run on
/// their own 5-minute clock — consulted by `comments`/`node-info`, never by
/// `ls`/`find` sweeps (one `/comments` call per file would be too costly).
#[test]
fn comments_have_their_own_clock() {
    let h = prefetched();
    let comments = "/v1/files/FxFixtureFile000000001/comments";
    let mut body: serde_json::Value = serde_json::from_str(&h.figma.route_body(comments)).unwrap();
    let mut new = body["comments"][0].clone();
    new["id"] = "1000000010".into();
    new["message"] = "Posted after the prefetch.".into();
    new["created_at"] = "2026-04-09T09:00:00.000Z".into();
    body["comments"].as_array_mut().unwrap().insert(0, new);
    h.figma.set_route(comments, 200, &body.to_string());

    let fresh = h.run(&["comments", "file:2"]).ok();
    assert!(fresh.requests.is_empty());
    assert!(!fresh.stdout.contains("Posted after the prefetch."));

    h.patch_meta(
        NOTIF,
        &[
            ("comments_checked_at_epoch", 0.into()),
            ("comments_fetched_at_epoch", 0.into()),
        ],
    );
    let ls = h.run(&["ls", "file:2", "--comments"]).ok();
    assert!(ls.requests.is_empty(), "{:?}", ls.paths());

    let due = h.run(&["comments", "file:2"]).ok();
    assert_eq!(due.paths(), [comments]);
    assert!(
        due.stdout.contains("Posted after the prefetch."),
        "{}",
        due.stdout
    );
    assert!(h.run(&["comments", "file:2"]).ok().requests.is_empty());
}

/// Folder listings are re-checked every 5 minutes by `ls`/`find` sweeps:
/// a file added on Figma is fetched, a deleted one drops out.
#[test]
fn folder_sync_picks_up_added_and_deleted_files() {
    let h = prefetched();
    let listing = "/v2/folders/1001/files";
    let mut body: serde_json::Value = serde_json::from_str(&h.figma.route_body(listing)).unwrap();
    let files = body["files"].as_array_mut().unwrap();
    files.retain(|f| f["key"] != "FxFixtureFile000000002");
    files.push(serde_json::json!({
        "key": "FxFixtureFile000000003",
        "name": "Added screen",
        "thumbnail_url": "https://example.invalid/placeholder.png",
        "last_modified": "2026-06-01T10:00:00Z"
    }));
    h.figma.set_route(listing, 200, &body.to_string());
    let mut added: serde_json::Value =
        serde_json::from_str(&h.figma.route_body("/v1/files/FxFixtureFile000000002")).unwrap();
    added["name"] = "Added screen".into();
    h.figma
        .set_route("/v1/files/FxFixtureFile000000003", 200, &added.to_string());
    h.figma.set_route(
        "/v1/files/FxFixtureFile000000003/comments",
        200,
        r#"{"comments":[]}"#,
    );

    // Within the window: the listing isn't re-checked.
    let before = h.run(&["ls"]).ok();
    assert!(before.requests.is_empty());
    assert!(before.stdout.contains(r#""404 / No Access""#));

    std::fs::write(
        h.cache_dir().join("folders/1001.json"),
        r#"{"listed_at_epoch":0}"#,
    )
    .unwrap();
    let run = h.run(&["ls"]).ok();
    assert!(run.paths().contains(&listing), "{:?}", run.paths());
    assert!(run.paths().contains(&"/v1/files/FxFixtureFile000000003"));
    assert!(
        !run.stdout.contains(r#""404 / No Access""#),
        "{}",
        run.stdout
    );
    // New file gets the next synth id; existing ids never move.
    assert!(
        run.stdout
            .contains(r#"file:3  -  |   FILE  "Added screen""#),
        "{}",
        run.stdout
    );
    assert!(run
        .stdout
        .contains(r#"file:2  -  |   FILE  "Notifications""#));
    assert!(!h.entry("FxFixtureFile000000002", "meta.json").exists());
}

/// A variables response for the "Notifications" text color (`1:7786`'s
/// fill), in the `/v1/files/{key}/variables/local` shape.
fn variables_body(hex_blue: f64) -> String {
    serde_json::json!({
        "status": 200, "error": false,
        "meta": {
            "variables": {
                "VariableID:242e0a01ca790a62f03df76b8463401471d14f4e/6275:727": {
                    "id": "VariableID:242e0a01ca790a62f03df76b8463401471d14f4e/6275:727",
                    "name": "text/default",
                    "variableCollectionId": "VariableCollectionId:fx/1:0",
                    "resolvedType": "COLOR",
                    "valuesByMode": { "1:0": { "r": 0.0, "g": 0.0, "b": hex_blue, "a": 1.0 } },
                    "scopes": ["TEXT_FILL"]
                }
            },
            "variableCollections": {
                "VariableCollectionId:fx/1:0": {
                    "id": "VariableCollectionId:fx/1:0",
                    "name": "Semantic",
                    "defaultModeId": "1:0",
                    "modes": [{ "modeId": "1:0", "name": "Light" }]
                }
            }
        }
    })
    .to_string()
}

/// Files with a variables sidecar get it re-fetched along with every
/// version-check refetch; files whose variables endpoint 403s are never
/// retried there.
#[test]
fn variables_sidecar_follows_refetches() {
    let h = Harness::new();
    let vars = "/v1/files/FxFixtureFile000000001/variables/local";
    h.figma.set_route(vars, 200, &variables_body(0.0));
    h.prefetched();
    assert!(h.entry(NOTIF, "variables.json").is_file());

    let info = h.run(&["node-info", "file:2:1:7786"]).ok();
    assert!(
        info.stdout.contains("name: text/default"),
        "{}",
        info.stdout
    );
    assert!(
        info.stdout.contains("Light: \"#000000\""),
        "{}",
        info.stdout
    );

    publish_edit(&h, "Filter bar");
    h.figma.set_route(vars, 200, &variables_body(1.0));
    h.age_version_checks();
    // A sweep refetches the changed file (with its variables) and only
    // probes the unchanged one, whose variables 403 anyway.
    let run = h.run(&["find", "text"]).ok();
    assert!(run.paths().contains(&FILE), "{:?}", run.paths());
    assert!(run.paths().contains(&vars), "{:?}", run.paths());
    assert!(!run
        .paths()
        .contains(&"/v1/files/FxFixtureFile000000002/variables/local"));

    let info = h.run(&["node-info", "file:2:1:7786"]).ok();
    assert!(
        info.stdout.contains("Light: \"#0000ff\""),
        "{}",
        info.stdout
    );
}

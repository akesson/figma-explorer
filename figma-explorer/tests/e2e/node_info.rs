//! `node-info`: the curated single-target view, read from the
//! `.full.json.gz` sidecar (no network after a prefetch).

use crate::harness::Harness;

fn prefetched() -> Harness {
    let h = Harness::new();
    h.prefetched();
    h
}

/// An instance: component variants/properties, bound variables hoisted to
/// `vN` handles, a named text style, and an inline region-anchored comment.
#[test]
fn instance_with_variables_styles_and_comment() {
    let h = prefetched();
    let run = h.run(&["node-info", "file:2:1:7761"]).ok();
    assert!(run.requests.is_empty());
    assert!(run.stdout.contains("kind: frame_offset_region"));
    snap!(h, "instance", run.stdout);
}

#[test]
fn text_node() {
    let h = prefetched();
    let run = h.run(&["node-info", "file:2:1:7786"]).ok();
    snap!(h, "text", run.stdout);
}

/// Hidden children are pruned to `hidden_children`; `--include-hidden`
/// renders them inline instead.
#[test]
fn hidden_children_pruned_unless_included() {
    let h = prefetched();
    let run = h.run(&["node-info", "file:2:1:7753", "--depth", "1"]).ok();
    assert!(run
        .stdout
        .contains("hidden_children: [{id: 1:7835, name: Container, type: FRAME}]"));
    snap!(h, "frame_depth1", run.stdout);

    let all = h
        .run(&[
            "node-info",
            "file:2:1:7753",
            "--depth",
            "1",
            "--include-hidden",
            "--no-comments",
        ])
        .ok();
    assert!(!all.stdout.contains("hidden_children"));
    assert!(all.stdout.contains("    - id: 1:7835\n"), "{}", all.stdout);
    assert!(!all.stdout.contains("comments:"));
}

/// Targeting a hidden node directly still works and says so.
#[test]
fn hidden_node_itself() {
    let h = prefetched();
    let run = h.run(&["node-info", "file:2:1:7835"]).ok();
    assert!(run.stdout.contains("  visible: false\n"));
    assert!(run.stdout.contains("Is this hidden menu still needed?"));
}

#[test]
fn image_fill() {
    let h = prefetched();
    let run = h.run(&["node-info", "file:1:1:6635"]).ok();
    assert!(run.stdout.contains(
        "fills: [{type: IMAGE, image_ref: b742831d7e457ec511090ccb5c2944bc2d440ab3, scale_mode: FILL}]"
    ), "{}", run.stdout);
}

#[test]
fn only_restricts_sections() {
    let h = prefetched();
    let run = h
        .run(&["node-info", "file:2:1:7761", "--only", "fills"])
        .ok();
    assert!(run.stdout.contains("fills:"));
    for gone in [
        "bounds:",
        "layout:",
        "component:",
        "comments:",
        "styles_index:",
    ] {
        assert!(!run.stdout.contains(gone), "{gone} kept:\n{}", run.stdout);
    }
}

/// `--raw` is Figma's own node, untouched (camelCase keys); YAML by
/// default like every view, JSON with `--json`.
#[test]
fn raw_is_figmas_node() {
    let h = prefetched();
    let yaml = h.run(&["node-info", "file:2:1:7786", "--raw"]).ok();
    assert!(
        yaml.stdout.contains("absoluteBoundingBox:"),
        "{}",
        yaml.stdout
    );
    let run = h
        .run(&["node-info", "file:2:1:7786", "--raw", "--json"])
        .ok();
    let v: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(v["id"], "1:7786");
    assert_eq!(v["type"], "TEXT");
    assert!(v["characters"].is_string());
    assert!(v["absoluteBoundingBox"].is_object());
}

/// File target: counts, pages, components, and resolved style values.
#[test]
fn file_summary() {
    let h = prefetched();
    let run = h.run(&["node-info", "file:1"]).ok();
    snap!(h, "file_summary", run.stdout);
}

#[test]
fn project_and_root_targets() {
    let h = prefetched();
    let proj = h.run(&["node-info", "proj:1"]).ok();
    assert!(
        proj.stdout
            .contains("project: {id: proj:1, project_id: \"1001\", name: Desktop, file_count: 2}"),
        "{}",
        proj.stdout
    );
    h.run(&["node-info"]).ok();
}

/// A comment id resolves to its thread; a stale anchor (deleted node) is
/// reported as such rather than dropped.
#[test]
fn comment_targets() {
    let h = prefetched();
    let run = h.run(&["node-info", "file:2:comm:4"]).ok();
    snap!(h, "comment", run.stdout);

    let stale = h.run(&["node-info", "file:2:comm:2"]).ok();
    assert!(
        stale.stdout.contains("stale_node_id: 1:99999"),
        "{}",
        stale.stdout
    );
    assert!(stale.stdout.contains("method: {kind: canvas_level}"));
}

/// No `.full.json.gz` sidecar (prefetched with `--no-full`): node-info
/// refetches the file once to get the fields the projection drops.
#[test]
fn missing_full_sidecar_is_refetched() {
    let h = Harness::new();
    h.run(&["cache", "prefetch", "--no-full"]).ok();
    assert!(!h.entry(crate::harness::NOTIF, "full.json.gz").exists());

    let run = h.run(&["node-info", "file:2:1:7786"]).ok();
    assert!(
        run.paths().contains(&"/v1/files/FxFixtureFile000000001"),
        "{:?}",
        run.paths()
    );
    assert!(h.entry(crate::harness::NOTIF, "full.json.gz").is_file());
    assert!(run.stdout.contains("text_auto_resize: HEIGHT"));

    let offline = h.run(&["node-info", "file:2:1:7786", "--cache-only"]).ok();
    assert!(offline.requests.is_empty());
}

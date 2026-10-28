//! `ls` and `find`: structural browsing from the cache. After a prefetch
//! (version checks fresh) none of these touch the network.

use crate::harness::Harness;

fn prefetched() -> Harness {
    let h = Harness::new();
    h.prefetched();
    h
}

#[test]
fn ls_root_project_and_file() {
    let h = prefetched();
    for (args, name) in [
        (&["ls"][..], "ls_root"),
        (&["ls", "proj:1"][..], "ls_project"),
        (&["ls", "file:2"][..], "ls_file"),
    ] {
        let run = h.run(args).ok();
        assert!(run.requests.is_empty(), "{args:?}: {:?}", run.paths());
        snap!(h, name, run.stdout);
    }
}

#[test]
fn ls_full_tree_with_ignored_canvases() {
    let h = prefetched();
    let default = h.run(&["ls", "file:2"]).ok();
    assert!(default.stdout.contains("# hidden canvas: Cover"));
    assert!(!default.stdout.contains(r#"CANVAS  "Cover""#));

    let run = h
        .run(&["ls", "file:2", "--depth", "99", "--no-ignore"])
        .ok();
    assert!(run.stdout.contains(r#"CANVAS  "Cover""#));
    snap!(h, "ls_file_full", run.stdout);
}

/// A bare native id, a figma.com URL, and `--in`-qualified ids all resolve
/// to the same cached node.
#[test]
fn ls_resolves_bare_ids_urls_and_in_scope() {
    let h = prefetched();
    let bare = h.run(&["ls", "1:7761"]).ok();
    assert!(bare.stdout.starts_with("file:2:1:7761 "));
    assert!(bare.stdout.contains(r#"INSTANCE  "Field"  [1 comment]"#));

    let url = h
        .run(&[
            "ls",
            "https://www.figma.com/design/FxFixtureFile000000001/X?node-id=1-7761",
        ])
        .ok();
    assert_eq!(url.stdout, bare.stdout);
    assert!(url.requests.is_empty());

    // `0:1` exists in both files; `--in` picks one.
    let scoped = h.run(&["ls", "0:1", "--in", "file:2"]).ok();
    assert!(
        scoped.stdout.starts_with("file:2:0:1 "),
        "{}",
        scoped.stdout
    );
}

#[test]
fn ls_comments_renders_thread_rows() {
    let h = prefetched();
    let run = h.run(&["ls", "file:2", "--comments"]).ok();
    // Thread anchored on a node, under that node…
    assert!(run.stdout.contains(
        r#"file:2:comm:9  -                 |       COMMENT  "@Fixture User 2 should the settings row wrap on narrow widths?"  by @Fixture User 1  +2"#
    ), "{}", run.stdout);
    // …and a stale-anchor thread at canvas level, under the file.
    assert!(run.stdout.contains(
        r#"file:2:comm:2  -                 |   COMMENT  "Old note on a removed frame.""#
    ));
    let open = h
        .run(&["ls", "file:2", "--comments", "--resolved", "false"])
        .ok();
    assert!(open.stdout.contains("file:2:comm:9"));
    let closed = h
        .run(&["ls", "file:2", "--comments", "--resolved", "true"])
        .ok();
    assert!(!closed.stdout.contains("file:2:comm:9"));
}

#[test]
fn ls_name_filter_keeps_ancestors() {
    let h = prefetched();
    let run = h
        .run(&["ls", "file:2", "--name", "filters", "--depth", "4"])
        .ok();
    snap!(h, "ls_name_filter", run.stdout);
}

/// A URL for a file the cache has never seen is the one cold-fetch path:
/// the file body and its comments, nothing else (no prefetch needed).
#[test]
fn ls_url_for_uncached_file_fetches_just_that_file() {
    let h = Harness::new();
    let run = h
        .run(&[
            "ls",
            "https://www.figma.com/design/FxFixtureFile000000001/X?node-id=1-7761",
        ])
        .ok();
    crate::harness::assert_authenticated(&run);
    assert_eq!(
        run.paths(),
        [
            "/v1/files/FxFixtureFile000000001",
            "/v1/files/FxFixtureFile000000001/comments"
        ]
    );
    assert!(run.stdout.starts_with("file:1:1:7761 "), "{}", run.stdout);

    // Now cached and freshly version-checked: served offline.
    let again = h.run(&["ls", "file:1:1:7761"]).ok();
    assert!(again.requests.is_empty());
    assert_eq!(again.stdout, run.stdout);
}

#[test]
fn cache_only_never_touches_the_network() {
    let h = Harness::new();
    let cold = h.run(&["ls", "file:2", "--cache-only"]);
    assert!(!cold.success);
    assert!(
        cold.stderr.contains("nothing cached for file:2"),
        "{}",
        cold.stderr
    );
    assert!(cold.requests.is_empty());

    h.prefetched();
    h.age_version_checks();
    for args in [
        &["ls", "file:2", "--cache-only"][..],
        &["find", "notification", "--cache-only"][..],
        &["node-info", "file:2:1:7786", "--cache-only"][..],
        &["comments", "file:2", "--cache-only"][..],
        &["library", "search", "button", "--cache-only"][..],
    ] {
        let run = h.run(args).ok();
        assert!(run.requests.is_empty(), "{args:?}: {:?}", run.paths());
    }
}

#[test]
fn find_query_grammar() {
    let h = prefetched();
    let cases: [(&[&str], &str); 4] = [
        (&["find", "notification"], "find_fuzzy"),
        (&["find", r#""Notification Content""#], "find_phrase"),
        (&["find", "avatar", "OR", "checkbox"], "find_or"),
        (&["find", "notification -content"], "find_exclude"),
    ];
    for (args, name) in cases {
        let run = h.run(args).ok();
        assert!(run.requests.is_empty());
        snap!(h, name, run.stdout);
    }
}

/// `-term` excludes hits whose ancestor chain matches: the "Notification
/// Content" frames drop out, leaving fewer sub-hits under each result.
#[test]
fn find_exclusion_narrows_hits() {
    let h = prefetched();
    let all = h.run(&["find", "notification"]).ok();
    let excluded = h.run(&["find", "notification -content"]).ok();
    assert!(all.stdout.contains("[+16 hits]"), "{}", all.stdout);
    assert!(
        !excluded.stdout.contains("[+16 hits]"),
        "{}",
        excluded.stdout
    );
    // Separate argv words work too, after `--`.
    let split = h.run(&["find", "--", "notification", "-content"]).ok();
    assert_eq!(split.stdout, excluded.stdout);
}

#[test]
fn find_scope_type_and_no_match() {
    let h = prefetched();
    let scoped = h.run(&["find", "billing", "--in", "file:1"]).ok();
    assert!(
        scoped.stdout.lines().all(|l| l.starts_with("file:1:")),
        "{}",
        scoped.stdout
    );
    assert_eq!(scoped.stdout.lines().count(), 2);

    let typed = h.run(&["find", "avatar", "--type", "INSTANCE"]).ok();
    let rows: Vec<_> = typed
        .stdout
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect();
    assert_eq!(rows.len(), 2, "{}", typed.stdout);
    assert!(rows.iter().all(|r| r.contains("| INSTANCE ")));

    let none = h.run(&["find", "zzzqqq"]).ok();
    assert_eq!(none.stdout.trim(), "# searched 2 cached files — 0 matches");
}

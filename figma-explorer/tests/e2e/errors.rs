//! User-facing errors: unknown ids, ids a command can't take, and Figma
//! errors on the cold-fetch path. Each fails without a network round-trip
//! unless the error comes from Figma itself.

use crate::harness::Harness;

#[test]
fn unknown_ids_fail_offline() {
    let h = Harness::new();
    h.prefetched();
    for (args, msg) in [
        (&["ls", "file:99"][..], "nothing cached for file:99"),
        (&["ls", "proj:7"][..], "nothing cached for proj:7"),
        (
            &["node-info", "file:2:9:9"][..],
            "nothing cached for file:2:9:9 (node id not found in file)",
        ),
        (&["comments", "file:2:comm:99"][..], "file:2:comm:99"),
        (&["node-info", "mark:nope"][..], "nope"),
    ] {
        let run = h.run(args);
        assert!(!run.success, "{args:?} succeeded");
        assert!(run.stderr.contains(msg), "{args:?}: {}", run.stderr);
        assert!(run.requests.is_empty(), "{args:?}: {:?}", run.paths());
    }
}

#[test]
fn commands_reject_comment_ids_with_a_hint() {
    let h = Harness::new();
    h.prefetched();
    let run = h.run(&["ls", "file:2:comm:1"]);
    assert!(!run.success);
    assert!(
        run.stderr
            .contains("ls does not accept comment ids; use `node-info` for a comment"),
        "{}",
        run.stderr
    );
}

/// Figma's own 404 (deleted/unshared file) surfaces on a cold URL fetch.
#[test]
fn figma_error_on_cold_fetch_surfaces() {
    let h = Harness::new();
    h.figma.set_route(
        "/v1/files/FxGoneFile00000000000009",
        404,
        r#"{"status":404,"err":"Not found"}"#,
    );
    let run = h.run(&[
        "ls",
        "https://www.figma.com/design/FxGoneFile00000000000009/X",
    ]);
    assert!(!run.success);
    assert!(run.stderr.contains("404"), "{}", run.stderr);
    assert_eq!(run.paths(), ["/v1/files/FxGoneFile00000000000009"]);
}

#[test]
fn prefetch_needs_folder_ids() {
    let mut h = Harness::new();
    h.unset("FIGMA_PROJECTS_IDS");
    let run = h.run(&["cache", "prefetch"]);
    assert!(!run.success);
    assert!(run.requests.is_empty());
}

//! `mark`: curated keyword → node marks, resolvable as `mark:<key>` by every
//! node-accepting command and surfaced first by `find`/`library search`.

use crate::harness::Harness;

#[test]
fn lifecycle() {
    let h = Harness::new();
    h.prefetched();

    let add = h
        .run(&[
            "mark",
            "add",
            "notif",
            "file:2:1:7753",
            "--alias",
            "bell",
            "--note",
            "main list",
        ])
        .ok();
    assert!(add.requests.is_empty());
    assert_eq!(add.stdout.trim(), "# marked mark:notif → 1 node(s)");

    let list = h.run(&["mark", "list"]).ok();
    snap!(h, "list", list.stdout);

    // The alias bridges vocabulary `find` alone wouldn't match.
    let find = h.run(&["find", "bell"]).ok();
    assert!(
        find.stdout
            .lines()
            .nth(1)
            .unwrap()
            .starts_with(r#"★ mark:notif  file:2:1:7753  | "Notifications"  — main list"#),
        "{}",
        find.stdout
    );
    let lib = h.run(&["library", "search", "bell"]).ok();
    assert!(lib.stdout.contains("★ mark:notif"), "{}", lib.stdout);

    // A single-node mark resolves like the node itself.
    let via_mark = h.run(&["ls", "mark:notif", "--depth", "1"]).ok();
    let direct = h.run(&["ls", "file:2:1:7753", "--depth", "1"]).ok();
    assert_eq!(via_mark.stdout, direct.stdout);
    let info = h.run(&["node-info", "mark:notif", "--no-children"]).ok();
    assert!(
        info.stdout.contains("  id: file:2:1:7753\n"),
        "{}",
        info.stdout
    );

    let dup = h.run(&["mark", "add", "notif", "file:2:1:7761"]);
    assert!(!dup.success);
    h.run(&["mark", "add", "notif", "file:2:1:7761", "--force"])
        .ok();
    let moved = h.run(&["ls", "mark:notif", "--depth", "0"]).ok();
    assert!(
        moved.stdout.starts_with("file:2:1:7761 "),
        "{}",
        moved.stdout
    );

    h.run(&["mark", "rm", "notif"]).ok();
    let gone = h.run(&["ls", "mark:notif"]);
    assert!(!gone.success);
}

/// A multi-node mark can't stand in for one node; it lists the ids instead.
#[test]
fn multi_node_mark_lists_its_targets() {
    let h = Harness::new();
    h.prefetched();
    h.run(&["mark", "add", "avatars", "file:2:6:3476", "file:2:1:7827"])
        .ok();
    let run = h.run(&["node-info", "mark:avatars"]);
    assert!(!run.success);
    assert!(run.stderr.contains("file:2:6:3476"), "{}", run.stderr);
    assert!(run.stderr.contains("file:2:1:7827"), "{}", run.stderr);
}

#[test]
fn rejects_non_node_targets_and_bad_keys() {
    let h = Harness::new();
    h.prefetched();
    for args in [
        &["mark", "add", "f", "file:2"][..],
        &["mark", "add", "c", "file:2:comm:4"][..],
        &["mark", "add", "bad:key", "file:2:1:7753"][..],
    ] {
        let run = h.run(args);
        assert!(!run.success, "{args:?} accepted");
    }
    let list = h.run(&["mark", "list"]).ok();
    assert!(list.stdout.starts_with("# 0 marks"), "{}", list.stdout);
}

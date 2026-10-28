//! `comments`: listing and filtering threads from the `.comments.json`
//! sidecar, anchored to nodes. Fixture threads are the hand-authored
//! overlay (`tests/fixtures/overlay`) on "Notifications" (`file:2`).

use crate::harness::Harness;

fn prefetched() -> Harness {
    let h = Harness::new();
    h.prefetched();
    h
}

/// The `comm_id`s of the thread heads, in output order.
fn heads(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .filter_map(|l| l.strip_prefix("    comm_id: "))
        .collect()
}

#[test]
fn lists_every_thread_newest_first() {
    let h = prefetched();
    let run = h.run(&["comments", "file:2"]).ok();
    assert!(run.requests.is_empty());
    assert_eq!(
        heads(&run.stdout),
        [
            "file:2:comm:1",
            "file:2:comm:2",
            "file:2:comm:3",
            "file:2:comm:4",
            "file:2:comm:6",
            "file:2:comm:9"
        ]
    );
    snap!(h, "all", run.stdout);
}

#[test]
fn filters() {
    let h = prefetched();
    let cases: [(&[&str], &[&str]); 6] = [
        (
            &["--unresolved"],
            &[
                "file:2:comm:2",
                "file:2:comm:3",
                "file:2:comm:4",
                "file:2:comm:9",
            ],
        ),
        (&["--grep", "PADDING"], &["file:2:comm:4"]),
        // Matches on reply text too ("Done." is a reply in comm:6's thread).
        (&["--grep", "done"], &["file:2:comm:6"]),
        (
            &["--since", "2026-04-05"],
            &[
                "file:2:comm:1",
                "file:2:comm:2",
                "file:2:comm:3",
                "file:2:comm:4",
            ],
        ),
        (&["--limit", "2"], &["file:2:comm:1", "file:2:comm:2"]),
        (&["--unresolved", "--grep", "wrap"], &["file:2:comm:9"]),
    ];
    for (flags, want) in cases {
        let mut args = vec!["comments", "file:2"];
        args.extend(flags);
        let run = h.run(&args).ok();
        assert_eq!(heads(&run.stdout), want, "{flags:?}\n{}", run.stdout);
    }
}

/// A node target keeps threads anchored anywhere in its subtree.
#[test]
fn node_subtree() {
    let h = prefetched();
    let run = h.run(&["comments", "file:2:1:7762"]).ok();
    assert_eq!(heads(&run.stdout), ["file:2:comm:6"]);
    let run = h.run(&["comments", "file:2:1:7753"]).ok();
    assert_eq!(
        heads(&run.stdout),
        [
            "file:2:comm:3",
            "file:2:comm:4",
            "file:2:comm:6",
            "file:2:comm:9"
        ]
    );
    let none = h.run(&["comments", "file:1"]).ok();
    assert!(heads(&none.stdout).is_empty());
}

/// One thread by id, with its replies — a reply id resolves to the same
/// thread view of that reply.
#[test]
fn single_thread() {
    let h = prefetched();
    let run = h.run(&["comments", "file:2:comm:9"]).ok();
    assert!(run.stdout.contains("message: Yes, wrap below 480px."));
    snap!(h, "thread", run.stdout);
}

/// `--refresh` re-fetches this one file's comments and nothing else.
#[test]
fn refresh_fetches_only_this_files_comments() {
    let h = prefetched();
    let run = h.run(&["comments", "file:2", "--refresh"]).ok();
    assert_eq!(run.paths(), ["/v1/files/FxFixtureFile000000001/comments"]);

    // A new thread on Figma shows up after the refresh.
    let mut body: serde_json::Value = serde_json::from_str(
        &h.figma
            .route_body("/v1/files/FxFixtureFile000000001/comments"),
    )
    .unwrap();
    let mut new = body["comments"][0].clone();
    new["id"] = "1000000010".into();
    new["message"] = "Brand new thread.".into();
    new["created_at"] = "2026-04-09T09:00:00.000Z".into();
    new["resolved_at"] = serde_json::Value::Null;
    body["comments"].as_array_mut().unwrap().insert(0, new);
    h.figma.set_route(
        "/v1/files/FxFixtureFile000000001/comments",
        200,
        &body.to_string(),
    );
    let stale = h.run(&["comments", "file:2"]).ok();
    assert!(!stale.stdout.contains("Brand new thread."));
    let fresh = h.run(&["comments", "file:2", "--refresh"]).ok();
    assert!(
        fresh.stdout.contains("Brand new thread."),
        "{}",
        fresh.stdout
    );
    assert!(
        fresh.stdout.starts_with("# 7 threads (5 unresolved)"),
        "{}",
        fresh.stdout
    );

    let refused = h.run(&["comments", "file:2", "--refresh", "--cache-only"]);
    assert!(!refused.success);
    assert!(refused.requests.is_empty());
}

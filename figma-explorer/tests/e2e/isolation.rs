//! The harness itself: the subprocess sees only what the harness sets, and
//! unrecorded endpoints fail loudly instead of silently reaching Figma.

use crate::harness::Harness;

/// A fixtures dir with no `routes.json` and no sibling `overlay/`.
fn no_fixtures() -> (tempfile::TempDir, std::path::PathBuf) {
    let td = tempfile::TempDir::new().unwrap();
    let api = td.path().join("api");
    (td, api)
}

/// The developer's real token (shell env, repo `.env`, or
/// `~/.config/figma-explorer/.env`) must not reach the subprocess.
#[test]
fn token_does_not_leak_from_developer_env() {
    let (_td, api) = no_fixtures();
    let mut h = Harness::with_fixtures(&api);
    h.unset("FIGMA_TOKEN");
    let run = h.run(&["cache", "status"]);
    assert!(!run.success);
    assert!(run.stderr.contains("FIGMA_TOKEN not set"), "{}", run.stderr);
    assert!(run.requests.is_empty());
}

/// A URL for a file with no fixture hits the fake server (never
/// api.figma.com) and surfaces its 404.
#[test]
fn unrecorded_endpoint_is_a_loud_404() {
    let (_td, api) = no_fixtures();
    let h = Harness::with_fixtures(&api);
    let run = h.run(&[
        "ls",
        "https://www.figma.com/design/AAAAAAAAAAAAAAAAAAAAAA/X",
    ]);
    assert!(!run.success);
    assert!(
        run.stderr
            .contains("no fixture for /v1/files/AAAAAAAAAAAAAAAAAAAAAA"),
        "{}",
        run.stderr
    );
    assert_eq!(run.paths(), ["/v1/files/AAAAAAAAAAAAAAAAAAAAAA"]);
    crate::harness::assert_authenticated(&run);
}

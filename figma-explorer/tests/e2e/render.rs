//! `screenshot` and `assets`: resolve from the cache, then render through
//! `/v1/images` and download from the returned URLs (which, unlike API
//! calls, must not carry the token).

use crate::fake_figma::{PNG, SVG};
use crate::harness::{assert_authenticated, Harness};

fn prefetched() -> Harness {
    let h = Harness::new();
    h.prefetched();
    h
}

#[test]
fn screenshot_prints_the_render_url_without_downloading() {
    let h = prefetched();
    let run = h.run(&["screenshot", "file:2:1:7761"]).ok();
    assert_eq!(
        run.paths(),
        ["/v1/images/FxFixtureFile000000001?ids=1%3A7761&scale=2&format=png"]
    );
    assert!(h
        .redact(&run.stdout)
        .contains("url: [FIGMA]/img/1-7761.png"));
}

#[test]
fn screenshot_out_downloads_the_bytes() {
    let h = prefetched();
    let run = h
        .run(&[
            "screenshot",
            "file:2:1:7761",
            "--out",
            "shot.svg",
            "--img-format",
            "svg",
            "--scale",
            "1",
        ])
        .ok();
    assert_authenticated(&run);
    assert_eq!(
        run.paths(),
        [
            "/img/1-7761.svg",
            "/v1/images/FxFixtureFile000000001?ids=1%3A7761&scale=1&format=svg"
        ]
    );
    assert_eq!(
        std::fs::read(h.cwd().join("shot.svg")).unwrap(),
        SVG.as_bytes()
    );
}

/// Rendering is inherently live, so `--cache-only` refuses before resolving.
#[test]
fn screenshot_and_assets_refuse_cache_only() {
    let h = prefetched();
    for cmd in ["screenshot", "assets"] {
        let run = h.run(&[cmd, "file:2:1:7761", "--cache-only"]);
        assert!(!run.success);
        assert!(run.stderr.contains("drop --cache-only"), "{}", run.stderr);
        assert!(run.requests.is_empty());
    }
}

/// Icons export as SVG at 1x, images as PNG at 2x, with a manifest.
#[test]
fn assets_exports_icons_images_and_manifest() {
    let h = prefetched();
    let run = h.run(&["assets", "file:1:1:6629", "--out-dir", "out"]).ok();
    assert_authenticated(&run);
    assert_eq!(
        run.paths(),
        [
            "/img/1-6635.png",
            "/img/I1-6630-251-29734-101-30150.svg",
            "/v1/files/FxFixtureFile000000002",
            "/v1/images/FxFixtureFile000000002?ids=1%3A6635&scale=2&format=png",
            "/v1/images/FxFixtureFile000000002?ids=I1%3A6630%3B251%3A29734%3B101%3A30150&scale=1&format=svg",
        ]
    );
    let out = h.cwd().join("out");
    assert_eq!(
        std::fs::read(out.join("icons/vector.svg")).unwrap(),
        SVG.as_bytes()
    );
    assert_eq!(std::fs::read(out.join("images/image-2.png")).unwrap(), PNG);
    snap!(h, "assets", run.stdout);
}

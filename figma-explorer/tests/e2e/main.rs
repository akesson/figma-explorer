//! End-to-end tests: the real binary against a local fake Figma API serving
//! recorded, scrubbed responses (`tests/fixtures/api`, written by
//! `scripts/record_fixtures.py`). See `harness.rs` for the sealed env.

/// Snapshot `$text` (stdout of a [`harness::Run`]) under `$name`, after
/// masking what differs between runs: the tempdir and server URL
/// ([`harness::Harness::redact`]), relative ages (`just now`, `0s`),
/// prefetch timing, and the team catalog's compressed size.
macro_rules! snap {
    ($h:expr, $name:expr, $text:expr) => {{
        let text = $h.redact(&$text);
        insta::with_settings!({
            filters => vec![
                (r"elapsed_seconds: [0-9.]+", "elapsed_seconds: [ELAPSED]"),
                // Gzipped catalog embeds its fetch time, so its size wobbles.
                (r"(team_catalog: \{.*bytes: )\d+", "${1}[BYTES]"),
                (r"\bjust now\b|\b\d+[smhd]( ago)?\b", "[AGE]"),
            ],
        }, {
            insta::assert_snapshot!($name, text);
        });
    }};
}

mod fake_figma;
mod harness;

mod browse;
mod cache;
mod comments;
mod errors;
mod freshness;
mod isolation;
mod library;
mod marks;
mod node_info;
mod render;

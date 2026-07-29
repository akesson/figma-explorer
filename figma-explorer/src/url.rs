//! Parser for `figma.com` URLs.
//!
//! Accepts forms like:
//!   https://www.figma.com/design/<fileKey>/<slug>?node-id=<a-b>
//!   https://www.figma.com/file/<fileKey>/<slug>?node-id=<a-b>
//!   https://www.figma.com/proto/<fileKey>/<slug>?node-id=<a-b>
//!   https://www.figma.com/board/<fileKey>/<slug>?node-id=<a-b>
//!
//! Node IDs in URLs use `-` where the REST API expects `:` (e.g. `5-12` →
//! `5:12`). Instance-descendant ids keep their leading `I` and separate
//! segments with `;`, percent-encoded as `%3B` (e.g. `I880-3606%3B2816-36646`
//! → `I880:3606;2816:36646`). We translate. Anything we don't recognize
//! returns an error rather than guessing.

use anyhow::{anyhow, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedUrl {
    pub file_key: String,
    pub node_id: Option<String>,
}

pub fn parse(url: &str) -> Result<ParsedUrl> {
    if !url.contains("figma.com") {
        return Err(anyhow!("URL is not a figma.com URL: {url}"));
    }
    let segment = ["/design/", "/file/", "/proto/", "/board/", "/slides/"]
        .iter()
        .find_map(|s| url.find(s).map(|i| (s, i)));

    let (marker, idx) = segment.ok_or_else(|| {
        anyhow!(
            "URL does not look like a Figma file URL: {}\n\
             expected one of /design/, /file/, /proto/, /board/, /slides/",
            url
        )
    })?;

    let after = &url[idx + marker.len()..];
    let file_key = after
        .split(['/', '?', '#'])
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("could not extract file key from URL: {}", url))?
        .to_owned();

    let node_id = url.split_once('?').map(|(_, q)| q).and_then(|q| {
        q.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == "node-id" || k == "node_id").then(|| decode_node_id(v))
        })
    });

    Ok(ParsedUrl { file_key, node_id })
}

/// Inverse of [`decode_node_id`]: render a native node id the way figma.com
/// URLs spell it (`5:12` → `5-12`, instance separators `;` → `%3B`).
pub fn encode_node_id(id: &str) -> String {
    id.replace(':', "-").replace(';', "%3B")
}

/// Canonical figma.com URL for a file or a node within it. Always uses the
/// `/design/` form — Figma redirects it to the right editor.
pub fn figma_url(file_key: &str, node_id: Option<&str>) -> String {
    match node_id {
        Some(id) => format!(
            "https://www.figma.com/design/{file_key}/?node-id={}",
            encode_node_id(id)
        ),
        None => format!("https://www.figma.com/design/{file_key}/"),
    }
}

fn decode_node_id(raw: &str) -> String {
    // The URL encodes the colon as `-` (e.g. 5-12 → 5:12). Some links also
    // percent-encode the colon as %3A — handle both. Instance ids separate
    // segments with `;`, usually percent-encoded as %3B; decoded ids contain
    // no dashes, so the `-`→`:` swap stays safe.
    raw.replace("%3A", ":")
        .replace("%3a", ":")
        .replace("%3B", ";")
        .replace("%3b", ";")
        .replace('-', ":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_design_url_with_node() {
        let p = parse("https://www.figma.com/design/AbCdEf123/Foo?node-id=5-12&t=xyz").unwrap();
        assert_eq!(p.file_key, "AbCdEf123");
        assert_eq!(p.node_id.as_deref(), Some("5:12"));
    }

    #[test]
    fn parses_legacy_file_url_without_node() {
        let p = parse("https://www.figma.com/file/AbCdEf123/Foo").unwrap();
        assert_eq!(p.file_key, "AbCdEf123");
        assert_eq!(p.node_id, None);
    }

    #[test]
    fn parses_percent_encoded_colon() {
        let p = parse("https://www.figma.com/design/K/Foo?node-id=5%3A12").unwrap();
        assert_eq!(p.node_id.as_deref(), Some("5:12"));
    }

    #[test]
    fn parses_instance_node_id() {
        let p = parse("https://www.figma.com/design/K/Foo?node-id=I880-3606%3B2816-36646").unwrap();
        assert_eq!(p.node_id.as_deref(), Some("I880:3606;2816:36646"));
    }

    #[test]
    fn parses_instance_node_id_raw_semicolon() {
        let p = parse("https://www.figma.com/design/K/Foo?node-id=I880-3606;2816-36646").unwrap();
        assert_eq!(p.node_id.as_deref(), Some("I880:3606;2816:36646"));
    }

    #[test]
    fn rejects_non_figma_url() {
        assert!(parse("https://example.com/design/abc").is_err());
    }

    #[test]
    fn figma_url_round_trips_through_parse() {
        for node in ["5:12", "I880:3606;2816:36646"] {
            let url = figma_url("AbCdEf123", Some(node));
            let p = parse(&url).unwrap();
            assert_eq!(p.file_key, "AbCdEf123");
            assert_eq!(p.node_id.as_deref(), Some(node));
        }
        let p = parse(&figma_url("AbCdEf123", None)).unwrap();
        assert_eq!(p.node_id, None);
    }
}

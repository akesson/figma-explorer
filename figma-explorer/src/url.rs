//! Parser for `figma.com` URLs.
//!
//! Accepts forms like:
//!   https://www.figma.com/design/<fileKey>/<slug>?node-id=<a-b>
//!   https://www.figma.com/file/<fileKey>/<slug>?node-id=<a-b>
//!   https://www.figma.com/proto/<fileKey>/<slug>?node-id=<a-b>
//!   https://www.figma.com/board/<fileKey>/<slug>?node-id=<a-b>
//!
//! Node IDs in URLs use `-` between the two integer halves; the REST API
//! expects `:`. We translate. Anything we don't recognize returns an error
//! rather than guessing.

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

fn decode_node_id(raw: &str) -> String {
    // The URL encodes the colon as `-` (e.g. 5-12 → 5:12). Some links also
    // percent-encode the colon as %3A — handle both.
    raw.replace("%3A", ":")
        .replace("%3a", ":")
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
    fn rejects_non_figma_url() {
        assert!(parse("https://example.com/design/abc").is_err());
    }
}

use anyhow::Result;
use clap::Subcommand;
use figma_api::apis::configuration::Configuration;

use crate::Globals;

pub mod assets;
pub mod cache;
pub mod comments;
pub mod find;
pub mod library;
pub mod ls;
pub mod mark;
pub mod node_info;
pub mod screenshot;

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List a node and its descendants (or projects/files at the root).
    Ls(ls::Args),
    /// Locate nodes by a multi-token ancestor-chain query across all cached
    /// files (scope with `--in file:N`).
    Find(find::Args),
    /// Search the published team-library catalog (components, component sets,
    /// styles) by name. Unlike `find`, this spans the whole team library
    /// rather than one cached file.
    Library(library::Args),
    /// Export a node as PNG/JPG/SVG/PDF.
    Screenshot(screenshot::Args),
    /// Export every icon/image/composite below a node into a directory.
    Assets(assets::Args),
    /// Comprehensive single-target view: node properties, layout, fills,
    /// effects, component metadata, bound variables, comments. Designed for
    /// Claude Code agents implementing designs in application code. Accepts
    /// node, comment, file, project, and root targets.
    NodeInfo(node_info::Args),
    /// List comment threads in a file or under a node, or dump one thread
    /// (`file:N:comm:M`). Reads the cached comments sidecar; `--refresh`
    /// re-fetches one file's comments without a full `cache prefetch`.
    Comments(comments::Args),
    /// Curated keyword→node marks: `mark add`/`rm`/`list`. Write down a node
    /// once you've identified it, then resolve it forever after as `mark:<key>`
    /// (and `find`/`library search` surface matching marks first).
    Mark(mark::Args),
    /// Maintain the local file cache (prefetch / clear).
    Cache(cache::Args),
}

impl Command {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        match self {
            Self::Ls(a) => a.run(cfg, globals).await,
            Self::Find(a) => a.run(cfg, globals).await,
            Self::Library(a) => a.run(cfg, globals).await,
            Self::Screenshot(a) => a.run(cfg, globals).await,
            Self::Assets(a) => a.run(cfg, globals).await,
            Self::NodeInfo(a) => a.run(cfg, globals).await,
            Self::Comments(a) => a.run(cfg, globals).await,
            Self::Mark(a) => a.run(cfg, globals).await,
            Self::Cache(a) => a.run(cfg, globals).await,
        }
    }
}

/// Shared loader: fetch a file's document JSON (at a controlled depth) and
/// return it as `serde_json::Value` so all our analysis modules can walk
/// untyped nodes.
///
/// Hits Figma's REST endpoint directly rather than going through figma-api's
/// typed deserializer. The generated client expects every node to match
/// the OpenAPI spec exactly, but real files routinely contain nodes the
/// spec doesn't model (or whose schema has drifted). Reading as
/// `serde_json::Value` avoids those landmines for the entire crate, which
/// is fine because we walk untyped Values anyway.
pub async fn fetch_file_json(
    cfg: &Configuration,
    file_key: &str,
    depth: Option<f64>,
) -> Result<serde_json::Value> {
    let mut url = format!("{}/v1/files/{}", cfg.base_path, file_key);
    if let Some(d) = depth {
        url.push_str(&format!("?depth={}", d));
    }
    get_json(cfg, &url).await
}

/// Backoff before each retry in [`fetch_file_json_retrying`], unless the
/// server sent `Retry-After`. `FIGMA_EXPLORER_RETRY_DELAY_MS` overrides the
/// unit (the e2e suite sets it to 0; not a user setting).
fn retry_delays() -> [std::time::Duration; 2] {
    let unit_ms = std::env::var("FIGMA_EXPLORER_RETRY_DELAY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(2000);
    [1, 4].map(|n| std::time::Duration::from_millis(unit_ms * n))
}

/// Longest `Retry-After` honored; past it the retry is given up on.
const MAX_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

/// [`fetch_file_json`] (whole file, no depth limit) with up to two retries
/// on a transient failure — timeout, connection error, 429, 5xx — for
/// `cache prefetch`, where one hiccup otherwise costs a whole rerun. Each
/// retry is logged with how long the failed attempt took, which is what
/// tells a slow server (attempt ran into the read timeout) from a dead one.
/// Interactive paths keep the single attempt: their failures fall back to
/// the cached copy and back off for a window instead.
pub async fn fetch_file_json_retrying(
    cfg: &Configuration,
    file_key: &str,
    name: &str,
) -> Result<serde_json::Value> {
    let delays = retry_delays();
    let mut attempt = 0;
    loop {
        let started = std::time::Instant::now();
        let err = match fetch_file_json(cfg, file_key, None).await {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };
        if attempt == delays.len() || !figma_common::is_transient(&err) {
            return Err(err);
        }
        let wait = figma_common::retry_after(&err).unwrap_or(delays[attempt]);
        if wait > MAX_RETRY_AFTER {
            return Err(err);
        }
        attempt += 1;
        eprintln!(
            "  retry {file_key:<25}  {name:<35}  failed after {:.1}s ({err:#}); retry {attempt}/{} in {}s",
            started.elapsed().as_secs_f64(),
            delays.len(),
            wait.as_secs()
        );
        tokio::time::sleep(wait).await;
    }
}

/// Borrow the `document` node from a fetched file response, erroring clearly if
/// it is absent or null. `serde_json`'s `Index` (`file["document"]`) returns
/// `Value::Null` on a missing key, so callers that index directly silently walk
/// an empty tree — reporting "no nodes" (commands) or caching an empty file
/// (prefetch) — instead of surfacing the real failure (auth error, partial
/// response, or a renamed field).
pub fn require_document<'a>(
    file: &'a serde_json::Value,
    file_key: &str,
) -> Result<&'a serde_json::Value> {
    file.get("document")
        .filter(|d| !d.is_null())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Figma response for file {file_key} is missing `document` \
                 (auth failure or unexpected response shape)"
            )
        })
}

/// Fetch the local-variables document for a file. Wraps the
/// `/v1/files/{key}/variables/local` endpoint and returns the raw JSON.
///
/// Non-Enterprise accounts get HTTP 403 ("This endpoint is only available to
/// users on plans with Variables REST API access"). Callers (currently
/// `cache prefetch`) treat that as a soft failure: record the error in
/// `FileMeta::variables_error`, optionally disable further variables fetches
/// for the run, but never abort the rest of the work.
pub async fn fetch_local_variables(
    cfg: &Configuration,
    file_key: &str,
) -> Result<serde_json::Value> {
    let url = format!("{}/v1/files/{}/variables/local", cfg.base_path, file_key);
    get_json(cfg, &url).await
}

/// Fetch a file's current `version` via `/v1/files/{key}/meta` — about 1 KB
/// against a full file body that can run to 100+ MB, so it is the cheap
/// change probe behind `cache::ensure_fresh`. Raw JSON for the same reason as
/// `fetch_file_json`: the typed `get_file_meta` model has required fields
/// (`creator`, `editorType`, …) we don't need and can't vouch for.
///
/// `/meta` needs the `file_metadata:read` scope, separate from the
/// `file_content:read` that `/v1/files` needs, so a token with only the
/// latter gets a 403 here. On a 403 fall back to `?depth=1` (~8 KB, also
/// carries `version`); only if that 403s too is the file really unreadable.
pub async fn fetch_file_version(cfg: &Configuration, file_key: &str) -> Result<String> {
    let url = format!("{}/v1/files/{}/meta", cfg.base_path, file_key);
    let (v, source) = match get_json(cfg, &url).await {
        Ok(v) => (v["file"].clone(), "/meta"),
        Err(e) if format!("{e:#}").contains("(403 ") => {
            (fetch_file_json(cfg, file_key, Some(1.0)).await?, "?depth=1")
        }
        Err(e) => return Err(e),
    };
    file_version(&v).ok_or_else(|| {
        anyhow::anyhow!("Figma {source} response for file {file_key} has no `version`")
    })
}

/// Read the `version` field from a `/v1/files/{key}` body (or the `file`
/// object of a `/meta` body). Figma documents it as a string; tolerate a bare
/// number so a wire-format quirk can't turn every probe into a refetch.
pub fn file_version(v: &serde_json::Value) -> Option<String> {
    match v.get("version")? {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Heuristic: does this error string look like a Variables-API 403?
/// Used by `cache prefetch` to decide whether to disable further variables
/// fetches for the rest of the run after a few consecutive 403s.
pub fn is_variables_forbidden_error(err: &str) -> bool {
    err.contains("403")
}

/// Issue a GET against the Figma REST API with the configuration's auth and
/// decode the body as JSON. Bypasses figma-api's typed deserialization;
/// see `fetch_file_json` for the rationale. The auth + transport lives in
/// `figma_common::get_text`; this layer only adds strict JSON parsing.
pub async fn get_json(cfg: &Configuration, url: &str) -> Result<serde_json::Value> {
    use anyhow::Context;
    let body = figma_common::get_text(cfg, url).await?;
    serde_json::from_str(&body).with_context(|| format!("parsing response from {url}"))
}

#[cfg(test)]
mod tests {
    use super::require_document;
    use serde_json::json;

    #[test]
    fn require_document_present() {
        let file = json!({ "document": { "id": "0:0", "children": [] } });
        let doc = require_document(&file, "abc").unwrap();
        assert_eq!(doc["id"], json!("0:0"));
    }

    #[test]
    fn require_document_missing_is_error() {
        let file = json!({ "name": "Untitled" });
        assert!(require_document(&file, "abc").is_err());
    }

    #[test]
    fn require_document_null_is_error() {
        let file = json!({ "document": null });
        assert!(require_document(&file, "abc").is_err());
    }
}

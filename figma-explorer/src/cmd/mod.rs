use anyhow::Result;
use clap::Subcommand;
use figma_api::apis::configuration::Configuration;

use crate::Globals;

pub mod assets;
pub mod cache;
pub mod comments;
pub mod context;
pub mod find;
pub mod ls;
pub mod node_info;
pub mod screenshot;
pub mod tokens;

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List a node and its descendants (or projects/files at the root).
    Ls(ls::Args),
    /// Locate nodes by a multi-token ancestor-chain query.
    Find(find::Args),
    /// List comments for a file (or node), each associated with the node it pins to.
    Comments(comments::Args),
    /// Export a node as PNG/JPG/SVG/PDF.
    Screenshot(screenshot::Args),
    /// Extract design tokens (colors, fonts, sizes, spacing, …).
    Tokens(tokens::Args),
    /// Export every icon/image/composite below a node into a directory.
    Assets(assets::Args),
    /// Aggregate: dump tree + screenshot + tokens + assets for a node.
    Context(context::Args),
    /// Comprehensive single-target view: node properties, layout, fills,
    /// effects, component metadata, bound variables, comments. Designed for
    /// Claude Code agents implementing designs in application code. Accepts
    /// node, comment, file, project, and root targets.
    NodeInfo(node_info::Args),
    /// Maintain the local file cache (prefetch / clear).
    Cache(cache::Args),
}

impl Command {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        match self {
            Self::Ls(a) => a.run(cfg, globals).await,
            Self::Find(a) => a.run(cfg, globals).await,
            Self::Comments(a) => a.run(cfg, globals).await,
            Self::Screenshot(a) => a.run(cfg, globals).await,
            Self::Tokens(a) => a.run(cfg, globals).await,
            Self::Assets(a) => a.run(cfg, globals).await,
            Self::Context(a) => a.run(cfg, globals).await,
            Self::NodeInfo(a) => a.run(cfg, globals).await,
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

/// Heuristic: does this error string look like a Variables-API 403?
/// Used by `cache prefetch` to decide whether to disable further variables
/// fetches for the rest of the run after a few consecutive 403s.
pub fn is_variables_forbidden_error(err: &str) -> bool {
    err.contains("403")
}

/// Issue a GET against the Figma REST API with the configuration's auth and
/// decode the body as JSON. Bypasses figma-api's typed deserialization;
/// see `fetch_file_json` for the rationale.
pub async fn get_json(cfg: &Configuration, url: &str) -> Result<serde_json::Value> {
    use anyhow::{anyhow, Context};
    let mut req = cfg.client.get(url);
    if let Some(ua) = &cfg.user_agent {
        req = req.header("user-agent", ua.clone());
    }
    if let Some(token) = &cfg.oauth_access_token {
        req = req.bearer_auth(token);
    }
    if let Some(apikey) = &cfg.api_key {
        let value = match &apikey.prefix {
            Some(prefix) => format!("{} {}", prefix, apikey.key),
            None => apikey.key.clone(),
        };
        req = req.header("X-Figma-Token", value);
    }
    let resp = req.send().await.context("HTTP request failed")?;
    let status = resp.status();
    let body = resp.text().await.context("reading response body")?;
    if !status.is_success() {
        return Err(anyhow!("figma API error ({status}): {body}"));
    }
    serde_json::from_str(&body).with_context(|| format!("parsing response from {url}"))
}

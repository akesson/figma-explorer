use anyhow::Result;
use clap::Subcommand;
use figma_api::apis::configuration::Configuration;

use crate::Output;

pub mod cache;
pub mod context;
pub mod extract_assets;
pub mod files;
pub mod frames;
pub mod pages;
pub mod screenshot;
pub mod search;
pub mod styles;
pub mod tree;

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List files across all projects in FIGMA_PROJECTS_IDS.
    Files(files::Args),
    /// List all pages in a file.
    Pages(pages::Args),
    /// List top-level frames on a page.
    Frames(frames::Args),
    /// Render a frame as a nested tree (skips invisible nodes).
    Tree(tree::Args),
    /// Locate nodes by a multi-token ancestor-chain hint (e.g. "wallchart grid filter button").
    Search(search::Args),
    /// Export a node as PNG/JPG/SVG/PDF.
    Screenshot(screenshot::Args),
    /// Export every icon/image/composite below a frame into a directory.
    ExtractAssets(extract_assets::Args),
    /// Extract design tokens (colors, fonts, sizes, spacing, …).
    Styles(styles::Args),
    /// Aggregate command: dump tree + screenshot + styles + assets for a frame.
    Context(context::Args),
    /// Maintain the local file cache (prefetch / clear).
    Cache(cache::Args),
}

impl Command {
    pub async fn run(self, cfg: &Configuration, format: Output) -> Result<()> {
        match self {
            Self::Files(a) => a.run(cfg, format).await,
            Self::Pages(a) => a.run(cfg, format).await,
            Self::Frames(a) => a.run(cfg, format).await,
            Self::Tree(a) => a.run(cfg, format).await,
            Self::Search(a) => a.run(cfg, format).await,
            Self::Screenshot(a) => a.run(cfg, format).await,
            Self::ExtractAssets(a) => a.run(cfg, format).await,
            Self::Styles(a) => a.run(cfg, format).await,
            Self::Context(a) => a.run(cfg, format).await,
            Self::Cache(a) => a.run(cfg, format).await,
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

/// Either explicit args or a URL — resolve down to (file_key, optional node_id).
#[derive(Debug, Clone, clap::Args)]
pub struct LocatorArgs {
    /// Figma file key (from the file URL).
    #[arg(long, conflicts_with = "url")]
    pub file_key: Option<String>,

    /// Figma file/node URL. If set, overrides --file-key and --node-id.
    #[arg(long)]
    pub url: Option<String>,

    /// Explicit Figma node id (e.g. `1:23`).
    #[arg(long, conflicts_with = "url")]
    pub node_id: Option<String>,
}

impl LocatorArgs {
    /// Resolve the locator into (file_key, optional node_id). Errors if
    /// neither URL nor file-key is supplied.
    pub fn resolve(&self) -> Result<(String, Option<String>)> {
        if let Some(url) = &self.url {
            let p = crate::url::parse(url)?;
            return Ok((p.file_key, p.node_id));
        }
        let file_key = self
            .file_key
            .clone()
            .ok_or_else(|| anyhow::anyhow!("--file-key or --url is required"))?;
        Ok((file_key, self.node_id.clone()))
    }
}

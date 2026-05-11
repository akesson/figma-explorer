use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::json;

use crate::cmd::{fetch_file_json, LocatorArgs};
use crate::node::id;
use crate::screenshot::{self as render, Format};
use crate::{print, resolve, Output};

/// Render a node as an image. Writes bytes to `--out` or prints the source URL.
#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(flatten)]
    pub locator: LocatorArgs,

    /// Page name (used with --frame).
    #[arg(long)]
    pub page: Option<String>,

    /// Frame/node name to render.
    #[arg(long)]
    pub frame: Option<String>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Png)]
    pub img_format: Format,

    /// Scale factor (Figma's `/images` accepts 0.01–4).
    #[arg(long, default_value_t = 2.0)]
    pub scale: f64,

    /// Path to write the rendered file to. If omitted, the rendered URL is
    /// printed instead (no bytes are downloaded).
    #[arg(long)]
    pub out: Option<PathBuf>,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, format: Output) -> Result<()> {
        let (file_key, url_node_id) = self.locator.resolve()?;

        let target_id = if let Some(nid) = url_node_id
            .or_else(|| self.locator.node_id.clone())
        {
            nid
        } else {
            // Resolve by name. We need to fetch the file to find the node id.
            let file = fetch_file_json(cfg, &file_key, None).await?;
            let doc = &file["document"];
            let page_query = self.page.as_deref().ok_or_else(|| {
                anyhow!("--page is required (or pin the target with --node-id/--url)")
            })?;
            let page = resolve::resolve_page(doc, page_query)
                .ok_or_else(|| anyhow!("no page matching {page_query:?}"))?;
            let node = match self.frame.as_deref() {
                Some(q) => resolve::resolve_frame(page, q).ok_or_else(|| {
                    anyhow!(
                        "no frame matching {q:?} on page {:?}",
                        crate::node::name(page).unwrap_or("")
                    )
                })?,
                None => page,
            };
            id(node)
                .ok_or_else(|| anyhow!("target node has no id"))?
                .to_owned()
        };

        match &self.out {
            Some(path) => {
                let rendered =
                    render::render_node(cfg, &file_key, &target_id, self.scale, self.img_format)
                        .await?;
                std::fs::write(path, &rendered.bytes)?;
                let out = json!({
                    "file_key": file_key,
                    "node_id": target_id,
                    "wrote": path.display().to_string(),
                    "bytes": rendered.bytes.len(),
                    "source_url": rendered.source_url,
                });
                print(&out, format)
            }
            None => {
                let urls = render::render_urls(
                    cfg,
                    &file_key,
                    std::slice::from_ref(&target_id),
                    self.scale,
                    self.img_format,
                )
                .await?;
                let url = urls
                    .get(&target_id)
                    .cloned()
                    .ok_or_else(|| anyhow!("Figma returned no URL for node {target_id}"))?;
                let out = json!({
                    "file_key": file_key,
                    "node_id": target_id,
                    "url": url,
                });
                print(&out, format)
            }
        }
    }
}


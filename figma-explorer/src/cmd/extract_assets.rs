use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::json;

use crate::assets;
use crate::cmd::{fetch_file_json, LocatorArgs};
use crate::resolve;
use crate::{print, Output};

/// Walk a frame and export every icon (as SVG) and image/composite (as PNG)
/// into a directory tree.
#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(flatten)]
    pub locator: LocatorArgs,

    /// Page name (used with --frame).
    #[arg(long)]
    pub page: Option<String>,

    /// Frame name. If omitted, the whole page is walked.
    #[arg(long)]
    pub frame: Option<String>,

    /// Destination directory. Will be created if missing.
    #[arg(long, default_value = "figma-assets")]
    pub out_dir: PathBuf,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, format: Output) -> Result<()> {
        let (file_key, url_node_id) = self.locator.resolve()?;
        let file = fetch_file_json(cfg, &file_key, None).await?;
        let doc = &file["document"];

        let target = if let Some(nid) = url_node_id
            .as_deref()
            .or(self.locator.node_id.as_deref())
        {
            resolve::resolve_node_id(doc, nid)
                .ok_or_else(|| anyhow!("no node with id {nid}"))?
        } else {
            let page_query = self.page.as_deref().ok_or_else(|| {
                anyhow!("--page is required (or pin the target with --node-id/--url)")
            })?;
            let page = resolve::resolve_page(doc, page_query)
                .ok_or_else(|| anyhow!("no page matching {page_query:?}"))?;
            match self.frame.as_deref() {
                Some(q) => resolve::resolve_frame(page, q).ok_or_else(|| {
                    anyhow!(
                        "no frame matching {q:?} on page {:?}",
                        crate::node::name(page).unwrap_or("")
                    )
                })?,
                None => page,
            }
        };

        let manifest = assets::extract(cfg, &file_key, target, &self.out_dir).await?;
        let out = json!({
            "out_dir": self.out_dir.display().to_string(),
            "icons": manifest.icons.len(),
            "images": manifest.images.len(),
            "composites": manifest.composites.len(),
            "failed": manifest.failed.len(),
            "manifest": serde_json::to_value(&manifest)?,
        });
        print(&out, format)
    }
}

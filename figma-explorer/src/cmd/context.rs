use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;

use crate::cmd::{fetch_file_json, LocatorArgs};
use crate::{context as ctx, print, resolve, Output};

/// Bundled export of everything needed to implement a frame in code.
#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(flatten)]
    pub locator: LocatorArgs,

    /// Page name (used with --frame).
    #[arg(long)]
    pub page: Option<String>,

    /// Frame name (or pass --node-id / --url).
    #[arg(long)]
    pub frame: Option<String>,

    /// Output directory. Will be created. Existing files will be overwritten.
    #[arg(long, default_value = "figma-context")]
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
            let page_q = self.page.as_deref().unwrap_or("");
            let page = resolve::resolve_page(doc, page_q)
                .ok_or_else(|| anyhow!("no page matching {page_q:?}"))?;
            match self.frame.as_deref() {
                Some(q) => resolve::resolve_frame(page, q)
                    .ok_or_else(|| anyhow!("no frame matching {q:?} on page"))?,
                None => page,
            }
        };

        let summary = ctx::build(cfg, &file_key, &file, target, &self.out_dir).await?;
        print(&summary, format)
    }
}

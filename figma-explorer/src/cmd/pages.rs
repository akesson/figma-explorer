use anyhow::Result;
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::json;

use crate::cmd::{fetch_file_json, LocatorArgs};
use crate::node::{children, is_visible, name};
use crate::{print, Output};

/// List every top-level page (CANVAS node) in a file.
#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(flatten)]
    pub locator: LocatorArgs,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, format: Output) -> Result<()> {
        let (file_key, _) = self.locator.resolve()?;
        // depth=1 is the minimum that returns canvases.
        let file = fetch_file_json(cfg, &file_key, Some(1.0)).await?;
        let doc = &file["document"];
        let pages: Vec<String> = children(doc)
            .iter()
            .filter(|p| is_visible(p))
            .filter_map(|p| name(p).map(str::to_owned))
            .collect();
        let out = json!({
            "file_key": file_key,
            "file_name": file.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            "pages": pages,
        });
        print(&out, format)
    }
}

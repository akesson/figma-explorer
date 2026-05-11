use anyhow::Result;
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::json;

use crate::cmd::{fetch_file_json, LocatorArgs};
use crate::node::{children, id, is_visible, name};
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
        let pages: Vec<_> = children(doc)
            .iter()
            .map(|p| {
                json!({
                    "id": id(p).unwrap_or(""),
                    "name": name(p).unwrap_or(""),
                    "visible": is_visible(p),
                })
            })
            .collect();
        let out = json!({
            "file_key": file_key,
            "file_name": file.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            "pages": pages,
        });
        print(&out, format)
    }
}

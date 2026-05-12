use anyhow::Result;
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::json;

use crate::cache;
use crate::cmd::LocatorArgs;
use crate::tree::format_cache_node_line;
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
        let file = cache::load_file(cfg, &file_key).await?;
        let pages: Vec<&cache::CacheNode> =
            file.document.children.iter().filter(|p| p.visible).collect();

        let value = match format {
            Output::Yaml => {
                let lines: Vec<String> = pages.iter().map(|p| format_cache_node_line(p)).collect();
                json!(lines)
            }
            Output::Json => json!({
                "file_key": file_key,
                "file_name": file.name,
                "pages": pages.iter().map(|p| json!({
                    "node_id": p.id,
                    "name": p.name,
                })).collect::<Vec<_>>(),
            }),
        };
        print(&value, format)
    }
}

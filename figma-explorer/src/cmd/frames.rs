use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::json;

use crate::cmd::{fetch_file_json, LocatorArgs};
use crate::node::{bounds, children, id, is_visible, name};
use crate::resolve::resolve_page;
use crate::{print, Output};

/// List the top-level frames on a given page.
#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(flatten)]
    pub locator: LocatorArgs,

    /// Page name (exact, substring, or fuzzy).
    #[arg(long)]
    pub page: String,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, format: Output) -> Result<()> {
        let (file_key, _) = self.locator.resolve()?;
        // depth=2 gets us pages and their top-level frame children.
        let file = fetch_file_json(cfg, &file_key, Some(2.0)).await?;
        let doc = &file["document"];
        let page = resolve_page(doc, &self.page)
            .ok_or_else(|| anyhow!("no page matching {:?}", self.page))?;
        let frames: Vec<_> = children(page)
            .iter()
            .filter(|n| is_visible(n))
            .map(|f| {
                let bb = bounds(f).map(|b| {
                    json!({ "width": b.width, "height": b.height, "x": b.x, "y": b.y })
                });
                json!({
                    "id": id(f).unwrap_or(""),
                    "name": name(f).unwrap_or(""),
                    "type": f.get("type").and_then(|v| v.as_str()).unwrap_or(""),
                    "bounds": bb,
                })
            })
            .collect();
        let out = json!({
            "file_key": file_key,
            "page": name(page).unwrap_or(""),
            "frames": frames,
        });
        print(&out, format)
    }
}

use anyhow::Result;
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::json;

use crate::cmd::{fetch_file_json, LocatorArgs};
use crate::resolve;
use crate::{print, Output};

/// Fuzzy-search for nodes by name across the whole file.
#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(flatten)]
    pub locator: LocatorArgs,

    /// Free-text query. Matched (case-insensitive) against every visible
    /// node's name; results are ranked by fuzzy score.
    #[arg(long)]
    pub query: String,

    /// Maximum number of hits to report.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, format: Output) -> Result<()> {
        let (file_key, _) = self.locator.resolve()?;
        let file = fetch_file_json(cfg, &file_key, None).await?;
        let doc = &file["document"];
        let hits = resolve::fuzzy_search(doc, &self.query, self.limit);

        let value = match format {
            Output::Yaml => {
                let lines: Vec<String> = hits
                    .iter()
                    .map(|h| {
                        let kind = if h.kind.is_empty() { "?" } else { &h.kind };
                        let mut line = format!("{} \"{}\" id:{}", kind, h.name, h.node_id);
                        if !h.path.is_empty() {
                            line.push_str(&format!(" ({})", h.path.join(" > ")));
                        }
                        line
                    })
                    .collect();
                json!(lines)
            }
            Output::Json => json!({
                "file_key": file_key,
                "query": self.query,
                "hits": hits.iter().map(|h| json!({
                    "node_id": h.node_id,
                    "name": h.name,
                    "type": h.kind,
                    "path": h.path,
                })).collect::<Vec<_>>(),
            }),
        };
        print(&value, format)
    }
}

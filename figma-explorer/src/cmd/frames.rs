use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::{json, Map, Value};

use crate::cache;
use crate::cmd::LocatorArgs;
use crate::resolve::resolve_page_cache;
use crate::tree::format_cache_node_line;
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
        let file = cache::load_file(cfg, &file_key).await?;
        let page = resolve_page_cache(&file.document, &self.page)
            .ok_or_else(|| anyhow!("no page matching {:?}", self.page))?;
        let frames: Vec<&cache::CacheNode> =
            page.children.iter().filter(|n| n.visible).collect();

        let value = match format {
            Output::Yaml => {
                let lines: Vec<String> = frames.iter().map(|f| format_cache_node_line(f)).collect();
                json!(lines)
            }
            Output::Json => {
                let frame_objs: Vec<Value> = frames
                    .iter()
                    .map(|f| {
                        let mut obj = Map::new();
                        obj.insert("node_id".into(), json!(f.id));
                        obj.insert("name".into(), json!(f.name));
                        obj.insert("type".into(), json!(f.type_));
                        if let Some(b) = f.bounds {
                            obj.insert("bounds".into(), json!(b.to_string()));
                        }
                        Value::Object(obj)
                    })
                    .collect();
                json!({
                    "file_key": file_key,
                    "page": page.name,
                    "frames": frame_objs,
                })
            }
        };
        print(&value, format)
    }
}

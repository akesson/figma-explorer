use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::{json, Map, Value};

use crate::cache;
use crate::cmd::LocatorArgs;
use crate::node::{bounds, children, id, is_visible, name};
use crate::resolve::resolve_page;
use crate::tree::format_node_line;
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
        let file = cache::load_file_doc(cfg, &file_key).await?;
        let doc = &file["document"];
        let page = resolve_page(doc, &self.page)
            .ok_or_else(|| anyhow!("no page matching {:?}", self.page))?;
        let frames: Vec<&Value> = children(page).iter().filter(|n| is_visible(n)).collect();

        let value = match format {
            Output::Yaml => {
                let lines: Vec<String> = frames.iter().map(|f| format_node_line(f)).collect();
                json!(lines)
            }
            Output::Json => {
                let frame_objs: Vec<Value> = frames
                    .iter()
                    .map(|f| {
                        let mut obj = Map::new();
                        if let Some(nid) = id(f) {
                            obj.insert("node_id".into(), json!(nid));
                        }
                        obj.insert("name".into(), json!(name(f).unwrap_or("")));
                        obj.insert(
                            "type".into(),
                            json!(f.get("type").and_then(|v| v.as_str()).unwrap_or("")),
                        );
                        if let Some(b) = bounds(f) {
                            obj.insert("bounds".into(), json!(b.to_string()));
                        }
                        Value::Object(obj)
                    })
                    .collect();
                json!({
                    "file_key": file_key,
                    "page": name(page).unwrap_or(""),
                    "frames": frame_objs,
                })
            }
        };
        print(&value, format)
    }
}

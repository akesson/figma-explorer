use anyhow::Result;
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::{json, Value};

use crate::cache;
use crate::cmd::LocatorArgs;
use crate::{print, resolve, tree, Output};

/// Render a target node as a nested tree.
///
/// Default (compact YAML) emits the structured tree directly — no metadata
/// wrapper. `--json` adds `file_key`/`node_id`/`name` headers around the same
/// tree. Invisible nodes are skipped at every level.
#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(flatten)]
    pub locator: LocatorArgs,

    /// Page name. Required unless --node-id/--url pins the target directly.
    #[arg(long)]
    pub page: Option<String>,

    /// Frame/node name to resolve within the page.
    #[arg(long)]
    pub frame: Option<String>,

    /// Max traversal depth (default 6 — deeper trees can get long).
    #[arg(long, default_value_t = 6)]
    pub depth: usize,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, format: Output) -> Result<()> {
        let (file_key, url_node_id) = self.locator.resolve()?;
        let file = cache::load_file_doc(cfg, &file_key).await?;
        let doc = &file["document"];

        let target: &Value = if let Some(nid) = url_node_id
            .as_deref()
            .or(self.locator.node_id.as_deref())
        {
            resolve::resolve_node_id(doc, nid)
                .ok_or_else(|| anyhow::anyhow!("no node with id {nid}"))?
        } else {
            let page_query = self.page.as_deref().ok_or_else(|| {
                anyhow::anyhow!("--page is required (or pin the target with --node-id/--url)")
            })?;
            let page = resolve::resolve_page(doc, page_query)
                .ok_or_else(|| anyhow::anyhow!("no page matching {page_query:?}"))?;
            match self.frame.as_deref() {
                Some(q) => resolve::resolve_frame(page, q).ok_or_else(|| {
                    anyhow::anyhow!(
                        "no frame matching {q:?} on page {:?}",
                        crate::node::name(page).unwrap_or("")
                    )
                })?,
                None => page,
            }
        };

        let value = match format {
            Output::Yaml => tree::render_compact(target, self.depth),
            Output::Json => json!({
                "file_key": file_key,
                "node_id": crate::node::id(target),
                "name": crate::node::name(target),
                "tree": tree::render_structured(target, self.depth),
            }),
        };
        print(&value, format)
    }
}

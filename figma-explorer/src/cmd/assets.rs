//! `assets` — export every icon (SVG) and image/composite (PNG) below a node
//! into a directory. Replaces the legacy `extract-assets` command.
//!
//! Asset detection walks live Figma JSON (fills, exportSettings, vector
//! geometry — fields the cache projection drops), so we live-fetch the
//! document through `fetch_file_json`. The id resolver gives us
//! `(file_key, node_id)`; everything downstream is unchanged.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::json;

use crate::assets;
use crate::cmd::fetch_file_json;
use crate::node_search;
use crate::resolver::{parse_id, render_resolve_error, ResolvedTarget, Resolver};
use crate::{print, Globals};

/// Walk a node's subtree and export every icon and image/composite to disk.
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Tagged or native ID, or a Figma URL pointing at the subtree to export.
    /// `file:N` walks the whole document.
    pub id: String,

    /// Destination directory. Will be created if missing.
    #[arg(long, default_value = "figma-assets")]
    pub out_dir: PathBuf,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        let resolver = Resolver::new(globals.cache_only)?;
        let format = globals.output;
        let id = parse_id(&self.id).map_err(|e| anyhow!("{e}"))?;
        let target = resolver
            .resolve(cfg, &id)
            .await
            .map_err(|e| render_resolve_error(e, format))?;

        let (file_key, node_id) = match target {
            ResolvedTarget::Node { meta, node, .. } => (meta.file_key, Some(node.id)),
            ResolvedTarget::File { meta, .. } => (meta.file_key, None),
            ResolvedTarget::Root | ResolvedTarget::Project { .. } => {
                anyhow::bail!("assets needs a file or node-level id; got {}", self.id);
            }
            ResolvedTarget::Comment { .. } => {
                anyhow::bail!(
                    "assets does not accept comment ids ({}); use `node-info` for a comment",
                    self.id
                );
            }
        };

        let file = fetch_file_json(cfg, &file_key, None).await?;
        let doc = &file["document"];

        let target_value = match &node_id {
            Some(nid) => node_search::resolve_node_id(doc, nid)
                .ok_or_else(|| anyhow!("no node with id {nid}"))?,
            None => doc,
        };

        let manifest = assets::extract(cfg, &file_key, target_value, &self.out_dir).await?;
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

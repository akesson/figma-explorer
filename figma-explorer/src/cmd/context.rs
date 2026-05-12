//! `context` — bundled export: tree + screenshot + tokens + assets into one
//! directory. The "give me everything to implement this frame in code" command.
//!
//! Wraps the existing `crate::context::build` helper, which takes a live
//! `&Value` document node. We resolve the user's ID to a `(file_key, node_id)`
//! pair, fetch the live document, and pass the appropriate subtree down.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;

use crate::cmd::fetch_file_json;
use crate::resolve;
use crate::resolver::{parse_id, render_resolve_error, ResolvedTarget, Resolver};
use crate::{context as ctx, print, Globals};

/// Bundled export of everything needed to implement a node in code.
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Tagged or native ID, or a Figma URL pointing at the node to bundle.
    pub id: String,

    /// Output directory. Will be created. Existing files will be overwritten.
    #[arg(long, default_value = "figma-context")]
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

        let (file_key, node_id, file_synth) = match target {
            ResolvedTarget::Node { meta, node, file_synth } => {
                (meta.file_key, Some(node.id), file_synth)
            }
            ResolvedTarget::File { meta, document, synth } => {
                // For a bare file:N, bundle the whole document — use its root id.
                (meta.file_key, Some(document.document.id.clone()), synth)
            }
            ResolvedTarget::Root | ResolvedTarget::Project { .. } => {
                anyhow::bail!(
                    "context needs a file or node-level id; got {}",
                    self.id
                );
            }
        };

        let file = fetch_file_json(cfg, &file_key, None).await?;
        let doc = &file["document"];

        let target_value = match &node_id {
            Some(nid) => resolve::resolve_node_id(doc, nid)
                .ok_or_else(|| anyhow!("no node with id {nid}"))?,
            None => doc,
        };

        let summary =
            ctx::build(cfg, &file_key, file_synth, &file, target_value, &self.out_dir).await?;
        print(&summary, format)
    }
}

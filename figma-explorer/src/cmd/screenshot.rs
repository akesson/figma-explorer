use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::json;

use crate::resolver::{parse_id, render_resolve_error, ResolvedTarget, Resolver};
use crate::screenshot::{self as render, Format};
use crate::{print, Globals};

/// Render a node as an image. Writes bytes to `--out` or prints the source
/// URL. Accepts any qualified node id (`file:N:x:y`), a bare native id, or a
/// Figma URL. File-level locators (`file:N`) screenshot the document root.
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Tagged or native ID, or a Figma URL pointing at the node to render.
    pub id: String,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Png)]
    pub img_format: Format,

    /// Scale factor (Figma's `/images` accepts 0.01–4).
    #[arg(long, default_value_t = 2.0)]
    pub scale: f64,

    /// Path to write the rendered file to. If omitted, the rendered URL is
    /// printed instead (no bytes are downloaded).
    #[arg(long)]
    pub out: Option<PathBuf>,
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

        // For screenshot we always need (file_key, node_id). File-level
        // targets fall back to the file's DOCUMENT id; ResolvedTarget::Root /
        // ::Project are rejected — there's nothing to render at that level.
        let (file_key, node_id, display_id) = match target {
            ResolvedTarget::Node { file_synth, meta, node } => {
                let node_id = node.id.clone();
                let display_id = if node.id.is_empty() {
                    format!("file:{file_synth}")
                } else {
                    format!("file:{file_synth}:{}", node.id)
                };
                (meta.file_key, node_id, display_id)
            }
            ResolvedTarget::File { synth, meta, document } => (
                meta.file_key,
                document.document.id,
                format!("file:{synth}"),
            ),
            ResolvedTarget::Root | ResolvedTarget::Project { .. } => {
                anyhow::bail!(
                    "screenshot needs a node-level id (file:N:x:y, a bare x:y, or a Figma URL); got {}",
                    self.id
                );
            }
        };

        match &self.out {
            Some(path) => {
                let rendered =
                    render::render_node(cfg, &file_key, &node_id, self.scale, self.img_format)
                        .await?;
                std::fs::write(path, &rendered.bytes)?;
                let out = json!({
                    "id": display_id,
                    "file_key": file_key,
                    "node_id": node_id,
                    "wrote": path.display().to_string(),
                    "bytes": rendered.bytes.len(),
                    "source_url": rendered.source_url,
                });
                print(&out, format)
            }
            None => {
                let urls = render::render_urls(
                    cfg,
                    &file_key,
                    std::slice::from_ref(&node_id),
                    self.scale,
                    self.img_format,
                )
                .await?;
                let url = urls
                    .get(&node_id)
                    .cloned()
                    .ok_or_else(|| anyhow!("Figma returned no URL for node {node_id}"))?;
                let out = json!({
                    "id": display_id,
                    "file_key": file_key,
                    "node_id": node_id,
                    "url": url,
                });
                print(&out, format)
            }
        }
    }
}


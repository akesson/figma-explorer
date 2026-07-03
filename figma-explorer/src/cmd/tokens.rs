//! `tokens` — extract design tokens (colors, fonts, sizes, spacing, radii,
//! shadows, grids) from a node's subtree and/or the file's published
//! variables. Replaces the legacy `styles` command.
//!
//! Token extraction needs full Figma JSON (fills, strokes, type styles —
//! fields the cache projection drops). We resolve the user's ID to a
//! `(file_key, node_id)` pair via the resolver, then fetch the live document
//! JSON through `fetch_file_json` and pass the matching JSON subtree to the
//! existing token-collection machinery in `crate::styles`.

use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;

use crate::cmd::{fetch_file_json, require_document};
use crate::node_search;
use crate::resolver::{parse_id, render_resolve_error, ResolvedTarget, Resolver};
use crate::styles::{self as st, Category, Format, Scope};
use crate::{print, Globals};

/// Extract design tokens for a target node (or whole file with `--scope file`).
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Tagged or native ID, or a Figma URL. `file:N` collects from the whole
    /// document; `file:N:x:y` (or bare/url) scopes to that subtree.
    pub id: String,

    /// Where to look for tokens.
    #[arg(long, value_enum, default_value_t = Scope::Both)]
    pub scope: Scope,

    /// Output format for the tokens themselves.
    #[arg(long, value_enum, default_value_t = Format::Tokens)]
    pub as_: Format,

    /// Restrict to specific categories (comma-separated). When unset, all
    /// categories are included.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub only: Vec<Category>,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, globals: &Globals) -> Result<()> {
        // Tokens always re-fetch the live document (the cache projection drops
        // fills/strokes/type styles), so there is nothing to serve offline.
        if globals.cache_only {
            anyhow::bail!("tokens needs a live fetch; drop --cache-only");
        }
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
                anyhow::bail!("tokens needs a file or node-level id; got {}", self.id);
            }
            ResolvedTarget::Comment { .. } => {
                anyhow::bail!(
                    "tokens does not accept comment ids ({}); use `node-info` for a comment",
                    self.id
                );
            }
        };

        // Token extraction reads fills/strokes/effects/style — fields the
        // cache projection drops. So we re-fetch the live JSON document here.
        let file = fetch_file_json(cfg, &file_key, None).await?;
        let doc = require_document(&file, &file_key)?;

        let target_value = match &node_id {
            Some(nid) => Some(
                node_search::resolve_node_id(doc, nid)
                    .ok_or_else(|| anyhow!("no node with id {nid}"))?,
            ),
            None => None,
        };

        let mut tokens = st::Tokens::default();
        match self.scope {
            Scope::Target => st::collect_from_target(target_value.unwrap_or(doc), &mut tokens),
            Scope::File => st::merge_published(&file, &mut tokens),
            Scope::Both => {
                st::collect_from_target(target_value.unwrap_or(doc), &mut tokens);
                st::merge_published(&file, &mut tokens);
            }
        }
        st::filter(&mut tokens, &self.only);

        let rendered = st::render(&tokens, self.as_);
        if self.as_ == Format::Css {
            if let Some(s) = rendered.as_str() {
                print!("{s}");
                return Ok(());
            }
        }
        print(&rendered, format)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cache_only_rejected_before_any_fetch() {
        let args = Args {
            id: "file:1".into(),
            scope: Scope::Both,
            as_: Format::Tokens,
            only: vec![],
        };
        let globals = Globals {
            output: crate::Output::Yaml,
            cache_only: true,
            scope: None,
        };
        // Bails before constructing the resolver or hitting the network.
        let err = args.run(&Configuration::new(), &globals).await.unwrap_err();
        assert!(err.to_string().contains("--cache-only"), "got: {err}");
    }
}

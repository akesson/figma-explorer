use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;

use crate::cmd::{fetch_file_json, LocatorArgs};
use crate::resolve;
use crate::styles::{self as st, Category, Format, Scope};
use crate::{print, Output};

/// Extract design tokens (colors, fonts, sizes, spacing, radii, shadows,
/// grids). When `--as css` is used the result is a `:root { … }` block
/// printed verbatim; otherwise the global `--format yaml|json` controls
/// shape.
#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(flatten)]
    pub locator: LocatorArgs,

    /// Page name (used with --frame to scope to a single subtree).
    #[arg(long)]
    pub page: Option<String>,

    /// Frame name to scope token collection. If omitted, the whole page (or
    /// whole document, when --page is also omitted) is walked.
    #[arg(long)]
    pub frame: Option<String>,

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
    pub async fn run(self, cfg: &Configuration, format: Output) -> Result<()> {
        let (file_key, url_node_id) = self.locator.resolve()?;
        let file = fetch_file_json(cfg, &file_key, None).await?;
        let doc = &file["document"];

        let target_opt = if let Some(nid) = url_node_id
            .as_deref()
            .or(self.locator.node_id.as_deref())
        {
            Some(
                resolve::resolve_node_id(doc, nid)
                    .ok_or_else(|| anyhow!("no node with id {nid}"))?,
            )
        } else if let Some(page_q) = self.page.as_deref() {
            let page = resolve::resolve_page(doc, page_q)
                .ok_or_else(|| anyhow!("no page matching {page_q:?}"))?;
            Some(match self.frame.as_deref() {
                Some(q) => resolve::resolve_frame(page, q)
                    .ok_or_else(|| anyhow!("no frame matching {q:?} on page"))?,
                None => page,
            })
        } else {
            None
        };

        let mut tokens = st::Tokens::default();
        match self.scope {
            Scope::Target => {
                if let Some(t) = target_opt {
                    st::collect_from_target(t, &mut tokens);
                } else {
                    // No target was specified — scope=target with no anchor
                    // is treated as "the whole document".
                    st::collect_from_target(doc, &mut tokens);
                }
            }
            Scope::File => {
                st::merge_published(&file, &mut tokens);
            }
            Scope::Both => {
                st::collect_from_target(target_opt.unwrap_or(doc), &mut tokens);
                st::merge_published(&file, &mut tokens);
            }
        }
        st::filter(&mut tokens, &self.only);

        // For CSS format, just print the text directly — wrapping CSS in
        // YAML/JSON quotes would be useless.
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

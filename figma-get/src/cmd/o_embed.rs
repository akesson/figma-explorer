use clap::Args;
use figma_api::apis::configuration::Configuration;
use figma_api::apis::o_embed_api as api;

use crate::finalize;

#[derive(Args, Debug)]
pub struct OEmbedArgs {
    /// URL of the Figma file or published Make site.
    #[arg(long)]
    pub url: String,
    /// Maximum width of the embed in pixels. Defaults to 800.
    #[arg(long)]
    pub maxwidth: Option<i32>,
    /// Maximum height of the embed in pixels. Defaults to 450.
    #[arg(long)]
    pub maxheight: Option<i32>,
}

impl OEmbedArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetOEmbedParams {
            url: self.url,
            maxwidth: self.maxwidth,
            maxheight: self.maxheight,
        };
        finalize(api::get_o_embed(cfg, params).await)
    }
}

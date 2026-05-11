use clap::Args;
use figma_api::apis::configuration::Configuration;
use figma_api::apis::dev_resources_api as api;

use crate::finalize;

#[derive(Args, Debug)]
pub struct DevResourcesArgs {
    /// Main file key (not a branch key).
    #[arg(long)]
    pub file_key: String,
    /// Comma-separated node IDs to filter by. Returns all if omitted.
    #[arg(long)]
    pub node_ids: Option<String>,
}

impl DevResourcesArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetDevResourcesParams {
            file_key: self.file_key,
            node_ids: self.node_ids,
        };
        finalize(api::get_dev_resources(cfg, params).await)
    }
}

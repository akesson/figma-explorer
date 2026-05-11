use clap::Args;
use figma_api::apis::configuration::Configuration;
use figma_api::apis::variables_api as api;

use crate::finalize;

#[derive(Args, Debug)]
pub struct LocalVariablesArgs {
    /// File key or branch key.
    #[arg(long)]
    pub file_key: String,
}

impl LocalVariablesArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetLocalVariablesParams {
            file_key: self.file_key,
        };
        finalize(api::get_local_variables(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct PublishedVariablesArgs {
    /// Main file key (not a branch key).
    #[arg(long)]
    pub file_key: String,
}

impl PublishedVariablesArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetPublishedVariablesParams {
            file_key: self.file_key,
        };
        finalize(api::get_published_variables(cfg, params).await)
    }
}

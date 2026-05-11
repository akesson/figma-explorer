use clap::Args;
use figma_api::apis::components_api as api;
use figma_api::apis::configuration::Configuration;

use crate::finalize;

#[derive(Args, Debug)]
pub struct ComponentArgs {
    /// Component key.
    #[arg(long)]
    pub key: String,
}

impl ComponentArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetComponentParams { key: self.key };
        finalize(api::get_component(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct FileComponentsArgs {
    /// Main file key (not a branch key).
    #[arg(long)]
    pub file_key: String,
}

impl FileComponentsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetFileComponentsParams {
            file_key: self.file_key,
        };
        finalize(api::get_file_components(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct TeamComponentsArgs {
    /// Team ID.
    #[arg(long, env = "FIGMA_TEAM_ID")]
    pub team_id: String,
    /// Page size. Defaults to 30; max 1000.
    #[arg(long)]
    pub page_size: Option<f64>,
    /// Cursor for items after this ID (exclusive with --before).
    #[arg(long)]
    pub after: Option<f64>,
    /// Cursor for items before this ID (exclusive with --after).
    #[arg(long)]
    pub before: Option<f64>,
}

impl TeamComponentsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetTeamComponentsParams {
            team_id: self.team_id,
            page_size: self.page_size,
            after: self.after,
            before: self.before,
        };
        finalize(api::get_team_components(cfg, params).await)
    }
}

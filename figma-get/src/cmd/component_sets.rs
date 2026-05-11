use clap::Args;
use figma_api::apis::component_sets_api as api;
use figma_api::apis::configuration::Configuration;

use crate::finalize;

#[derive(Args, Debug)]
pub struct ComponentSetArgs {
    /// Component set key.
    #[arg(long)]
    pub key: String,
}

impl ComponentSetArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetComponentSetParams { key: self.key };
        finalize(api::get_component_set(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct FileComponentSetsArgs {
    /// Main file key (not a branch key).
    #[arg(long)]
    pub file_key: String,
}

impl FileComponentSetsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetFileComponentSetsParams {
            file_key: self.file_key,
        };
        finalize(api::get_file_component_sets(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct TeamComponentSetsArgs {
    /// Team ID.
    #[arg(long, env = "FIGMA_TEAM_ID")]
    pub team_id: String,
    /// Page size. Defaults to 30.
    #[arg(long)]
    pub page_size: Option<f64>,
    /// Cursor for items after this ID (exclusive with --before).
    #[arg(long)]
    pub after: Option<f64>,
    /// Cursor for items before this ID (exclusive with --after).
    #[arg(long)]
    pub before: Option<f64>,
}

impl TeamComponentSetsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetTeamComponentSetsParams {
            team_id: self.team_id,
            page_size: self.page_size,
            after: self.after,
            before: self.before,
        };
        finalize(api::get_team_component_sets(cfg, params).await)
    }
}

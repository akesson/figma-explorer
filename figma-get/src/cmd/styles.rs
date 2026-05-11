use clap::Args;
use figma_api::apis::configuration::Configuration;
use figma_api::apis::styles_api as api;

use crate::finalize;

#[derive(Args, Debug)]
pub struct FileStylesArgs {
    /// Main file key (not a branch key).
    #[arg(long)]
    pub file_key: String,
}

impl FileStylesArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetFileStylesParams {
            file_key: self.file_key,
        };
        finalize(api::get_file_styles(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct StyleArgs {
    /// Style key.
    #[arg(long)]
    pub key: String,
}

impl StyleArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetStyleParams { key: self.key };
        finalize(api::get_style(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct TeamStylesArgs {
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

impl TeamStylesArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetTeamStylesParams {
            team_id: self.team_id,
            page_size: self.page_size,
            after: self.after,
            before: self.before,
        };
        finalize(api::get_team_styles(cfg, params).await)
    }
}

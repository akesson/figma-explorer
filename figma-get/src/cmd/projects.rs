use clap::Args;
use figma_api::apis::configuration::Configuration;
use figma_api::apis::projects_api as api;

use crate::finalize;

#[derive(Args, Debug)]
pub struct ProjectFilesArgs {
    /// Project ID.
    #[arg(long)]
    pub project_id: String,
    /// Include branch metadata for files with branches.
    #[arg(long)]
    pub branch_data: Option<bool>,
}

impl ProjectFilesArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetProjectFilesParams {
            project_id: self.project_id,
            branch_data: self.branch_data,
        };
        finalize(api::get_project_files(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct TeamProjectsArgs {
    /// Team ID.
    #[arg(long)]
    pub team_id: String,
}

impl TeamProjectsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetTeamProjectsParams {
            team_id: self.team_id,
        };
        finalize(api::get_team_projects(cfg, params).await)
    }
}

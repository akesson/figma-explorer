use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use figma_api::apis::projects_api as api;
use futures::future::try_join_all;
use serde_json::json;

use crate::{into_anyhow, print, Output};

/// List files in every project named by FIGMA_PROJECTS_IDS (or --project-ids).
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Comma-separated list of Figma project IDs. Falls back to FIGMA_PROJECTS_IDS.
    #[arg(long, env = "FIGMA_PROJECTS_IDS", value_delimiter = ',', required = true)]
    pub project_ids: Vec<String>,

    /// Include branch metadata for files with branches.
    #[arg(long)]
    pub branch_data: Option<bool>,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, format: Output) -> Result<()> {
        let branch_data = self.branch_data;
        let fetches = self.project_ids.into_iter().map(|pid| async move {
            let params = api::GetProjectFilesParams {
                project_id: pid.clone(),
                branch_data,
            };
            let resp = api::get_project_files(cfg, params)
                .await
                .map_err(into_anyhow)
                .with_context(|| format!("listing files for project {pid}"))?;
            Ok::<_, anyhow::Error>(json!({
                "project_name": resp.name,
                "project_id": pid,
                "files": resp.files.into_iter().map(|f| json!({
                    "name": f.name,
                    "key": f.key,
                    "last_modified": f.last_modified,
                })).collect::<Vec<_>>(),
            }))
        });
        let projects: Vec<serde_json::Value> = try_join_all(fetches).await?;
        print(&json!({ "projects": projects }), format)
    }
}

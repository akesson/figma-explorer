use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use figma_api::apis::projects_api as api;
use futures::future::{join_all, try_join_all};
use serde_json::json;

use crate::cmd::fetch_file_json;
use crate::node::{children, is_visible, name};
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

    /// For each file, also fetch its page list. Costs one extra API call per file.
    #[arg(long)]
    pub with_pages: bool,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, format: Output) -> Result<()> {
        let branch_data = self.branch_data;
        let with_pages = self.with_pages;
        let fetches = self.project_ids.into_iter().map(|pid| async move {
            let params = api::GetProjectFilesParams {
                project_id: pid.clone(),
                branch_data,
            };
            let resp = api::get_project_files(cfg, params)
                .await
                .map_err(into_anyhow)
                .with_context(|| format!("listing files for project {pid}"))?;

            let file_jsons = if with_pages {
                let page_fetches = resp.files.iter().map(|f| {
                    let key = f.key.clone();
                    async move {
                        fetch_file_json(cfg, &key, Some(1.0)).await.map(|file| {
                            children(&file["document"])
                                .iter()
                                .filter(|p| is_visible(p))
                                .filter_map(|p| name(p).map(str::to_owned))
                                .collect::<Vec<String>>()
                        })
                    }
                });
                let pages_per_file = join_all(page_fetches).await;
                resp.files
                    .into_iter()
                    .zip(pages_per_file)
                    .map(|(f, pages_result)| match pages_result {
                        Ok(pages) => json!({
                            "name": f.name,
                            "key": f.key,
                            "last_modified": f.last_modified,
                            "pages": pages,
                        }),
                        Err(e) => json!({
                            "name": f.name,
                            "key": f.key,
                            "last_modified": f.last_modified,
                            "pages_error": e.to_string(),
                        }),
                    })
                    .collect::<Vec<_>>()
            } else {
                resp.files
                    .into_iter()
                    .map(|f| {
                        json!({
                            "name": f.name,
                            "key": f.key,
                            "last_modified": f.last_modified,
                        })
                    })
                    .collect::<Vec<_>>()
            };

            Ok::<_, anyhow::Error>(json!({
                "project_name": resp.name,
                "project_id": pid,
                "files": file_jsons,
            }))
        });
        let projects: Vec<serde_json::Value> = try_join_all(fetches).await?;
        print(&json!({ "projects": projects }), format)
    }
}

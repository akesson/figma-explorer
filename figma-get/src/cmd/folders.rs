//! Folder endpoints (`/v2/folders`, `/v2/teams/{id}/folders`). Figma renamed
//! projects to folders in August 2026; these replace the deprecated
//! `/v1/projects` and `/v1/teams/{id}/projects` endpoints and are the only
//! ones a personal access token created after 2026-08-03 (`folders:read`,
//! no `projects:read`) can call. Folder ids are the old project ids.

use clap::Args;
use figma_api::apis::configuration::Configuration;
use figma_api::apis::folders_api as api;

use crate::finalize;

#[derive(Args, Debug)]
pub struct FolderFilesArgs {
    /// Folder ID (same numeric id as the former project id).
    #[arg(long, visible_alias = "project-id")]
    pub folder_id: String,
    /// Include branch metadata for files with branches.
    #[arg(long)]
    pub branch_data: Option<bool>,
}

impl FolderFilesArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetFolderFilesParams {
            folder_id: self.folder_id,
            branch_data: self.branch_data,
        };
        finalize(api::get_folder_files(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct FolderFoldersArgs {
    /// Parent folder ID.
    #[arg(long)]
    pub folder_id: String,
}

impl FolderFoldersArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetFolderFoldersParams {
            folder_id: self.folder_id,
        };
        finalize(api::get_folder_folders(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct FolderMetaArgs {
    /// Folder ID.
    #[arg(long)]
    pub folder_id: String,
}

impl FolderMetaArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetFolderMetaParams {
            folder_id: self.folder_id,
        };
        finalize(api::get_folder_meta(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct TeamFoldersArgs {
    /// Team ID.
    #[arg(long, env = "FIGMA_TEAM_ID")]
    pub team_id: String,
}

impl TeamFoldersArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetTeamFoldersParams {
            team_id: self.team_id,
        };
        finalize(api::get_team_folders(cfg, params).await)
    }
}

use clap::Args;
use figma_api::apis::configuration::Configuration;
use figma_api::apis::library_analytics_api as api;

use crate::finalize;

#[derive(Args, Debug)]
pub struct ComponentActionsArgs {
    /// Library file key.
    #[arg(long)]
    pub file_key: String,
    /// Group-by dimension (e.g. "component", "team").
    #[arg(long)]
    pub group_by: String,
    /// Pagination cursor from a previous response.
    #[arg(long)]
    pub cursor: Option<String>,
    /// ISO 8601 date (YYYY-MM-DD) of the earliest week to include.
    #[arg(long)]
    pub start_date: Option<String>,
    /// ISO 8601 date (YYYY-MM-DD) of the latest week to include.
    #[arg(long)]
    pub end_date: Option<String>,
}

impl ComponentActionsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetLibraryAnalyticsComponentActionsParams {
            file_key: self.file_key,
            group_by: self.group_by,
            cursor: self.cursor,
            start_date: self.start_date,
            end_date: self.end_date,
        };
        finalize(api::get_library_analytics_component_actions(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct ComponentUsagesArgs {
    /// Library file key.
    #[arg(long)]
    pub file_key: String,
    /// Group-by dimension.
    #[arg(long)]
    pub group_by: String,
    /// Pagination cursor.
    #[arg(long)]
    pub cursor: Option<String>,
}

impl ComponentUsagesArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetLibraryAnalyticsComponentUsagesParams {
            file_key: self.file_key,
            group_by: self.group_by,
            cursor: self.cursor,
        };
        finalize(api::get_library_analytics_component_usages(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct StyleActionsArgs {
    /// Library file key.
    #[arg(long)]
    pub file_key: String,
    /// Group-by dimension.
    #[arg(long)]
    pub group_by: String,
    /// Pagination cursor.
    #[arg(long)]
    pub cursor: Option<String>,
    /// ISO 8601 date (YYYY-MM-DD) of the earliest week to include.
    #[arg(long)]
    pub start_date: Option<String>,
    /// ISO 8601 date (YYYY-MM-DD) of the latest week to include.
    #[arg(long)]
    pub end_date: Option<String>,
}

impl StyleActionsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetLibraryAnalyticsStyleActionsParams {
            file_key: self.file_key,
            group_by: self.group_by,
            cursor: self.cursor,
            start_date: self.start_date,
            end_date: self.end_date,
        };
        finalize(api::get_library_analytics_style_actions(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct StyleUsagesArgs {
    /// Library file key.
    #[arg(long)]
    pub file_key: String,
    /// Group-by dimension.
    #[arg(long)]
    pub group_by: String,
    /// Pagination cursor.
    #[arg(long)]
    pub cursor: Option<String>,
}

impl StyleUsagesArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetLibraryAnalyticsStyleUsagesParams {
            file_key: self.file_key,
            group_by: self.group_by,
            cursor: self.cursor,
        };
        finalize(api::get_library_analytics_style_usages(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct VariableActionsArgs {
    /// Library file key.
    #[arg(long)]
    pub file_key: String,
    /// Group-by dimension.
    #[arg(long)]
    pub group_by: String,
    /// Pagination cursor.
    #[arg(long)]
    pub cursor: Option<String>,
    /// ISO 8601 date (YYYY-MM-DD) of the earliest week to include.
    #[arg(long)]
    pub start_date: Option<String>,
    /// ISO 8601 date (YYYY-MM-DD) of the latest week to include.
    #[arg(long)]
    pub end_date: Option<String>,
}

impl VariableActionsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetLibraryAnalyticsVariableActionsParams {
            file_key: self.file_key,
            group_by: self.group_by,
            cursor: self.cursor,
            start_date: self.start_date,
            end_date: self.end_date,
        };
        finalize(api::get_library_analytics_variable_actions(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct VariableUsagesArgs {
    /// Library file key.
    #[arg(long)]
    pub file_key: String,
    /// Group-by dimension.
    #[arg(long)]
    pub group_by: String,
    /// Pagination cursor.
    #[arg(long)]
    pub cursor: Option<String>,
}

impl VariableUsagesArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetLibraryAnalyticsVariableUsagesParams {
            file_key: self.file_key,
            group_by: self.group_by,
            cursor: self.cursor,
        };
        finalize(api::get_library_analytics_variable_usages(cfg, params).await)
    }
}

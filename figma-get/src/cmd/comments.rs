use clap::Args;
use figma_api::apis::comments_api as api;
use figma_api::apis::configuration::Configuration;

use crate::finalize;

#[derive(Args, Debug)]
pub struct CommentsArgs {
    /// File key (or branch key) to get comments from.
    #[arg(long)]
    pub file_key: String,
    /// Return comments as markdown when applicable.
    #[arg(long)]
    pub as_md: Option<bool>,
}

impl CommentsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetCommentsParams {
            file_key: self.file_key,
            as_md: self.as_md,
        };
        finalize(api::get_comments(cfg, params).await)
    }
}

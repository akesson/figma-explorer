use clap::Args;
use figma_api::apis::comment_reactions_api as api;
use figma_api::apis::configuration::Configuration;

use crate::finalize;

#[derive(Args, Debug)]
pub struct CommentReactionsArgs {
    /// File key (or branch key) the comment lives in.
    #[arg(long)]
    pub file_key: String,
    /// Comment ID.
    #[arg(long)]
    pub comment_id: String,
    /// Pagination cursor from a previous response.
    #[arg(long)]
    pub cursor: Option<String>,
}

impl CommentReactionsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetCommentReactionsParams {
            file_key: self.file_key,
            comment_id: self.comment_id,
            cursor: self.cursor,
        };
        finalize(api::get_comment_reactions(cfg, params).await)
    }
}

use clap::Args;
use figma_api::apis::configuration::Configuration;
use figma_api::apis::payments_api as api;

use crate::finalize;

#[derive(Args, Debug)]
pub struct PaymentsArgs {
    /// Short-lived token from `getPluginPaymentTokenAsync` (plugin payments API).
    #[arg(long)]
    pub plugin_payment_token: Option<String>,
    /// User ID to query payment info for.
    #[arg(long)]
    pub user_id: Option<String>,
    /// Community file ID. Provide exactly one of --community-file-id, --plugin-id, --widget-id.
    #[arg(long)]
    pub community_file_id: Option<String>,
    /// Plugin ID. Provide exactly one of --community-file-id, --plugin-id, --widget-id.
    #[arg(long)]
    pub plugin_id: Option<String>,
    /// Widget ID. Provide exactly one of --community-file-id, --plugin-id, --widget-id.
    #[arg(long)]
    pub widget_id: Option<String>,
}

impl PaymentsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetPaymentsParams {
            plugin_payment_token: self.plugin_payment_token,
            user_id: self.user_id,
            community_file_id: self.community_file_id,
            plugin_id: self.plugin_id,
            widget_id: self.widget_id,
        };
        finalize(api::get_payments(cfg, params).await)
    }
}

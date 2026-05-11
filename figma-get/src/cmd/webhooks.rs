use clap::Args;
use figma_api::apis::configuration::Configuration;
use figma_api::apis::webhooks_api as api;

use crate::finalize;

#[derive(Args, Debug)]
pub struct TeamWebhooksArgs {
    /// Team ID.
    #[arg(long)]
    pub team_id: String,
}

impl TeamWebhooksArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetTeamWebhooksParams {
            team_id: self.team_id,
        };
        finalize(api::get_team_webhooks(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct WebhookArgs {
    /// Webhook ID.
    #[arg(long)]
    pub webhook_id: String,
}

impl WebhookArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetWebhookParams {
            webhook_id: self.webhook_id,
        };
        finalize(api::get_webhook(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct WebhookRequestsArgs {
    /// Webhook subscription ID.
    #[arg(long)]
    pub webhook_id: String,
}

impl WebhookRequestsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetWebhookRequestsParams {
            webhook_id: self.webhook_id,
        };
        finalize(api::get_webhook_requests(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct WebhooksArgs {
    /// Context: "team", "project", or "file".
    #[arg(long)]
    pub context: Option<String>,
    /// ID of the context. Cannot be combined with --plan-api-id.
    #[arg(long)]
    pub context_id: Option<String>,
    /// Plan API ID. Cannot be combined with --context / --context-id. Paginates the response.
    #[arg(long)]
    pub plan_api_id: Option<String>,
    /// Pagination cursor (only when --plan-api-id is set).
    #[arg(long)]
    pub cursor: Option<String>,
}

impl WebhooksArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetWebhooksParams {
            context: self.context,
            context_id: self.context_id,
            plan_api_id: self.plan_api_id,
            cursor: self.cursor,
        };
        finalize(api::get_webhooks(cfg, params).await)
    }
}

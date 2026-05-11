use clap::Args;
use figma_api::apis::activity_logs_api as api;
use figma_api::apis::configuration::Configuration;

use crate::finalize;

#[derive(Args, Debug)]
pub struct ActivityLogsArgs {
    /// Comma-separated event type(s) to include. All events by default.
    #[arg(long)]
    pub events: Option<String>,
    /// Unix timestamp of the earliest event. Defaults to one year ago.
    #[arg(long)]
    pub start_time: Option<f64>,
    /// Unix timestamp of the latest event. Defaults to now.
    #[arg(long)]
    pub end_time: Option<f64>,
    /// Maximum number of events to return. Defaults to 1000.
    #[arg(long)]
    pub limit: Option<f64>,
    /// "asc" (default) or "desc".
    #[arg(long)]
    pub order: Option<String>,
}

impl ActivityLogsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetActivityLogsParams {
            events: self.events,
            start_time: self.start_time,
            end_time: self.end_time,
            limit: self.limit,
            order: self.order,
        };
        finalize(api::get_activity_logs(cfg, params).await)
    }
}

use figma_api::apis::configuration::Configuration;
use figma_api::apis::users_api as api;

use crate::finalize;

pub async fn run_me(cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
    finalize(api::get_me(cfg).await)
}

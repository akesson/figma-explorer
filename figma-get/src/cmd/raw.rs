use anyhow::{anyhow, Context};
use clap::Args;
use figma_api::apis::configuration::Configuration;

#[derive(Args, Debug)]
pub struct RawArgs {
    /// URL path with optional query string. Leading slash is optional.
    /// Examples: `/v1/me`, `/v1/files/ABC/meta`, `/v1/files/ABC?depth=1`.
    pub path: String,
}

impl RawArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let path = if self.path.starts_with('/') {
            self.path
        } else {
            format!("/{}", self.path)
        };
        let url = format!("{}{}", cfg.base_path, path);

        let mut req = cfg.client.get(&url);
        if let Some(ua) = &cfg.user_agent {
            req = req.header("user-agent", ua.clone());
        }
        if let Some(token) = &cfg.oauth_access_token {
            req = req.bearer_auth(token);
        }
        if let Some(apikey) = &cfg.api_key {
            let value = match &apikey.prefix {
                Some(prefix) => format!("{} {}", prefix, apikey.key),
                None => apikey.key.clone(),
            };
            req = req.header("X-Figma-Token", value);
        }

        let resp = req.send().await.context("HTTP request failed")?;
        let status = resp.status();
        let body = resp.text().await.context("reading response body")?;

        if !status.is_success() {
            return Err(anyhow!("figma API error ({}): {}", status, body));
        }

        match serde_json::from_str(&body) {
            Ok(v) => Ok(v),
            Err(_) => Ok(serde_json::Value::String(body)),
        }
    }
}

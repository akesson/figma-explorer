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

        // Shared auth + transport; `raw` keeps a tolerant body policy (return
        // non-JSON responses verbatim as a string) rather than erroring.
        let body = figma_common::get_text(cfg, &url).await?;
        match serde_json::from_str(&body) {
            Ok(v) => Ok(v),
            Err(_) => Ok(serde_json::Value::String(body)),
        }
    }
}

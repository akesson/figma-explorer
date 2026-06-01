//! Shared helpers for the figma-explorer workspace, factored out so the two
//! binaries (`figma-explorer`, `figma-get`) don't each carry their own copy:
//! `.env` discovery, an authenticated Figma HTTP GET, and the one stable-hash
//! choice used for persisted fingerprints.

use anyhow::{anyhow, Context, Result};
use figma_api::apis::configuration::Configuration;

/// The hasher used for any fingerprint that is written to disk and compared
/// across runs. `std::collections::hash_map::DefaultHasher` is explicitly not
/// stable across Rust toolchain versions; xxHash64 is a fixed algorithm, so a
/// persisted fingerprint stays valid after a `rustup update`.
pub type StableHasher = twox_hash::XxHash64;

/// Walk from cwd up to the filesystem root, loading every `.env` we find.
/// `dotenvy::from_path` does not override already-set vars, so the closest
/// `.env` wins and ancestors fill in any keys it didn't define.
pub fn load_envs() {
    let Ok(start) = std::env::current_dir() else {
        return;
    };
    let mut dir = start.as_path();
    loop {
        let candidate = dir.join(".env");
        if candidate.is_file() {
            let _ = dotenvy::from_path(&candidate);
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => break,
        }
    }
}

/// Issue an authenticated GET against the Figma REST API and return the raw
/// response body as text. Applies the configuration's auth headers
/// (`X-Figma-Token` / bearer / user-agent) and turns any non-success status
/// into an error carrying the body. Callers decide how to parse the body
/// (strict JSON, or parse-or-passthrough).
pub async fn get_text(cfg: &Configuration, url: &str) -> Result<String> {
    let mut req = cfg.client.get(url);
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
        return Err(anyhow!("figma API error ({status}): {body}"));
    }
    Ok(body)
}

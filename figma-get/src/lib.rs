use std::fmt::Debug;

use anyhow::{anyhow, Context};
use figma_api::apis::configuration::{ApiKey, Configuration};
use figma_api::apis::Error as ApiError;
use serde::Serialize;

pub mod cmd;

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum Output {
    #[default]
    Yaml,
    Json,
}

pub fn build_config(token: Option<&str>) -> anyhow::Result<Configuration> {
    let key = match token {
        Some(t) => t.to_owned(),
        None => std::env::var("FIGMA_TOKEN").map_err(|_| {
            anyhow!(
                "FIGMA_TOKEN not set. Export it (https://www.figma.com/developers/api#access-tokens) \
                 or pass --token."
            )
        })?,
    };
    let mut cfg = Configuration::new();
    cfg.api_key = Some(ApiKey { prefix: None, key });
    Ok(cfg)
}

pub fn into_value<T: Serialize>(t: T) -> anyhow::Result<serde_json::Value> {
    serde_json::to_value(t).context("converting response to JSON value")
}

pub fn finalize<T: Serialize, E: Debug>(
    result: Result<T, ApiError<E>>,
) -> anyhow::Result<serde_json::Value> {
    let value = result.map_err(into_anyhow)?;
    into_value(value)
}

pub fn into_anyhow<E: Debug>(e: ApiError<E>) -> anyhow::Error {
    match e {
        ApiError::ResponseError(r) => {
            anyhow!("figma API error ({}): {}", r.status, r.content)
        }
        ApiError::Reqwest(e) => anyhow::Error::new(e).context("HTTP request failed"),
        ApiError::Serde(e) => anyhow::Error::new(e).context("decoding response body"),
        ApiError::Io(e) => anyhow::Error::new(e).context("I/O error"),
    }
}

pub fn print(value: &serde_json::Value, output: Output) -> anyhow::Result<()> {
    match output {
        Output::Yaml => {
            let s = serde_yaml_ng::to_string(value).context("serializing to YAML")?;
            print!("{s}");
            if !s.ends_with('\n') {
                println!();
            }
        }
        Output::Json => {
            let s = serde_json::to_string_pretty(value).context("serializing to JSON")?;
            println!("{s}");
        }
    }
    Ok(())
}

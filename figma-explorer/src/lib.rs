use std::fmt::Debug;

use anyhow::{anyhow, Context};
use figma_api::apis::configuration::{ApiKey, Configuration};
use figma_api::apis::Error as ApiError;
use serde::Serialize;

pub mod assets;
pub mod cache;
pub mod cmd;
pub mod comment_assoc;
pub mod comment_view;
pub mod context;
pub mod file_summary;
pub mod full_cache;
pub mod geometry;
pub mod id;
pub mod node;
pub mod node_index;
pub mod node_search;
pub mod node_view;
pub mod resolver;
pub mod screenshot;
pub mod styles;
pub mod synth;
pub mod team_catalog;
pub mod tree;
pub mod url;
pub mod util;

/// Hard recursion backstop for walks over a Figma node tree. The tree's depth
/// is driven by untrusted remote data, and unbounded recursion can overflow
/// the stack (an uncatchable abort in Rust). This is purely a safety ceiling:
/// real files nest only a few dozen levels, so it never affects normal output;
/// it sits far below the ~thousands of frames needed to overflow. Distinct from
/// `node_view`'s display-depth default (10), which is a presentation choice.
/// The single untrusted-`Value` → `CacheNode` ingestion point (`cache::project_to_cache`)
/// enforces this, so every downstream `CacheNode` walk is bounded transitively.
pub const MAX_NODE_DEPTH: usize = 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Output {
    /// Compact YAML (default): node-list commands emit a sequence of one-liner
    /// strings; `tree` emits a nested structured tree; non-list commands emit
    /// their current short structured YAML. No metadata headers.
    #[default]
    Yaml,
    /// Full pretty JSON with all metadata headers — for jq-style pipelines.
    Json,
}

/// Process-wide flags assembled from the global CLI args. Threaded through
/// every command so behavior (output format, cache-only mode, bare-id scope)
/// stays consistent regardless of which subcommand consumes them.
#[derive(Clone, Debug)]
pub struct Globals {
    pub output: Output,
    /// `--cache-only`: refuse to fall through to a live API fetch. Commands
    /// should construct their `Resolver` with this flag forwarded.
    pub cache_only: bool,
    /// `--in <ID>`: scope override for the bare-id form, used by `ls` (to
    /// disambiguate the few intrinsically-ambiguous ids like `0:0`) and
    /// `find` (to restrict the search). Other commands ignore it.
    pub scope: Option<String>,
}

pub fn build_config(token: Option<&str>) -> anyhow::Result<Configuration> {
    let key = match token {
        Some(t) => t.to_owned(),
        None => std::env::var("FIGMA_TOKEN").map_err(|_| {
            anyhow!(
                "FIGMA_TOKEN not set. Export it (https://www.figma.com/developers/api#access-tokens), \
                 pass --token, or put it in a `.env` — the nearest one walking up from cwd wins, \
                 with ~/.config/figma-explorer/.env as the global fallback."
            )
        })?,
    };
    let mut cfg = Configuration::new();
    cfg.client = figma_common::http_client().context("building HTTP client")?;
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

use clap::Parser;
use figma_explorer::{build_config, cmd::Command, Globals, Output};

/// High-level CLI on top of the Figma REST API.
///
/// Designed for design-to-code workflows: navigate by tagged IDs, render
/// compact tree listings, export screenshots, extract assets and design
/// tokens.
#[derive(Parser, Debug)]
#[command(name = "figma-explorer", version, about, long_about = None)]
struct Cli {
    /// Emit full pretty JSON instead of the default compact YAML.
    #[arg(long, global = true)]
    json: bool,

    /// Personal access token. Falls back to the FIGMA_TOKEN environment variable.
    #[arg(long, env = "FIGMA_TOKEN", global = true, hide_env_values = true)]
    token: Option<String>,

    /// Refuse to fall through to live API fetches; error if a request can't be
    /// served from the local cache.
    #[arg(long, global = true)]
    cache_only: bool,

    /// Scope the bare-id form to a specific file or subtree. Used to break
    /// ambiguity for common ids like `0:0`, and by `find` to restrict the
    /// search. Accepts any tagged id (e.g. `file:2`, `file:2:1094:66591`).
    // `id = "scope_in"` namespaces this flag in clap so it doesn't collide
    // with subcommand-local fields named `scope` (e.g. `tokens --scope`).
    #[arg(long = "in", id = "scope_in", value_name = "ID", global = true)]
    scope: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    let cfg = build_config(cli.token.as_deref())?;
    let globals = Globals {
        output: if cli.json { Output::Json } else { Output::Yaml },
        cache_only: cli.cache_only,
        scope: cli.scope,
    };
    cli.command.run(&cfg, &globals).await
}

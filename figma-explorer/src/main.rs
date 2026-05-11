use clap::Parser;
use figma_explorer::{build_config, cmd::Command, Output};

/// High-level CLI on top of the Figma REST API.
///
/// Designed for design-to-code workflows: navigate files by name, render
/// compact trees, export screenshots, extract assets and design tokens.
#[derive(Parser, Debug)]
#[command(name = "figma-explorer", version, about, long_about = None)]
struct Cli {
    /// Emit full pretty JSON instead of the default compact YAML.
    #[arg(long, global = true)]
    json: bool,

    /// Personal access token. Falls back to the FIGMA_TOKEN environment variable.
    #[arg(long, env = "FIGMA_TOKEN", global = true, hide_env_values = true)]
    token: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    let cfg = build_config(cli.token.as_deref())?;
    let output = if cli.json { Output::Json } else { Output::Yaml };
    cli.command.run(&cfg, output).await
}

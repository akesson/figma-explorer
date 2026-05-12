use clap::Parser;
use figma_get::{build_config, cmd::Command, print, Output};

/// Read-only CLI for the Figma REST API.
#[derive(Parser, Debug)]
#[command(name = "figma-get", version, about, long_about = None)]
struct Cli {
    /// Output format.
    #[arg(long, value_enum, default_value_t = Output::default(), global = true)]
    format: Output,

    /// Personal access token. Falls back to the FIGMA_TOKEN environment variable.
    #[arg(long, env = "FIGMA_TOKEN", global = true, hide_env_values = true)]
    token: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_envs();
    let cli = Cli::parse();
    let cfg = build_config(cli.token.as_deref())?;
    let value = cli.command.run(&cfg).await?;
    print(&value, cli.format)
}

/// Walk from cwd up to the filesystem root, loading every `.env` we find.
/// `dotenvy::from_path` does not override already-set vars, so the closest
/// `.env` wins and ancestors fill in any keys it didn't define.
fn load_envs() {
    let Ok(start) = std::env::current_dir() else { return };
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

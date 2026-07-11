#![cfg_attr(not(test), deny(clippy::unwrap_used))]

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "kirkforge", about = "Instruction-driven video production")]
struct Cli {
    #[command(subcommand)]
    cmd: kirkforge_video::Cmd,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    kirkforge_video::run(cli.cmd).await
}

mod cli;
mod config;
mod core;
mod interactive_mode;
mod native_env;
mod tui;

use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Set up file logging to ~/.flown/logs/
    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".flown")
        .join("logs");
    std::fs::create_dir_all(&log_dir)?;

    let file_appender = tracing_appender::rolling::daily(&log_dir, "flown.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();

    // Keep guard alive for the duration of the program
    let _guard = _guard;

    let cli = cli::Cli::parse();

    let model_override = cli.model;
    let provider_override = cli.provider;
    let _verbose = cli.verbose;

    match cli
        .command
        .unwrap_or(cli::Commands::Chat { prompt: vec![] })
    {
        cli::Commands::Chat { prompt } => {
            cli::cmd_chat(model_override, provider_override, prompt).await
        }
        cli::Commands::Config { action } => cli::cmd_config(action),
        cli::Commands::Mcp { action } => cli::cmd_mcp(action).await,
        cli::Commands::Completions { shell } => cli::cmd_completions(shell),
    }
}

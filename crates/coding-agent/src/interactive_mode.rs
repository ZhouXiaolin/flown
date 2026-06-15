//! TUI entry point.
//!
//! Delegates to [`crate::tui::runtime::run_tui`], which builds the iodilos
//! renderer + the cross-runtime flume bridge. This file is kept thin so
//! `cli::cmd_chat` calls into the same place regardless of the TUI backend.

use crate::config::Config;

pub async fn run_tui(
    config: Config,
    model_str: String,
    provider_name: String,
    api_key: Option<String>,
    initial_prompt: Option<String>,
) -> anyhow::Result<()> {
    crate::tui::runtime::run_tui(config, model_str, provider_name, api_key, initial_prompt).await
}

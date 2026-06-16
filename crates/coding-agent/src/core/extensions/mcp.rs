//! [`McpExtension`] — the `/mcp` command plus MCP tools (M2a's single extension).
//!
//! The command side (`/mcp list|status|help`) is read-only over config and
//! returns [`CommandEffect::Notify`]. The tool side snapshots the current MCP
//! tools at registration and takes a [`ToolHandle`] for runtime add/remove as
//! MCP servers connect/disconnect.
//!
//! MCP tool construction (`mcp_manager_tools`, moved here from `core/tools`)
//! is an implementation detail of this extension — it wraps each MCP server
//! tool as an `AgentTool` that delegates `call_tool` to the `McpManager`.

use std::sync::Arc;

use flown_agent::harness::env::types::ExecutionEnv;
use flown_agent::types::{AgentTool, AgentToolError, AgentToolResult, ToolExecutionMode};
use serde_json::Value;

use crate::config::Config;
use crate::core::mcp::McpManager;

use super::types::{CommandEffect, CommandMeta, Extension, ExtensionApi, SubcommandDef, ToolHandle};

/// The MCP extension: `/mcp` command + MCP tools (runtime add/remove).
pub struct McpExtension {
    config: Config,
    mcp: Option<Arc<tokio::sync::Mutex<McpManager>>>,
}

impl McpExtension {
    pub fn new(config: Config, mcp: Option<Arc<tokio::sync::Mutex<McpManager>>>) -> Self {
        Self { config, mcp }
    }
}

impl Extension for McpExtension {
    fn name(&self) -> &'static str {
        "mcp"
    }

    fn register(&self, api: &mut ExtensionApi) {
        self.register_mcp_command(api);
        self.register_mcp_tools(api);
    }
}

impl McpExtension {
    fn register_mcp_command(&self, api: &mut ExtensionApi) {
        let config = self.config.clone();
        api.register_command(
            "/mcp",
            CommandMeta {
                description: "Manage MCP servers".into(),
                aliases: Vec::new(),
                subcommands: vec![
                    SubcommandDef {
                        name: "list".into(),
                        description: "List configured servers".into(),
                    },
                    SubcommandDef {
                        name: "status".into(),
                        description: "Show connection state".into(),
                    },
                    SubcommandDef {
                        name: "help".into(),
                        description: "Show MCP help".into(),
                    },
                ],
            },
            {
                let config = config.clone();
                // The handler closure is `Send + Sync`: it captures only the
                // `Config` (Clone, owned) and returns a plain `CommandEffect`.
                // It never touches `UiState`; the iodilos side interprets the
                // returned effect.
                Arc::new(move |args: &str| -> CommandEffect {
                    handle_mcp_subcommand(args, &config)
                })
            },
        );
    }

    fn register_mcp_tools(&self, api: &mut ExtensionApi) {
        let Some(mcp) = self.mcp.clone() else {
            return;
        };
        // Snapshot the current MCP tool set once at registration. Tools added
        // later (a server connecting) are pushed via the ToolHandle.
        for tool in mcp_manager_tools(mcp) {
            api.register_tool(tool);
        }
        // Take a persistent handle for runtime add/remove. A caller-side
        // watcher task clones it to push servers coming online later — see
        // decision A1' in docs/m2a-extension-api-draft.md.
        let _handle: ToolHandle = api.tool_handle();
        // NOTE: dropped intentionally. The one-shot registration above covers
        // M2a's bootstrap (tools known at startup). Runtime add/remove is
        // exercised by a wiring-layer watcher that takes its own handle.
    }
}

/// Convert the `McpManager`'s tools into `AgentTool` instances.
///
/// Moved here from `core/tools/mod.rs` so MCP tool construction is co-located
/// with its owning extension. Each tool delegates `call_tool` to the manager
/// (async); tool *infos* are read synchronously via `try_lock`.
fn mcp_manager_tools(mcp: Arc<tokio::sync::Mutex<McpManager>>) -> Vec<AgentTool> {
    // Tool infos synchronously for registration; call_tool is async at runtime.
    let tool_infos = {
        let manager = match mcp.try_lock() {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };
        manager.tool_infos()
    };

    tool_infos
        .into_iter()
        .map(|info| {
            let mcp = mcp.clone();
            let tool_name = info.name.clone();
            let description = info.description.clone();
            let parameters = info.input_schema.clone();

            AgentTool {
                name: info.name,
                label: info.label,
                description,
                parameters,
                execute: Arc::new(move |_id, args, _abort, _update| {
                    let mcp = mcp.clone();
                    let tool_name = tool_name.clone();
                    Box::pin(async move {
                        let result = {
                            let mut manager = mcp.lock().await;
                            manager.call_tool(&tool_name, args).await
                        };

                        match result {
                            Ok(output) => Ok(AgentToolResult {
                                content: vec![flown_ai::types::ToolResultContent::Text(
                                    flown_ai::types::TextContent {
                                        content_type: "text".to_string(),
                                        text: output,
                                        text_signature: None,
                                    },
                                )],
                                details: Value::Null,
                                terminate: None,
                            }),
                            Err(e) => Err(AgentToolError::new(format!("MCP error: {e}"))),
                        }
                    })
                }),
                prepare_arguments: None,
                execution_mode: Some(ToolExecutionMode::Sequential),
            }
        })
        .collect()
}

/// Mirror of the prior `handle_mcp` logic in `tui/slash_commands.rs`, but
/// returning a [`CommandEffect`] instead of pushing to a transcript handle.
fn handle_mcp_subcommand(args: &str, config: &Config) -> CommandEffect {
    match args.trim() {
        "" | "help" => CommandEffect::Notify(mcp_help_text()),
        "list" => CommandEffect::Notify(mcp_list_text(config)),
        "status" => CommandEffect::Notify(mcp_status_text(config)),
        other => CommandEffect::NotifyError(format!(
            "Unknown /mcp subcommand: {other}. Type /mcp help for usage."
        )),
    }
}

fn mcp_help_text() -> String {
    let mut lines = vec!["MCP server management:".to_string()];
    for (name, desc) in [
        ("list", "List configured servers"),
        ("status", "Show connection state"),
        ("help", "Show MCP help"),
    ] {
        lines.push(format!("  /mcp {name:<10} {desc}"));
    }
    lines.join("\n")
}

fn mcp_list_text(config: &Config) -> String {
    if config.mcp_servers.is_empty() {
        return "No MCP servers configured.".into();
    }
    let mut lines = vec!["MCP Servers:".to_string()];
    for (name, server) in &config.mcp_servers {
        let status = if server.disabled {
            "disabled"
        } else {
            "enabled"
        };
        let full_cmd = if server.args.is_empty() {
            server.command.clone()
        } else {
            format!("{} {}", server.command, server.args.join(" "))
        };
        lines.push(format!("  {name}  - {full_cmd} ({status})"));
    }
    lines.push(String::new());
    lines.push("Use /mcp status to check connection state.".into());
    lines.join("\n")
}

fn mcp_status_text(config: &Config) -> String {
    if config.mcp_servers.is_empty() {
        return "No MCP servers configured.".into();
    }
    let mut lines = vec!["MCP Servers (configured):".to_string()];
    for (name, server) in &config.mcp_servers {
        let icon = if server.disabled { "x" } else { "*" };
        let full_cmd = if server.args.is_empty() {
            server.command.clone()
        } else {
            format!("{} {}", server.command, server.args.join(" "))
        };
        lines.push(format!("  {icon} {name}  - {full_cmd}"));
    }
    lines.push(String::new());
    lines.push(
        "Note: /mcp status shows config state. Run `flown mcp status` for live connection info."
            .into(),
    );
    lines.join("\n")
}

// `ExecutionEnv` is re-exported here so downstream callers that used to reach
// it via `core::tools` still resolve. (No MCP code uses it; kept for stability
// of the module's public surface during the move.)
#[allow(unused_imports)]
use flown_agent::harness::env::types::ExecutionEnv as _ExecutionEnv;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn empty_config() -> Config {
        Config::default()
    }

    #[test]
    fn mcp_help_subcommand() {
        let cfg = empty_config();
        let effect = handle_mcp_subcommand("help", &cfg);
        let CommandEffect::Notify(text) = effect else {
            panic!("expected Notify");
        };
        assert!(text.contains("/mcp list"));
        assert!(text.contains("/mcp status"));
    }

    #[test]
    fn mcp_list_no_servers() {
        let cfg = empty_config();
        let effect = handle_mcp_subcommand("list", &cfg);
        let CommandEffect::Notify(text) = effect else {
            panic!("expected Notify");
        };
        assert_eq!(text, "No MCP servers configured.");
    }

    #[test]
    fn mcp_unknown_subcommand_is_error() {
        let cfg = empty_config();
        let effect = handle_mcp_subcommand("frobnicate", &cfg);
        assert!(matches!(effect, CommandEffect::NotifyError(_)));
    }

    #[test]
    fn mcp_bare_invocation_shows_help() {
        let cfg = empty_config();
        let effect = handle_mcp_subcommand("", &cfg);
        assert!(matches!(effect, CommandEffect::Notify(_)));
    }
}

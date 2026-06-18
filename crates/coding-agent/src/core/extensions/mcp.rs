//! [`McpExtension`] — the `/mcp` command plus MCP tools (M2a's single extension).
//!
//! The command side (`/mcp list|status|help`) is read-only: `list` echoes
//! config, `status` reports live connection state from the `McpManager` when
//! available (falling back to config otherwise), and writes output through the
//! async command's `ExtensionContext`.
//!
//! MCP tool construction (`mcp_manager_tools`, moved here from `core/tools`)
//! is an implementation detail of this extension — it wraps each MCP server
//! tool as an `AgentTool` that delegates `call_tool` to the `McpManager`.

use std::sync::Arc;

use flown_agent::{AgentTool, AgentToolError, AgentToolResult, ToolExecutionMode};
use serde_json::Value;

use crate::config::Config;
use crate::core::mcp::McpManager;

use super::types::{CommandMeta, Extension, ExtensionApi, SubcommandDef};

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
                let mcp = self.mcp.clone();
                Arc::new(move |invocation, ctx| {
                    let config = config.clone();
                    let mcp = mcp.clone();
                    Box::pin(async move {
                        match mcp_subcommand_text(&invocation.args, &config, mcp.as_ref()) {
                            Ok(text) => ctx.ui.notify(text),
                            Err(text) => ctx.ui.notify_error(text),
                        }
                        Ok(())
                    })
                })
            },
        );
    }

    fn register_mcp_tools(&self, api: &mut ExtensionApi) {
        let Some(mcp) = self.mcp.clone() else {
            return;
        };
        // Snapshot the current MCP tool set once at registration. Tools added
        // later (a server connecting) would be pushed via a ToolHandle, but no
        // runtime watcher exists yet — when one is needed it will mint its own
        // handle. Until then the one-shot registration below is the whole
        // story; do not take-and-drop a handle here (that read as a TODO).
        for tool in mcp_manager_tools(mcp) {
            api.register_tool(tool);
        }
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
                                content: vec![flown_ai::ToolResultContent::Text(
                                    flown_ai::TextContent {
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

/// Build `/mcp <subcommand>` output. When a live `McpManager` is available,
/// `/mcp status` reports actual connection state (connected/error/disconnected
/// + tool count); without one it falls back to config-only (disabled/enabled).
fn mcp_subcommand_text(
    args: &str,
    config: &Config,
    mcp: Option<&Arc<tokio::sync::Mutex<McpManager>>>,
) -> Result<String, String> {
    match args.trim() {
        "" | "help" => Ok(mcp_help_text()),
        "list" => Ok(mcp_list_text(config)),
        "status" => Ok(mcp_status_text(config, mcp)),
        other => Err(format!(
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

/// `/mcp status` text. When a live `McpManager` is present and its lock is
/// acquirable without blocking (try_lock), reports real connection state per
/// server: connected (with tool count), error (with message), or disconnected.
/// Otherwise falls back to a config-only view (enabled/disabled) so the command
/// is never empty.
fn mcp_status_text(config: &Config, mcp: Option<&Arc<tokio::sync::Mutex<McpManager>>>) -> String {
    if config.mcp_servers.is_empty() {
        return "No MCP servers configured.".into();
    }

    // Live view: only when the manager exists AND we can lock it without
    // blocking. The handler runs on the iodilos thread and must not .await,
    // so a contended lock degrades to the config fallback rather than stalling.
    if let Some(mcp) = mcp
        && let Ok(manager) = mcp.try_lock()
    {
        let infos = manager.server_info();
        let mut lines = vec!["MCP Servers (live):".to_string()];
        use crate::core::types::McpServerStatus;
        for info in &infos {
            let icon = match info.status {
                McpServerStatus::Connected => "✓",
                McpServerStatus::Error => "✗",
                McpServerStatus::Disconnected => "○",
            };
            let suffix = match (&info.status, info.tool_count, info.error.as_deref()) {
                (McpServerStatus::Connected, n, _) => format!("({n} tools)"),
                (McpServerStatus::Error, _, Some(e)) => format!("error: {e}"),
                (McpServerStatus::Error, _, None) => "error".to_string(),
                (McpServerStatus::Disconnected, _, _) => "disconnected".to_string(),
            };
            let full_cmd = if info.args.is_empty() {
                info.command.clone()
            } else {
                format!("{} {}", info.command, info.args.join(" "))
            };
            lines.push(format!("  {icon} {}  - {full_cmd} [{suffix}]", info.name));
        }
        return lines.join("\n");
    }

    // Config fallback.
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
        "Note: live state unavailable here. Run `flown mcp status` for connection info.".into(),
    );
    lines.join("\n")
}

// `ExecutionEnv` is re-exported here so downstream callers that used to reach
// it via `core::tools` still resolve. (No MCP code uses it; kept for stability
// of the module's public surface during the move.)
#[allow(unused_imports)]
use flown_agent::ExecutionEnv as _ExecutionEnv;

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
        let text = mcp_subcommand_text("help", &cfg, None).expect("expected text");
        assert!(text.contains("/mcp list"));
        assert!(text.contains("/mcp status"));
    }

    #[test]
    fn mcp_list_no_servers() {
        let cfg = empty_config();
        let text = mcp_subcommand_text("list", &cfg, None).expect("expected text");
        assert_eq!(text, "No MCP servers configured.");
    }

    #[test]
    fn mcp_unknown_subcommand_is_error() {
        let cfg = empty_config();
        assert!(mcp_subcommand_text("frobnicate", &cfg, None).is_err());
    }

    #[test]
    fn mcp_bare_invocation_shows_help() {
        let cfg = empty_config();
        assert!(mcp_subcommand_text("", &cfg, None).is_ok());
    }

    /// With no live manager, `/mcp status` falls back to the config view.
    #[test]
    fn mcp_status_no_live_manager_falls_back_to_config() {
        let cfg = empty_config();
        let text = mcp_subcommand_text("status", &cfg, None).expect("expected text");
        assert!(text.contains("configured"), "fallback text: {text}");
    }
}

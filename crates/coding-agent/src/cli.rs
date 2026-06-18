use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::config::Config;

#[derive(Parser, Debug)]
#[command(
    name = "flown",
    about = "Terminal coding agent powered by LLM",
    version,
    author
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Model to use (overrides config)
    #[arg(short, long, global = true)]
    pub model: Option<String>,

    /// API provider (overrides config)
    #[arg(short, long, global = true)]
    pub provider: Option<String>,

    /// Verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Start an interactive coding session (default)
    Chat {
        /// Initial prompt to send
        #[arg(trailing_var_arg = true)]
        prompt: Vec<String>,
    },

    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },

    /// Manage MCP servers
    Mcp {
        #[command(subcommand)]
        action: Option<McpAction>,
    },

    /// Shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Show current configuration
    Show,

    /// Set a configuration value
    Set {
        /// Configuration key
        key: String,
        /// Value to set
        value: String,
    },

    /// Open config file in editor
    Edit,
}

#[derive(Subcommand, Debug)]
pub enum McpAction {
    /// List configured MCP servers
    List,

    /// Show MCP server status
    Status,

    /// List available MCP tools
    Tools,

    /// Call an MCP tool via JSON-RPC
    Call {
        /// MCP server name
        server: String,
        /// Tool name to call
        tool: String,
        /// JSON arguments
        #[arg(long, default_value = "{}")]
        args: String,
    },
}

// ── Command handlers ──────────────────────────────────────────────

pub async fn cmd_chat(
    model_override: Option<String>,
    _provider_override: Option<String>,
    prompt: Vec<String>,
) -> anyhow::Result<()> {
    let config = Config::load()?;

    let default_model = config.resolve_default_model();
    let model_str = model_override.unwrap_or(default_model);
    let (provider_name, api_key) = config.resolve_provider_and_key(&model_str);

    let initial_prompt = if prompt.is_empty() {
        None
    } else {
        Some(prompt.join(" "))
    };

    crate::interactive_mode::run_tui(config, model_str, provider_name, api_key, initial_prompt)
        .await
}

pub fn cmd_config(action: Option<ConfigAction>) -> anyhow::Result<()> {
    let config = Config::load()?;

    match action.unwrap_or(ConfigAction::Show) {
        ConfigAction::Show => {
            println!("{}", serde_json::to_string_pretty(&config)?);
        }
        ConfigAction::Set { key, value } => {
            println!("Setting {key} = {value}");
        }
        ConfigAction::Edit => {
            let path = Config::config_path();
            println!("Config path: {}", path.display());
        }
    }
    Ok(())
}

pub async fn cmd_mcp(action: Option<McpAction>) -> anyhow::Result<()> {
    use crate::core::mcp::McpManager;

    let config = Config::load()?;

    match action.unwrap_or(McpAction::List) {
        McpAction::List => {
            if config.mcp_servers.is_empty() {
                println!("No MCP servers configured.");
            } else {
                println!("Configured MCP servers:");
                for (name, server) in &config.mcp_servers {
                    let status = if server.disabled {
                        "disabled"
                    } else {
                        "configured"
                    };
                    println!("  {name} — {} ({status})", server.command);
                }
            }
        }
        McpAction::Status => {
            if config.mcp_servers.is_empty() {
                println!("No MCP servers configured.");
            } else {
                let mut manager = McpManager::new(config.mcp_servers.clone());
                manager.connect_all().await;
                let infos = manager.server_info();
                println!("MCP Server Status:");
                println!();
                for info in &infos {
                    let status_icon = match info.status {
                        crate::core::types::McpServerStatus::Connected => "✓",
                        crate::core::types::McpServerStatus::Disconnected => "✗",
                        crate::core::types::McpServerStatus::Error => "⚠",
                    };
                    let tool_count = info.tool_count;
                    let error_info = info
                        .error
                        .as_deref()
                        .map(|e| format!(" ({e})"))
                        .unwrap_or_default();
                    println!(
                        "  {status_icon} {} — {} ({} tool(s)){}",
                        info.name, info.command, tool_count, error_info
                    );
                }
            }
        }
        McpAction::Tools => {
            if config.mcp_servers.is_empty() {
                println!("No MCP servers configured.");
            } else {
                let mut manager = McpManager::new(config.mcp_servers.clone());
                manager.connect_all().await;
                let tool_infos = manager.tool_infos();
                if tool_infos.is_empty() {
                    println!(
                        "No MCP tools available (servers may be disconnected or have no tools)."
                    );
                } else {
                    // Group tools by server
                    let mut by_server: std::collections::BTreeMap<
                        String,
                        Vec<&crate::core::types::ToolInfo>,
                    > = std::collections::BTreeMap::new();
                    for tool in &tool_infos {
                        let server = tool.source.as_deref().unwrap_or("unknown").to_string();
                        by_server.entry(server).or_default().push(tool);
                    }
                    for (server, tools) in &by_server {
                        println!("  {} ({})", server, tools.len());
                        for tool in tools {
                            let desc = if tool.description.is_empty() {
                                String::new()
                            } else {
                                let truncated: String = tool.description.chars().take(80).collect();
                                if truncated.len() < tool.description.len() {
                                    format!(" — {}…", truncated)
                                } else {
                                    format!(" — {}", truncated)
                                }
                            };
                            println!("      {}    {}", tool.name, desc);
                        }
                        println!();
                    }
                }
            }
        }
        McpAction::Call { server, tool, args } => {
            let arguments: serde_json::Value = serde_json::from_str(&args)
                .map_err(|e| anyhow::anyhow!("invalid --args JSON: {e}"))?;
            let mut manager = McpManager::new(config.mcp_servers.clone());
            manager
                .connect(&server)
                .await
                .map_err(|e| anyhow::anyhow!("failed to connect to MCP server '{server}': {e}"))?;
            let result = manager
                .call_tool(&format!("mcp__{server}__{tool}"), arguments)
                .await
                .map_err(|e| anyhow::anyhow!("MCP tool call failed: {e}"))?;
            println!("{result}");
        }
    }
    Ok(())
}

pub fn cmd_completions(shell: clap_complete::Shell) -> anyhow::Result<()> {
    use clap_complete::generate;
    let mut cmd = <Cli as clap::CommandFactory>::command();
    generate(shell, &mut cmd, "flown", &mut std::io::stdout());
    Ok(())
}

// ── Agent construction ────────────────────────────────────────────

/// Build the Agent from config, model string, and API key.
///
/// Returns the harness plus the live `McpManager` (when MCP servers are
/// configured). The manager escapes the function boundary so the TUI can hand
/// it to the McpExtension (which registers MCP tools through the extension
/// layer rather than through `built_in_coding_tools`).
pub async fn build_agent(
    model_str: &str,
    api_key: Option<String>,
    config: &Config,
) -> anyhow::Result<(
    flown_agent::AgentHarness,
    Option<Arc<tokio::sync::Mutex<crate::core::mcp::McpManager>>>,
)> {
    flown_ai::register_built_in_api_providers();

    let (provider_hint, model_id) = model_str
        .find('/')
        .map(|i| (&model_str[..i], &model_str[i + 1..]))
        .unwrap_or(("", model_str));

    let model =
        flown_ai::get_model(provider_hint, model_id).or_else(|| flown_ai::get_model("", model_id));

    let model = match model {
        Some(m) => m,
        None => {
            anyhow::bail!(
                "Model '{}' not found in registry. Check your config.",
                model_str
            );
        }
    };

    // Build system prompt with project context
    let cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let context_files = crate::core::system_prompt::load_project_context_files(&cwd);

    // Load skills from ~/.flown/skills and .claude/skills
    let skills_dir = dirs::home_dir()
        .map(|h| h.join(".flown").join("skills"))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".flown/skills".to_string());

    let local_skills_dir = ".claude/skills";

    let skills = match crate::core::skills::load_skills(&[&skills_dir, local_skills_dir]).await {
        Ok(skills) => skills,
        Err(_) => Vec::new(),
    };

    // Set up MCP manager if configured
    let mcp_manager: Option<Arc<tokio::sync::Mutex<crate::core::mcp::McpManager>>> =
        if !config.mcp_servers.is_empty() {
            let mut mcp_manager = crate::core::mcp::McpManager::new(config.mcp_servers.clone());
            mcp_manager.connect_all().await;
            Some(Arc::new(tokio::sync::Mutex::new(mcp_manager)))
        } else {
            None
        };

    let system_prompt = crate::core::system_prompt::build_system_prompt(
        crate::core::system_prompt::BuildSystemPromptOptions {
            cwd: cwd.clone(),
            context_files,
            skills,
            ..Default::default()
        },
    )
    .await;

    // Execution environment (cloned: moved into harness `env` and into tool builders).
    let env = Arc::new(crate::native_env::NativeExecutionEnv::new());

    // Set up tools. MCP tools are now registered via the McpExtension (see
    // core/extensions), not through built_in_coding_tools.
    let tools = crate::core::tools::built_in_coding_tools(env.clone());

    // Build a persistent session. The harness owns the session internally and
    // auto-persists user/assistant/tool-result messages on MessageEnd/TurnEnd
    // (harness.rs:1709-1738), so coding-agent no longer needs its own persistence
    // task. Session is not Clone, so it is created directly via the repo and
    // moved into the harness by value.
    use flown_agent::{FileSystem, JsonlSessionCreateOptions, JsonlSessionRepo, SessionRepo};
    let fs: Arc<dyn FileSystem> = Arc::new(crate::core::real_fs::RealFileSystem::new());
    let sessions_root = dirs::home_dir()
        .map(|h| h.join(".flown").join("agent").join("sessions"))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".flown/agent/sessions".to_string());
    let repo = JsonlSessionRepo::new(fs, sessions_root);
    let session = repo
        .create(JsonlSessionCreateOptions {
            id: None,
            cwd: cwd.clone(),
            parent_session_path: None,
        })
        .await?;

    let harness = flown_agent::AgentHarness::new(flown_agent::AgentHarnessOptions {
        env,
        session,
        tools,
        system_prompt: flown_agent::SystemPromptConfig::Static(system_prompt),
        model,
        thinking_level: Some(flown_ai::ThinkingLevel::Off),
        get_api_key_and_headers: Some(Arc::new(move |_model: &flown_ai::Model| {
            api_key.clone().map(|k| (k, None))
        })),
        resources: None,
        stream_options: None,
        active_tool_names: None,
        steering_mode: None,
        follow_up_mode: None,
    });

    Ok((harness, mcp_manager))
}

pub fn detect_git_branch() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
}

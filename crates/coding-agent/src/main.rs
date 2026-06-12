mod cli;
mod clipboard;
mod config;
mod core;
mod native_env;
mod skills;
mod system_prompt;
mod tui;

use clap::Parser;
use cli::{Cli, Commands};
use config::Config;
use futures::stream::StreamExt;
use std::sync::Arc;
use tokio::sync::mpsc;

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

    tracing::info!("flown started, logs at {}", log_dir.display());

    let cli = Cli::parse();

    let model_override = cli.model;
    let provider_override = cli.provider;
    let _verbose = cli.verbose;

    match cli.command.unwrap_or(Commands::Chat { prompt: vec![] }) {
        Commands::Chat { prompt } => cmd_chat(model_override, provider_override, prompt).await,
        Commands::Config { action } => cmd_config(action),
        Commands::Mcp { action } => cmd_mcp(action).await,
        Commands::Completions { shell } => cmd_completions(shell),
    }
}

async fn cmd_chat(
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

    run_tui(config, model_str, provider_name, api_key, initial_prompt).await
}

/// Build the Agent from config, model string, and API key.
async fn build_agent(model_str: &str, api_key: Option<String>) -> anyhow::Result<flown_agent::Agent> {
    flown_ai::init();

    let (provider_hint, model_id) = model_str
        .find('/')
        .map(|i| (&model_str[..i], &model_str[i + 1..]))
        .unwrap_or(("", model_str));

    tracing::debug!(provider = %provider_hint, model_id = %model_id, "looking up model");
    let model = flown_ai::models::get_model(provider_hint, model_id)
        .or_else(|| flown_ai::models::get_model("", model_id));

    let model = match model {
        Some(m) => {
            tracing::debug!(model = %m.name, provider = ?m.provider, "model found");
            m
        }
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

    let context_files = system_prompt::load_project_context_files(&cwd);
    tracing::info!(context_files = context_files.len(), "loaded project context files");

    // Load skills from ~/.flown/skills and .claude/skills
    let skills_dir = dirs::home_dir()
        .map(|h| h.join(".flown").join("skills"))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".flown/skills".to_string());

    let local_skills_dir = ".claude/skills";

    let skills = match skills::load_skills(&[&skills_dir, local_skills_dir]).await {
        Ok(skills) => {
            tracing::info!(skills = skills.len(), "loaded skills");
            skills
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to load skills");
            Vec::new()
        }
    };

    // Set up MCP manager if configured
    let config = Config::load().unwrap_or_default();
    tracing::info!(mcp_servers = config.mcp_servers.len(), "MCP servers configured");
    let mcp_manager: Option<Arc<tokio::sync::Mutex<core::mcp::McpManager>>> =
        if !config.mcp_servers.is_empty() {
            let mut mcp_manager = core::mcp::McpManager::new(config.mcp_servers.clone());
            tracing::info!("connecting to MCP servers...");
            mcp_manager.connect_all().await;
            let mcp_tools = mcp_manager.tool_infos();
            tracing::info!(mcp_tools = mcp_tools.len(), "MCP tools available");
            Some(Arc::new(tokio::sync::Mutex::new(mcp_manager)))
        } else {
            tracing::info!("no MCP servers configured");
            None
        };

    let system_prompt = system_prompt::build_system_prompt(system_prompt::BuildSystemPromptOptions {
        cwd,
        context_files,
        skills,
        mcp_manager: mcp_manager.clone(),
        ..Default::default()
    }).await;

    tracing::info!(system_prompt_len = system_prompt.len(), "system prompt built");

    let agent = flown_agent::Agent::new(flown_agent::AgentOptions {
        initial_state: Some(flown_agent::AgentState {
            system_prompt,
            model,
            thinking_level: flown_ai::types::ThinkingLevel::Off,
            messages: Vec::new(),
            is_streaming: false,
            streaming_message: None,
            pending_tool_calls: std::collections::HashSet::new(),
            error_message: None,
        }),
        get_api_key: Some(Arc::new(move |_provider| {
            let key = api_key.clone();
            Box::pin(async move { key })
        })),
        ..Default::default()
    });

    // Set up tools
    let env = Arc::new(native_env::NativeExecutionEnv::new());
    let tools = core::tools::built_in_coding_tools(env, mcp_manager);

    agent.set_tools(tools);
    tracing::info!(tool_count = agent.tools().len(), "tools configured");

    Ok(agent)
}

async fn run_tui(
    _config: Config,
    model_str: String,
    provider_name: String,
    api_key: Option<String>,
    initial_prompt: Option<String>,
) -> anyhow::Result<()> {
    use crossterm::event::{KeyCode, KeyModifiers};
    use tui::editor::EditorAction;
    use tui::terminal::{poll_event, InputEvent, TerminalSession};
    use tui::transcript::TranscriptEntry;

    // Build the real agent
    tracing::info!(model = %model_str, provider = %provider_name, "building agent");
    let agent = match build_agent(&model_str, api_key.clone()).await {
        Ok(a) => {
            tracing::info!("agent built successfully");
            Some(Arc::new(a))
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to build agent");
            eprintln!("Warning: Could not initialize agent: {e}");
            eprintln!("Running in session-only mode (no LLM).");
            None
        }
    };

    // Initialize session manager
    let fs = Arc::new(core::real_fs::RealFileSystem::new());
    let sessions_root = dirs::home_dir()
        .map(|h| h.join(".flown").join("agent").join("sessions"))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".flown/agent/sessions".to_string());

    let mut session_mgr = core::session_manager::SessionManager::new(fs, &sessions_root);

    // Load recent sessions for the welcome page
    let recent_sessions: Vec<tui::welcome::RecentSession> = match session_mgr.list_sessions().await {
        Ok(metas) => metas
            .into_iter()
            .take(5)
            .map(|m| tui::welcome::RecentSession {
                id: m.base.id.clone(),
                name: None,
                created_at: m.base.created_at.clone(),
                path: m.path.clone(),
            })
            .collect(),
        Err(_) => Vec::new(),
    };

    let mut terminal_session = TerminalSession::enter()?;

    let mut status_line = tui::status_line::StatusLine {
        model: model_str.clone(),
        provider: provider_name.clone(),
        cwd: std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "?".into()),
        git_branch: detect_git_branch(),
        git_dirty: false,
        context_pct: 0.0,
        context_total: "200k".into(),
        session_name: None,
    };

    let welcome = tui::welcome::Welcome {
        version: env!("CARGO_PKG_VERSION").to_string(),
        model: model_str.clone(),
        provider: provider_name.clone(),
        recent_sessions,
    };

    let mut transcript = tui::transcript::Transcript::new();
    let mut editor = tui::editor::Editor::new();
    let mut show_welcome = true;

    let (event_tx, mut event_rx) = mpsc::channel::<UiEvent>(256);
    let mut agent_busy = false;

    // Handle initial prompt (from CLI args)
    if let Some(ref prompt) = initial_prompt {
        tracing::info!(user_input = %prompt, "initial prompt");
        show_welcome = false;
        transcript.push(TranscriptEntry::user(prompt));
        if let Err(e) = session_mgr.append_user_message(prompt).await {
            tracing::warn!("Failed to persist user message: {e}");
        }
        if let Some(ref agent) = agent {
            agent_busy = true;
            let agent = agent.clone();
            let tx = event_tx.clone();
            let prompt_text = prompt.clone();
            tokio::spawn(async move {
                tracing::info!(prompt = %prompt_text, "agent stream starting");
                run_agent_stream(agent, prompt_text, tx).await;
                tracing::info!("agent stream finished");
            });
        }
    }

    // Main event loop
    loop {
        // Drain all pending UI events from the agent
        while let Ok(evt) = event_rx.try_recv() {
            match evt {
                UiEvent::TextDelta(delta) => {
                    // Append to the last assistant entry, or create a new one
                    if let Some(last) = transcript.last_mut() {
                        if last.kind == tui::transcript::MessageKind::Assistant {
                            last.body.push_str(&delta);
                        } else {
                            transcript.push(TranscriptEntry::assistant(&delta));
                        }
                    } else {
                        transcript.push(TranscriptEntry::assistant(&delta));
                    }
                    transcript.scroll_to_bottom();
                }
                UiEvent::AssistantText(text) => {
                    tracing::info!(len = text.len(), preview = %text.chars().take(80).collect::<String>(), "assistant response");
                    // Persist to session (display was already handled via TextDelta)
                    let msg = flown_ai::types::AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![flown_ai::types::AssistantContent::Text(
                            flown_ai::types::TextContent {
                                content_type: "text".to_string(),
                                text,
                                text_signature: None,
                            },
                        )],
                        api: flown_ai::types::Api::Known(flown_ai::types::KnownApi::OpenAiCompletions),
                        provider: flown_ai::types::Provider::Known(flown_ai::types::KnownProvider::OpenAi),
                        model: model_str.clone(),
                        response_model: None,
                        response_id: None,
                        usage: Default::default(),
                        stop_reason: flown_ai::types::StopReason::Stop,
                        error_message: None,
                        timestamp: chrono::Utc::now(),
                    };
                    if let Err(e) = session_mgr.append_assistant_message(&flown_agent::types::AgentMessage::Assistant(msg)).await {
                        tracing::warn!("Failed to persist assistant message: {e}");
                    }
                }
                UiEvent::ToolCall { name, args } => {
                    tracing::info!(tool = %name, args = %serde_json::to_string(&args).unwrap_or_default(), "tool call");
                    let formatted = format_tool_call(&name, &args);
                    transcript.push(TranscriptEntry::tool(&formatted));
                }
                UiEvent::ToolResult { name, output, is_error } => {
                    tracing::info!(tool = %name, is_error, len = output.len(), "tool result");
                    if is_error {
                        let msg = if output.starts_with("Error ") {
                            output.clone()
                        } else if name.is_empty() {
                            format!("Error {output}")
                        } else {
                            format!("Error {name}: {output}")
                        };
                        transcript.push(TranscriptEntry::error(msg));
                    }
                }
                UiEvent::Thinking(text) => {
                    tracing::debug!(len = text.len(), "thinking block");
                    transcript.push(TranscriptEntry::thinking(&text));
                }
                UiEvent::Error(msg) => {
                    tracing::warn!(error = %msg, "agent error");
                    transcript.push(TranscriptEntry::error(&msg));
                }
                UiEvent::TurnComplete => {
                    tracing::info!("turn complete");
                    agent_busy = false;
                }
            }
        }

        // Render
        terminal_session.terminal.draw(|f| {
            if show_welcome {
                welcome.render(f, f.area());
            } else {
                tui::layout::render_layout(f, &mut transcript, &status_line, &editor, agent_busy);
            }
        })?;

        let timeout = if agent_busy { 16 } else { 50 };
        let event = poll_event(timeout);

        match event {
            InputEvent::Key(key) => {
                match (key.code, key.modifiers) {
                    (KeyCode::Esc, _) => {
                        if show_welcome {
                            show_welcome = false;
                        } else if agent_busy {
                            agent_busy = false;
                        } else {
                            break;
                        }
                    }
                    (KeyCode::Char('q'), KeyModifiers::CONTROL) => break,
                    _ => {}
                }

                if !show_welcome {
                    let slash_action = editor.handle_key(key);
                    match slash_action {
                        EditorAction::Submit => {
                            let text = editor.text().trim().to_string();
                            if !text.is_empty() {
                                editor.clear();
                                show_welcome = false;

                                if text.starts_with('/') {
                                    handle_slash_command(&text, &mut transcript);
                                } else {
                                    tracing::info!(user_input = %text, "user submitted");
                                    transcript.push(TranscriptEntry::user(&text));
                                    if let Err(e) = session_mgr.append_user_message(&text).await {
                                        tracing::warn!("Failed to persist user message: {e}");
                                    }
                                    if let Some(sid) = session_mgr.current_session_id() {
                                        status_line.session_name = Some(sid[..8].to_string());
                                    }
                                    if let Some(ref agent) = agent {
                                        agent_busy = true;
                                        let agent = agent.clone();
                                        let tx = event_tx.clone();
                                        tracing::info!("spawning agent task");
                                        let handle = tokio::spawn(async move {
                                            tracing::info!(prompt = %text, "agent stream starting");
                                            run_agent_stream(agent, text, tx).await;
                                            tracing::info!("agent stream finished");
                                        });
                                        tracing::info!(?handle, "agent task spawned");
                                    } else {
                                        tracing::warn!("no agent available, cannot process input");
                                        transcript.push(TranscriptEntry::error(
                                            "No LLM agent available. Check your config.",
                                        ));
                                    }
                                }
                            }
                        }
                        EditorAction::Quit => break,
                        EditorAction::None => {}
                    }
                } else {
                    show_welcome = false;
                }
            }
            InputEvent::Mouse(mouse) => {
                use crossterm::event::MouseEventKind;
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        if !show_welcome {
                            transcript.scroll_up(3);
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if !show_welcome {
                            transcript.scroll_down(3);
                        }
                    }
                    _ => {}
                }
            }
            InputEvent::Resize(_, _) => {}
            InputEvent::Tick => {}
            InputEvent::None => {}
        }
    }

    terminal_session.restore()?;
    Ok(())
}

const MAX_TOOL_DISPLAY_LINES: usize = 100;

/// Format a tool call for display, aligned with ~/Projects/flown style.
fn format_tool_call(name: &str, args: &serde_json::Value) -> String {
    match name {
        "read" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            format!("Read {path}")
        }
        "write" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let lang = detect_language(path);
            let visible = truncate_first_lines(content);
            format!("Write {path}\n```{lang}\n{visible}\n```")
        }
        "edit" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let edits = normalized_edit_list(args);
            let (added, removed, diff) = build_edit_diffs(&edits);
            format!("Edit {path}(+{added} -{removed})\n<diff>{diff}\n</diff>")
        }
        "bash" => {
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            format!("Bash\n```bash\n{command}\n```")
        }
        _ => format!("Tool {name}"),
    }
}

fn truncate_first_lines(text: &str) -> String {
    let mut lines = text.lines();
    let mut visible: Vec<&str> = lines.by_ref().take(MAX_TOOL_DISPLAY_LINES).collect();
    if lines.next().is_some() {
        visible.push("...");
    }
    visible.join("\n")
}

fn normalized_edit_list(args: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut edits = match args.get("edits") {
        Some(serde_json::Value::Array(edits)) => edits.clone(),
        _ => Vec::new(),
    };
    // Also handle legacy single-edit format
    if let (Some(old), Some(new)) = (
        args.get("oldText").and_then(|v| v.as_str()),
        args.get("newText").and_then(|v| v.as_str()),
    ) {
        edits.push(serde_json::json!({ "oldText": old, "newText": new }));
    }
    edits
}

fn build_edit_diffs(edits: &[serde_json::Value]) -> (usize, usize, String) {
    let mut total_added = 0usize;
    let mut total_removed = 0usize;
    let mut all_lines = Vec::new();

    for edit in edits {
        let old_text = edit.get("oldText").and_then(|v| v.as_str()).unwrap_or("");
        let new_text = edit.get("newText").and_then(|v| v.as_str()).unwrap_or("");
        let diff = build_line_diff(old_text, new_text);
        total_added += diff.added;
        total_removed += diff.removed;
        all_lines.extend(diff.lines);
    }

    let diff_text = truncate_first_lines(&all_lines.join("\n"));
    (total_added, total_removed, diff_text)
}

struct LineDiff {
    added: usize,
    removed: usize,
    lines: Vec<String>,
}

fn build_line_diff(old_text: &str, new_text: &str) -> LineDiff {
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();

    if old_lines == new_lines {
        return LineDiff {
            added: 0,
            removed: 0,
            lines: old_lines.iter().map(|l| format!(" {l}")).collect(),
        };
    }

    // Simple LCS-based diff
    let old_len = old_lines.len();
    let new_len = new_lines.len();
    let mut lcs = vec![vec![0usize; new_len + 1]; old_len + 1];
    for i in (0..old_len).rev() {
        for j in (0..new_len).rev() {
            lcs[i][j] = if old_lines[i] == new_lines[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut oi = 0;
    let mut ni = 0;
    let mut added = 0;
    let mut removed = 0;
    let mut lines = Vec::new();

    while oi < old_len && ni < new_len {
        if old_lines[oi] == new_lines[ni] {
            lines.push(format!(" {}", old_lines[oi]));
            oi += 1;
            ni += 1;
        } else if lcs[oi + 1][ni] >= lcs[oi][ni + 1] {
            lines.push(format!("-{}", old_lines[oi]));
            removed += 1;
            oi += 1;
        } else {
            lines.push(format!("+{}", new_lines[ni]));
            added += 1;
            ni += 1;
        }
    }
    while oi < old_len {
        lines.push(format!("-{}", old_lines[oi]));
        removed += 1;
        oi += 1;
    }
    while ni < new_len {
        lines.push(format!("+{}", new_lines[ni]));
        added += 1;
        ni += 1;
    }

    LineDiff { added, removed, lines }
}

/// Detect language from file extension for code fences.
fn detect_language(path: &str) -> &str {
    match path.rsplit('.').next() {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("js") => "javascript",
        Some("ts") => "typescript",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("yaml") | Some("yml") => "yaml",
        Some("md") => "markdown",
        Some("sh") => "bash",
        Some("html") => "html",
        Some("css") => "css",
        Some("go") => "go",
        Some("java") => "java",
        Some("c") => "c",
        Some("cpp") | Some("cc") | Some("cxx") => "cpp",
        Some("h") | Some("hpp") => "c",
        _ => "",
    }
}

/// Run the agent stream and forward events to the TUI channel.
async fn run_agent_stream(
    agent: Arc<flown_agent::Agent>,
    prompt: String,
    tx: mpsc::Sender<UiEvent>,
) {
    use flown_agent::AgentEvent;

    let mut stream = agent.prompt(prompt);
    let mut accumulated_text = String::new();
    let mut in_thinking = false;

    while let Some(event) = stream.next().await {
        tracing::trace!(?event, "agent event");
        match event {
            AgentEvent::AgentStart => {
                tracing::info!("agent start");
            }
            AgentEvent::MessageUpdate {
                assistant_message_event,
                ..
            } => {
                use flown_ai::types::AssistantMessageEvent;
                match assistant_message_event {
                    AssistantMessageEvent::TextDelta { delta, .. } => {
                        if in_thinking {
                            let _ = tx.send(UiEvent::Thinking(accumulated_text.clone())).await;
                            accumulated_text.clear();
                            in_thinking = false;
                        }
                        // Stream each delta to the TUI for live rendering
                        let _ = tx.send(UiEvent::TextDelta(delta.clone())).await;
                        accumulated_text.push_str(&delta);
                    }
                    AssistantMessageEvent::ThinkingStart { .. } => {
                        // Flush any accumulated text first
                        if !accumulated_text.is_empty() {
                            let _ = tx.send(UiEvent::AssistantText(accumulated_text.clone())).await;
                            accumulated_text.clear();
                        }
                        in_thinking = true;
                    }
                    AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                        accumulated_text.push_str(&delta);
                    }
                    AssistantMessageEvent::ThinkingEnd { .. } => {
                        let _ = tx.send(UiEvent::Thinking(accumulated_text.clone())).await;
                        accumulated_text.clear();
                        in_thinking = false;
                    }
                    _ => {}
                }
            }
            AgentEvent::MessageEnd { message } => {
                match &message {
                    flown_agent::AgentMessage::Assistant(assistant) => {
                        // Extract final text from assistant message content
                        let text = assistant
                            .content
                            .iter()
                            .filter_map(|c| match c {
                                flown_ai::types::AssistantContent::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("");

                        let final_text = if text.is_empty() {
                            accumulated_text.clone()
                        } else {
                            text
                        };

                        if !final_text.is_empty() {
                            let _ = tx.send(UiEvent::AssistantText(final_text)).await;
                        }
                        accumulated_text.clear();
                    }
                    flown_agent::AgentMessage::ToolResult(result) => {
                        let output = result
                            .content
                            .iter()
                            .filter_map(|c| match c {
                                flown_ai::types::ToolResultContent::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        let _ = tx
                            .send(UiEvent::ToolResult {
                                name: result.tool_name.clone(),
                                output,
                                is_error: result.is_error,
                            })
                            .await;
                    }
                    _ => {}
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_name, args, ..
            } => {
                let _ = tx
                    .send(UiEvent::ToolCall {
                        name: tool_name,
                        args,
                    })
                    .await;
            }
            AgentEvent::ToolExecutionEnd {
                tool_name,
                result,
                is_error,
                ..
            } => {
                let output = result
                    .get("content")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|c| c.get("text").and_then(|v| v.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_else(|| serde_json::to_string_pretty(&result).unwrap_or_default());

                let _ = tx
                    .send(UiEvent::ToolResult {
                        name: tool_name,
                        output,
                        is_error,
                    })
                    .await;
            }
            AgentEvent::AgentEnd { .. } => {
                tracing::info!("agent end");
                let _ = tx.send(UiEvent::TurnComplete).await;
                return;
            }
            AgentEvent::TurnEnd { .. } => {}
            _ => {}
        }
    }

    // Flush remaining text
    if !accumulated_text.is_empty() {
        if in_thinking {
            let _ = tx.send(UiEvent::Thinking(accumulated_text)).await;
        } else {
            let _ = tx.send(UiEvent::AssistantText(accumulated_text)).await;
        }
    }
    let _ = tx.send(UiEvent::TurnComplete).await;
}

/// UI events sent from the agent to the TUI.
enum UiEvent {
    /// Streaming text delta — append to current assistant message
    TextDelta(String),
    /// Complete assistant message (used for session persistence)
    AssistantText(String),
    Thinking(String),
    ToolCall { name: String, args: serde_json::Value },
    ToolResult { name: String, output: String, is_error: bool },
    Error(String),
    TurnComplete,
}

/// Handle slash commands locally.
fn handle_slash_command(text: &str, transcript: &mut tui::transcript::Transcript) {
    use tui::transcript::TranscriptEntry;

    let parts: Vec<&str> = text.splitn(2, ' ').collect();
    let cmd = parts[0];
    let _args = parts.get(1).copied().unwrap_or("");

    match cmd {
        "/help" | "/h" | "/?" => {
            transcript.push(TranscriptEntry::system(
                "Available commands:\n  /help          Show this help\n  /clear         Clear transcript\n  /model <name>  Switch model\n  /compact       Compact conversation\n  /quit          Exit",
            ));
        }
        "/clear" | "/cls" => {
            transcript.clear();
        }
        "/quit" | "/exit" | "/q" => {
            std::process::exit(0);
        }
        _ => {
            transcript.push(TranscriptEntry::error(
                format!("Unknown command: {cmd}. Type /help for available commands.")
            ));
        }
    }
}

fn cmd_config(action: Option<cli::ConfigAction>) -> anyhow::Result<()> {
    use cli::ConfigAction;

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

async fn cmd_mcp(action: Option<cli::McpAction>) -> anyhow::Result<()> {
    use cli::McpAction;
    use core::mcp::McpManager;

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
                        core::types::McpServerStatus::Connected => "✓",
                        core::types::McpServerStatus::Disconnected => "✗",
                        core::types::McpServerStatus::Error => "⚠",
                    };
                    let tool_count = info.tool_count;
                    let error_info = info.error.as_deref().map(|e| format!(" ({e})")).unwrap_or_default();
                    println!("  {status_icon} {} — {} ({} tool(s)){}",
                        info.name, info.command, tool_count, error_info);
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
                    println!("No MCP tools available (servers may be disconnected or have no tools).");
                } else {
                    // Group tools by server
                    let mut by_server: std::collections::BTreeMap<String, Vec<&core::types::ToolInfo>> = std::collections::BTreeMap::new();
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
            manager.connect(&server).await
                .map_err(|e| anyhow::anyhow!("failed to connect to MCP server '{server}': {e}"))?;
            let result = manager.call_tool(&format!("mcp__{server}__{tool}"), arguments).await
                .map_err(|e| anyhow::anyhow!("MCP tool call failed: {e}"))?;
            println!("{result}");
        }
    }
    Ok(())
}

fn cmd_completions(shell: clap_complete::Shell) -> anyhow::Result<()> {
    use clap_complete::generate;
    let mut cmd = <Cli as clap::CommandFactory>::command();
    generate(shell, &mut cmd, "flown", &mut std::io::stdout());
    Ok(())
}

fn detect_git_branch() -> Option<String> {
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


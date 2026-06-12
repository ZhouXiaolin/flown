use std::sync::Arc;

use futures::stream::StreamExt;
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;

use crate::cli;
use crate::config::Config;
use crate::tui::transcript::Transcript;
use crate::tui::component::Component;
use crate::tui::editor::EditorAction;
use crate::tui::terminal::{poll_event, InputEvent, TerminalSession};

pub async fn run_tui(
    config: Config,
    model_str: String,
    provider_name: String,
    api_key: Option<String>,
    initial_prompt: Option<String>,
) -> anyhow::Result<()> {
    use crossterm::event::{KeyCode, KeyModifiers};

    // Build the real agent
    let agent = match cli::build_agent(&model_str, api_key.clone(), &config).await {
        Ok(a) => Some(Arc::new(a)),
        Err(e) => {
            eprintln!("Warning: Could not initialize agent: {e}");
            eprintln!("Running in session-only mode (no LLM).");
            None
        }
    };

    // Initialize session manager
    let fs = Arc::new(crate::core::real_fs::RealFileSystem::new());
    let sessions_root = dirs::home_dir()
        .map(|h| h.join(".flown").join("agent").join("sessions"))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".flown/agent/sessions".to_string());

    let mut session_mgr = crate::core::session_manager::SessionManager::new(fs, &sessions_root);

    let mut terminal_session = TerminalSession::enter()?;

    let mut status_line = crate::tui::status_line::StatusLine::new();
    status_line.model = model_str.clone();
    status_line.provider = provider_name.clone();
    status_line.thinking_level = "off".into();
    status_line.cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    status_line.git_branch = cli::detect_git_branch();
    status_line.context_total = "200k".into();

    let mut transcript = Transcript::new();
    let mut editor = crate::tui::editor::Editor::new();
    let mut hint_bar = crate::tui::hint_bar::HintBar::new();

    let (event_tx, mut event_rx) = mpsc::channel::<flown_agent::AgentEvent>(256);
    let mut agent_busy = false;
    let mut agent_handle: Option<tokio::task::JoinHandle<()>> = None;
    let mut accumulated_text = String::new();
    let mut in_thinking = false;

    // Handle initial prompt (from CLI args)
    if let Some(ref prompt) = initial_prompt {
        transcript.push_user(prompt);
        if let Err(_) = session_mgr.append_user_message(prompt).await {}
        if let Some(ref agent) = agent {
            agent_busy = true;
            let agent = agent.clone();
            let tx = event_tx.clone();
            let prompt_text = prompt.clone();
            agent_handle = Some(tokio::spawn(async move {
                forward_agent_stream(agent, prompt_text, tx).await;
            }));
        }
    }

    // Main event loop
    loop {
        // Drain all pending agent events
        while let Ok(event) = event_rx.try_recv() {
            use flown_agent::AgentEvent;
            use flown_ai::types::AssistantMessageEvent;

            match event {
                AgentEvent::AgentStart => {}

                AgentEvent::MessageUpdate {
                    assistant_message_event,
                    ..
                } => match assistant_message_event {
                    AssistantMessageEvent::TextDelta { delta, .. } => {
                        if in_thinking {
                            transcript.push_thinking(&accumulated_text);
                            accumulated_text.clear();
                            in_thinking = false;
                        }
                        if !transcript.append_to_assistant(&delta) {
                            transcript.push_assistant(&delta);
                        }
                        transcript.scroll_to_bottom();
                        accumulated_text.push_str(&delta);
                    }
                    AssistantMessageEvent::ThinkingStart { .. } => {
                        accumulated_text.clear();
                        in_thinking = true;
                    }
                    AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                        accumulated_text.push_str(&delta);
                    }
                    AssistantMessageEvent::ThinkingEnd { .. } => {
                        transcript.push_thinking(&accumulated_text);
                        accumulated_text.clear();
                        in_thinking = false;
                    }
                    _ => {}
                },

                AgentEvent::MessageEnd { message } => match &message {
                    flown_agent::AgentMessage::Assistant(assistant) => {
                        let text: String = assistant
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
                            let msg = flown_ai::types::AssistantMessage {
                                role: "assistant".to_string(),
                                content: vec![flown_ai::types::AssistantContent::Text(
                                    flown_ai::types::TextContent {
                                        content_type: "text".to_string(),
                                        text: final_text,
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
                            if let Err(_) = session_mgr.append_assistant_message(
                                &flown_agent::types::AgentMessage::Assistant(msg),
                            ).await {}
                        }
                        accumulated_text.clear();
                    }
                    flown_agent::AgentMessage::ToolResult(result) => {
                        if result.is_error {
                            let output: String = result
                                .content
                                .iter()
                                .filter_map(|c| match c {
                                    flown_ai::types::ToolResultContent::Text(t) => Some(t.text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            let msg = if output.starts_with("Error ") {
                                output
                            } else if result.tool_name.is_empty() {
                                format!("Error {output}")
                            } else {
                                format!("Error {}: {output}", result.tool_name)
                            };
                            transcript.push_error(msg);
                        }
                    }
                    _ => {}
                },

                AgentEvent::ToolExecutionStart {
                    tool_name, args, ..
                } => {
                    let formatted = format_tool_call(&tool_name, &args);
                    transcript.push_tool(&formatted);
                }

                AgentEvent::ToolExecutionEnd {
                    tool_name,
                    result,
                    is_error,
                    ..
                } => {
                    if is_error {
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
                        let msg = if output.starts_with("Error ") {
                            output
                        } else {
                            format!("Error {tool_name}: {output}")
                        };
                        transcript.push_error(msg);
                    }
                }

                AgentEvent::AgentEnd { .. } => {
                    agent_busy = false;
                    agent_handle = None;
                }

                _ => {}
            }
        }

        // Guard: if the agent task finished without sending AgentEnd (e.g. panic),
        // reset agent_busy so the UI doesn't freeze forever.
        if agent_busy {
            if let Some(ref handle) = agent_handle {
                if handle.is_finished() {
                    agent_busy = false;
                    agent_handle = None;
                }
            }
        }

        // Update status line state
        status_line.busy = agent_busy;
        if agent_busy {
            status_line.tick();
        }

        // ── Render ────────────────────────────────────────────────────
        terminal_session.terminal.draw(|f| {
            let area = f.area();
            let editor_height = editor.input_height(area.width).max(3);

            // [transcript (fill)]
            // [status line (1 row)]
            // [editor (dynamic height)]
            // [hint bar (1 row)]
            let chunks = Layout::vertical([
                Constraint::Min(10),
                Constraint::Length(1),
                Constraint::Length(editor_height),
                Constraint::Length(1),
            ])
            .split(area);

            transcript.render_frame(f, chunks[0]);
            status_line.render_frame(f, chunks[1]);
            editor.render_frame(f, chunks[2]);

            hint_bar.busy = agent_busy;
            let hint_lines = hint_bar.render(chunks[3].width);
            f.render_widget(Paragraph::new(hint_lines), chunks[3]);
        })?;
        terminal_session.terminal.show_cursor()?;

        // ── Input ─────────────────────────────────────────────────────
        let timeout = if agent_busy { 16 } else { 50 };
        let event = poll_event(timeout);

        match event {
            InputEvent::Key(key) => {
                match (key.code, key.modifiers) {
                    (KeyCode::Esc, _) => {
                        if agent_busy {
                            agent_busy = false;
                        } else {
                            break;
                        }
                    }
                    (KeyCode::Char('q'), KeyModifiers::CONTROL) => break,
                    _ => {}
                }

                let slash_action = editor.handle_key(key);
                match slash_action {
                    EditorAction::Submit => {
                        let text = editor.text().trim().to_string();
                        if !text.is_empty() {
                            editor.clear();

                            if text.starts_with('/') {
                                if handle_slash_command(&text, &mut transcript) {
                                    break;
                                }
                            } else {
                                transcript.push_user(&text);
                                if let Err(_) = session_mgr.append_user_message(&text).await {}
                                if let Some(sid) = session_mgr.current_session_id() {
                                    status_line.session_name = Some(sid[..8].to_string());
                                }
                                if let Some(ref agent) = agent {
                                    agent_busy = true;
                                    let agent = agent.clone();
                                    let tx = event_tx.clone();
                                    accumulated_text.clear();
                                    in_thinking = false;
                                    let handle = tokio::spawn(async move {
                                        forward_agent_stream(agent, text, tx).await;
                                    });
                                    agent_handle = Some(handle);
                                } else {
                                    transcript.push_error(
                                        "No LLM agent available. Check your config.",
                                    );
                                }
                            }
                        }
                    }
                    EditorAction::Quit => break,
                    EditorAction::None => {}
                }
            }
            InputEvent::Mouse(mouse) => {
                use crossterm::event::MouseEventKind;
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        transcript.scroll_up(3);
                    }
                    MouseEventKind::ScrollDown => {
                        transcript.scroll_down(3);
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

/// Forward the agent stream directly through the channel — no translation layer.
async fn forward_agent_stream(
    agent: Arc<flown_agent::Agent>,
    prompt: String,
    tx: mpsc::Sender<flown_agent::AgentEvent>,
) {
    let mut stream = agent.prompt(prompt);
    while let Some(event) = stream.next().await {
        if tx.send(event).await.is_err() {
            break;
        }
    }
}

/// Handle slash commands locally. Returns `true` if the user wants to quit.
fn handle_slash_command(text: &str, transcript: &mut Transcript) -> bool {
    let parts: Vec<&str> = text.splitn(2, ' ').collect();
    let cmd = parts[0];
    let _args = parts.get(1).copied().unwrap_or("");

    match cmd {
        "/help" | "/h" | "/?" => {
            transcript.push_system(
                "Available commands:\n  /help          Show this help\n  /clear         Clear transcript\n  /model <name>  Switch model\n  /compact       Compact conversation\n  /quit          Exit",
            );
        }
        "/clear" | "/cls" => {
            transcript.clear();
        }
        "/quit" | "/exit" | "/q" => {
            return true;
        }
        _ => {
            transcript.push_error(
                format!("Unknown command: {cmd}. Type /help for available commands.")
            );
        }
    }
    false
}

// ── Tool call formatting helpers ──────────────────────────────────

const MAX_TOOL_DISPLAY_LINES: usize = 100;

/// Format a tool call for display.
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
            format!("Bash({command})")
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
    let diff = similar::TextDiff::from_lines(old_text, new_text);
    let mut added = 0;
    let mut removed = 0;
    let mut lines = Vec::new();

    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Delete => {
                removed += 1;
                lines.push(format!("-{}", change.to_string_lossy().trim_end_matches('\n')));
            }
            similar::ChangeTag::Insert => {
                added += 1;
                lines.push(format!("+{}", change.to_string_lossy().trim_end_matches('\n')));
            }
            similar::ChangeTag::Equal => {
                lines.push(format!(" {}", change.to_string_lossy().trim_end_matches('\n')));
            }
        }
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

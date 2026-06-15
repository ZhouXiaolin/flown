//! The TUI entry point and the cross-runtime bridge.
//!
//! flown's agent runs on **tokio** (`Agent::prompt` returns a `Send` stream;
//! `SessionManager` is async). iodilos runs its own single-threaded,
//! `Rc`/`thread_local`-based event loop via `Renderer::run_blocking`. These two
//! loops cannot merge, so a `flume` channel carries `AgentEvent`s from the
//! tokio side to the iodilos side:
//!
//! ```text
//!   [tokio multi-thread runtime]              [iodilos main thread]
//!   run_tui:                                    renderer.mount(|| { … })
//!     ├ build agent + session manager            └ spawn_local(event pump)
//!     └ tokio::spawn(agent driver)──┐                 └ rx.recv_async().await
//!         agent.prompt(text)         │ flume               → state.* updates
//!         stream.next().await        │                     (signal writes →
//!         tx.send(AgentEvent) ───────┴──────────────►       dirty flag → redraw)
//! ```
//!
//! `main` stays `#[tokio::main]` (multi-thread). It builds the agent + session
//! manager on the runtime, then calls `run_tui`, which enters
//! `renderer.run_blocking()`. The tokio worker threads keep driving the agent
//! stream while the **main thread** is blocked inside iodilos's loop. The iodilos
//! event pump drains the `flume` receiver with flume's futures-0.3 API, which
//! iodilos's `spawn_local` executor polls directly.

use std::rc::Rc;
use std::sync::Arc;

use futures::stream::StreamExt;
use iodilos::prelude::*;

use crate::cli;
use crate::config::Config;
use crate::tui::components::app::{App, AppProps};
use crate::tui::state::{BUSY_FRAMES, StatusInfo, UiState};

/// Run the TUI. Called from `interactive_mode.rs` after the agent + config are
/// built on the tokio runtime.
pub async fn run_tui(
    config: Config,
    model_str: String,
    provider_name: String,
    api_key: Option<String>,
    initial_prompt: Option<String>,
) -> anyhow::Result<()> {
    // Build the agent on the tokio runtime (it needs async init).
    let agent = match cli::build_agent(&model_str, api_key.clone(), &config).await {
        Ok(a) => Some(Arc::new(a)),
        Err(e) => {
            eprintln!("Warning: Could not initialize agent: {e}");
            eprintln!("Running in session-only mode (no LLM).");
            None
        }
    };

    // Session manager (best-effort persistence — errors ignored, matching the
    // old behavior). Owned by a dedicated tokio task that drains finalized
    // entries off the persist channel.
    let fs = Arc::new(crate::core::real_fs::RealFileSystem::new());
    let sessions_root = dirs::home_dir()
        .map(|h| h.join(".flown").join("agent").join("sessions"))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".flown/agent/sessions".to_string());
    let mut session_mgr = crate::core::session_manager::SessionManager::new(fs, &sessions_root);

    // The flume bridge: tokio agent driver → iodilos event pump.
    let (event_tx, event_rx) = flume::unbounded::<flown_agent::AgentEvent>();

    // Persistence channel: iodilos event pump → tokio session-manager task.
    let (persist_tx, persist_rx) = flume::unbounded::<PersistReq>();

    // Spawn the persistence task (tokio side). It owns the session manager and
    // drains finalized entries. Best-effort: errors are ignored.
    {
        // Move the real session manager into the task; leave a throwaway stub
        // behind so `session_mgr` stays borrowable-free for the rest of run_tui.
        let mut session_mgr = std::mem::replace(
            &mut session_mgr,
            crate::core::session_manager::SessionManager::new(
                Arc::new(crate::core::real_fs::RealFileSystem::new()),
                "",
            ),
        );
        tokio::spawn(async move {
            while let Ok(req) = persist_rx.recv_async().await {
                let _ = match req {
                    PersistReq::User(text) => session_mgr.append_user_message(&text).await,
                    PersistReq::Assistant(msg) => session_mgr.append_assistant_message(&msg).await,
                };
            }
        });
    }

    // Initial status snapshot.
    let status = StatusInfo {
        model: model_str.clone(),
        provider: provider_name.clone(),
        thinking_level: "off".into(),
        cwd: std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "?".into()),
        git_branch: cli::detect_git_branch(),
        context_total: "200k".into(),
        ..StatusInfo::default()
    };

    // If there's an initial prompt, kick off the agent driver before mounting.
    let initial_busy = initial_prompt.is_some() && agent.is_some();
    if let Some(ref prompt) = initial_prompt {
        let _ = persist_tx.send(PersistReq::User(prompt.clone()));
        if let Some(ref agent) = agent {
            let agent = Arc::clone(agent);
            let tx = event_tx.clone();
            let prompt_text = prompt.clone();
            tokio::spawn(async move {
                forward_agent_stream(agent, prompt_text, tx).await;
            });
        }
    }

    // Build the iodilos renderer. From here on the main thread is owned by
    // iodilos's event loop until quit.
    let mut renderer = Renderer::new()?;

    // Captures for the mount closure: the agent handle + event sender (for the
    // App's on_key to spawn new prompts) and the persist sender + receiver (the
    // receiver moves into the event pump). `status` seeds the initial snapshot.
    let mount_agent = agent.clone();
    let mount_status = status.clone();
    let mount_busy = initial_busy;
    let mount_persist_tx = persist_tx.clone();
    let mount_event_tx = event_tx.clone();
    let mount_config = config.clone();

    renderer.mount(move || {
        // The shared reactive state, seeded with the initial status + busy flag.
        let state = Rc::new(UiState::new(TextAreaState::default()));
        state.status.update(|s| *s = mount_status.clone());
        state.busy.set(mount_busy);

        // Provide shared handles via iodilos context. The App component and its
        // on_key handler read these (they run under an owner, so use_context
        // works). Note: the spawn_local event pump CANNOT use_context — it has
        // no active owner while polled — so it captures what it needs by move.
        provide_context(Rc::clone(&state));
        provide_context(mount_agent.clone());
        provide_context(mount_event_tx.clone());
        provide_context(mount_persist_tx.clone());
        provide_context(mount_config.clone());

        // The event pump: drain the flume receiver and translate each
        // AgentEvent into UiState mutations. Runs on the iodilos thread; flume's
        // recv_async is pollable by iodilos's spawn_local executor. Captures the
        // persist sender by move (no context access inside the future).
        let pump_state = Rc::clone(&state);
        let pump_rx = event_rx;
        let pump_persist_tx = mount_persist_tx.clone();
        spawn_local(async move {
            let mut accumulated_text = String::new();
            let mut in_thinking = false;
            while let Ok(event) = pump_rx.recv_async().await {
                translate_event(
                    event,
                    &pump_state,
                    &pump_persist_tx,
                    &mut accumulated_text,
                    &mut in_thinking,
                );
            }
        });

        // Busy-spinner tick: advance the spinner frame while an agent is
        // running. Registered once at mount; owner-scoped, cleaned up on quit.
        let spinner_state = Rc::clone(&state);
        every(std::time::Duration::from_millis(500), move || {
            if spinner_state.busy.get() {
                spinner_state.status.update(|s| {
                    s.frame = (s.frame + 1) % BUSY_FRAMES.len();
                });
            } else {
                // Reset the frame when idle so the spinner resumes from the top.
                spinner_state.status.update(|s| s.frame = 0);
            }
        });

        view! { App() }
    });

    renderer.run().await?;
    Ok(())
}

/// Translate one `AgentEvent` into `UiState` mutations. A near-verbatim port of
/// the old `interactive_mode.rs` event-match block, but every `transcript.*`
/// call becomes a `state.*` call, and finalized assistant messages ship to the
/// persistence task via `persist_tx`.
fn translate_event(
    event: flown_agent::AgentEvent,
    state: &UiState,
    persist_tx: &flume::Sender<PersistReq>,
    accumulated_text: &mut String,
    in_thinking: &mut bool,
) {
    use flown_agent::AgentEvent;
    use flown_ai::types::AssistantMessageEvent;

    match event {
        AgentEvent::AgentStart => {}

        AgentEvent::MessageUpdate {
            assistant_message_event,
            ..
        } => match assistant_message_event {
            AssistantMessageEvent::TextDelta { delta, .. } => {
                if *in_thinking {
                    state.push_thinking(std::mem::take(accumulated_text));
                    *in_thinking = false;
                }
                if !state.append_to_assistant(&delta) {
                    state.push_assistant(&delta);
                }
                accumulated_text.push_str(&delta);
            }
            AssistantMessageEvent::ThinkingStart { .. } => {
                accumulated_text.clear();
                *in_thinking = true;
            }
            AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                accumulated_text.push_str(&delta);
            }
            AssistantMessageEvent::ThinkingEnd { .. } => {
                state.push_thinking(std::mem::take(accumulated_text));
                *in_thinking = false;
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
                    // Ship a clean AssistantMessage to the persistence task.
                    let msg = flown_ai::types::AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![flown_ai::types::AssistantContent::Text(
                            flown_ai::types::TextContent {
                                content_type: "text".to_string(),
                                text: final_text,
                                text_signature: None,
                            },
                        )],
                        api: flown_ai::types::Api::Known(
                            flown_ai::types::KnownApi::OpenAiCompletions,
                        ),
                        provider: flown_ai::types::Provider::Known(
                            flown_ai::types::KnownProvider::OpenAi,
                        ),
                        model: String::new(),
                        response_model: None,
                        response_id: None,
                        usage: Default::default(),
                        stop_reason: flown_ai::types::StopReason::Stop,
                        error_message: None,
                        timestamp: chrono::Utc::now(),
                    };
                    let _ = persist_tx.send(PersistReq::Assistant(Box::new(
                        flown_agent::types::AgentMessage::Assistant(msg),
                    )));
                }
                accumulated_text.clear();
            }
            flown_agent::AgentMessage::ToolResult(result) if result.is_error => {
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
                state.push_error(msg);
            }
            _ => {}
        },

        AgentEvent::ToolExecutionStart {
            tool_name, args, ..
        } => {
            state.push_tool_call(&tool_name, &args);
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
                state.push_error(msg);
            }
        }

        AgentEvent::AgentEnd { .. } => {
            state.busy.set(false);
            state.status.update(|s| s.busy = false);
        }

        _ => {}
    }
}

/// Forward the agent stream into the flume channel. Runs on a tokio worker
/// thread.
async fn forward_agent_stream(
    agent: Arc<flown_agent::Agent>,
    prompt: String,
    tx: flume::Sender<flown_agent::AgentEvent>,
) {
    let mut stream = agent.prompt(prompt);
    while let Some(event) = stream.next().await {
        if tx.send_async(event).await.is_err() {
            break;
        }
    }
}

/// Best-effort persistence requests, shipped from the iodilos thread to the
/// tokio session-manager task.
pub enum PersistReq {
    User(String),
    Assistant(Box<flown_agent::types::AgentMessage>),
}

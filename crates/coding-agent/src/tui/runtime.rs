//! The TUI entry point and the cross-runtime bridge.
//!
//! flown's agent runs on **tokio** (`AgentHarness::prompt` is async). iodilos
//! runs its own single-threaded, `Rc`/`thread_local`-based event loop via
//! `Renderer::run_blocking`. These two loops cannot merge, so a `flume` channel
//! carries `HarnessEvent`s from the tokio side to the iodilos side:
//!
//! ```text
//!   [tokio multi-thread runtime]              [iodilos main thread]
//!   run_tui:                                    renderer.mount(|| { … })
//!     ├ build harness + register subscriber       └ spawn_local(event pump)
//!     └ tokio::spawn(harness.prompt(text))──┐          └ rx.recv_async().await
//!         harness.execute_turn()             │ flume         → state.* updates
//!         emit_any(HarnessEvent) ────────────┴──────────►     (signal writes →
//!                                                            dirty flag → redraw)
//! ```
//!
//! `main` stays `#[tokio::main]` (multi-thread). It builds the harness on the
//! runtime, then calls `run_tui`, which enters `renderer.run_blocking()`. The
//! tokio worker threads keep driving the harness while the **main thread** is
//! blocked inside iodilos's loop. The iodilos event pump drains the `flume`
//! receiver with flume's futures-0.3 API, which iodilos's `spawn_local`
//! executor polls directly.
//!
//! Persistence is owned by the harness: on `MessageEnd`/`TurnEnd` it appends to
//! the session tree and writes to disk (`harness.rs:1709-1738`). coding-agent
//! no longer runs its own persistence task.

use std::rc::Rc;
use std::sync::Arc;

use iodilos::prelude::*;

use crate::cli;
use crate::config::Config;
use crate::tui::components::app::{App, AppProps};
use crate::tui::state::{BUSY_FRAMES, StatusInfo, UiState};
use flown_agent::harness::HarnessEvent;

/// Run the TUI. Called from `interactive_mode.rs` after the harness + config are
/// built on the tokio runtime.
pub async fn run_tui(
    config: Config,
    model_str: String,
    provider_name: String,
    api_key: Option<String>,
    initial_prompt: Option<String>,
) -> anyhow::Result<()> {
    // Build the harness on the tokio runtime (it needs async init).
    // `build_agent` returns the harness plus the live McpManager (when MCP is
    // configured); the manager is handed to the extension layer below.
    let (harness, mcp_manager, built_in_tools) = match cli::build_agent(
        &model_str,
        api_key.clone(),
        &config,
    )
    .await
    {
        Ok((h, mcp)) => {
            // The harness was constructed with the built-in tools; the extension
            // layer needs them too so it can prepend them to every set_tools
            // call (set_tools is full-replace — see extensions::runner::ToolSide).
            let built_in = h.tools();
            (Some(Arc::new(h)), mcp, Some(built_in))
        }
        Err(e) => {
            eprintln!("Warning: Could not initialize agent: {e}");
            eprintln!("Running in session-only mode (no LLM).");
            (None, None, None)
        }
    };

    // Build the extension layer on the tokio side. `tool_side` owns the harness
    // and reconciles runtime tool edits (MCP servers connecting/disconnecting);
    // `command_table` is pure Send metadata moved into iodilos and bound to the
    // UiState sink at mount. Only meaningful when the harness exists.
    let extension = match (&harness, built_in_tools.as_ref()) {
        (Some(h), Some(built_in)) => Some(crate::core::extensions::build_runner(
            Arc::clone(h),
            config.clone(),
            built_in.clone(),
            mcp_manager.clone(),
        )),
        _ => None,
    };
    let tool_side = extension.as_ref().map(|(ts, _)| ts.clone());
    let command_table = extension.map(|(_, ct)| ct);

    // Seed the harness with the full initial tool set (built-in + MCP one-shot).
    // The harness was constructed with built-ins only; this overlays the MCP
    // tools discovered during registration. Spawned on tokio (set_tools is async).
    if let (Some(tool_side), Some(harness)) = (&tool_side, &harness) {
        let initial = tool_side.initial_tools();
        let h = Arc::clone(harness);
        tokio::spawn(async move {
            let _ = h.set_tools(initial, None).await;
        });
        // Reconcile loop: periodically flush runtime tool edits (MCP server
        // connect/disconnect) into the harness. Polls on tokio; cheap no-op
        // when no ToolHandle was dirtied.
        let ts = tool_side.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(1000));
            loop {
                interval.tick().await;
                ts.reconcile_tools().await;
            }
        });
    }

    // The flume bridge: harness subscriber (tokio) → iodilos event pump.
    let (event_tx, event_rx) = flume::unbounded::<HarnessEvent>();

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

    // If there's an initial prompt, kick off the harness driver before mounting.
    let initial_busy = initial_prompt.is_some() && harness.is_some();
    if let Some(ref prompt) = initial_prompt {
        if let Some(ref harness) = harness {
            let harness = Arc::clone(harness);
            let prompt_text = prompt.clone();
            tokio::spawn(async move {
                let _ = harness.prompt(&prompt_text, None).await;
            });
        }
    }

    // Build the iodilos renderer. From here on the main thread is owned by
    // iodilos's event loop until quit.
    let mut renderer = Renderer::new()?;

    // Captures for the mount closure: the harness handle + event sender (for the
    // App's on_key to spawn new prompts). `status` seeds the initial snapshot.
    // `mount_command_table` is the extension command registry, bound to the
    // UiState sink inside the mount closure (where Rc<UiState> exists).
    let mount_harness = harness.clone();
    let mount_status = status.clone();
    let mount_busy = initial_busy;
    let mount_event_tx = event_tx.clone();
    let mount_config = config.clone();
    let mut mount_command_table = command_table;

    renderer.mount(move || {
        // The shared reactive state, seeded with the initial status + busy flag.
        let state = Rc::new(UiState::new(TextAreaState::default()));
        state.status.update(|s| *s = mount_status.clone());
        state.busy.set(mount_busy);

        // Bind the extension command table to the live UiState, producing the
        // dispatch-capable CommandSide. The sink translates each CommandEffect
        // into the matching UiState operation (notify/notify_error/clear).
        // Wrapped in Rc so it can be shared via iodilos context (which requires
        // Clone); CommandSide itself owns Rc<CommandSink> and isn't Clone.
        // Always provide a value (Some when the extension layer exists, None
        // otherwise) so consumers can `use_context::<Option<Rc<CommandSide>>>`
        // without panicking.
        let command_side: Option<Rc<crate::core::extensions::CommandSide>> =
            mount_command_table.take().map(|table| {
                let state_for_sink = Rc::clone(&state);
                let sink = Rc::new(crate::core::extensions::CommandSink {
                    notify: Box::new(move |text: String| {
                        state_for_sink.push_system(text);
                    }),
                    notify_error: Box::new({
                        let state = Rc::clone(&state);
                        move |text: String| {
                            state.push_error(text);
                        }
                    }),
                    clear: Box::new({
                        let state = Rc::clone(&state);
                        move || {
                            state.clear();
                        }
                    }),
                });
                Rc::new(table.bind(sink))
            });
        provide_context(command_side);

        // Provide shared handles via iodilos context. The App component and its
        // on_key handler read these (they run under an owner, so use_context
        // works). Note: the spawn_local event pump CANNOT use_context — it has
        // no active owner while polled — so it captures what it needs by move.
        provide_context(Rc::clone(&state));
        provide_context(mount_harness.clone());
        provide_context(mount_config.clone());

        // Register a harness subscriber ONCE at mount. It runs on the tokio
        // worker during execute_turn (synchronous emit_any) and forwards each
        // HarnessEvent into the flume channel. This replaces the old pull-based
        // Stream consumption (forward_agent_stream).
        if let Some(ref harness) = mount_harness {
            let tx = mount_event_tx.clone();
            harness.subscribe(move |event: &HarnessEvent| {
                let _ = tx.send(event.clone());
            });
        }

        // The event pump: drain the flume receiver and translate each
        // HarnessEvent into UiState mutations. Runs on the iodilos thread;
        // flume's recv_async is pollable by iodilos's spawn_local executor.
        let pump_state = Rc::clone(&state);
        let pump_rx = event_rx;
        spawn_local(async move {
            let mut accumulated_text = String::new();
            let mut in_thinking = false;
            while let Ok(event) = pump_rx.recv_async().await {
                translate_event(
                    event,
                    &pump_state,
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

/// Translate one `HarnessEvent` into `UiState` mutations. A near-verbatim port
/// of the old `interactive_mode.rs` event-match block, but every `transcript.*`
/// call becomes a `state.*` call. Persistence is now owned by the harness
/// (MessageEnd/TurnEnd → session.append_message), so this function no longer
/// ships messages to a separate persistence task.
fn translate_event(
    event: HarnessEvent,
    state: &UiState,
    accumulated_text: &mut String,
    in_thinking: &mut bool,
) {
    use flown_ai::types::AssistantMessageEvent;

    match event {
        HarnessEvent::AgentStart => {}

        HarnessEvent::MessageUpdate {
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

        HarnessEvent::MessageEnd { message } => match &message {
            flown_agent::AgentMessage::Assistant(_) => {
                // The finalized assistant message is persisted by the harness.
                // Reset the streaming accumulator for the next message.
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

        HarnessEvent::ToolExecutionStart {
            tool_name, args, ..
        } => {
            state.push_tool_call(&tool_name, &args);
        }

        HarnessEvent::ToolExecutionEnd {
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

        HarnessEvent::AgentEnd { .. } => {
            state.busy.set(false);
            state.status.update(|s| s.busy = false);
        }

        _ => {}
    }
}

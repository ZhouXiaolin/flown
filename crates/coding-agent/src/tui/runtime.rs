//! The TUI entry point and the cross-runtime bridge.
//!
//! flown's agent runs on **tokio** (`AgentHarness::prompt` is async). iodilos
//! runs its own single-threaded, `Rc`/`thread_local`-based event loop via
//! `Renderer::run_blocking`. These two loops cannot merge, so a `flume` channel
//! carries `AgentHarnessEvent`s from the tokio side to the iodilos side:
//!
//! ```text
//!   [tokio multi-thread runtime]              [iodilos main thread]
//!   run_tui:                                    renderer.mount(|| { … })
//!     ├ build harness + register subscriber       └ spawn_local(event pump)
//!     └ tokio::spawn(harness.prompt(text))──┐          └ rx.recv_async().await
//!         harness.execute_turn()             │ flume         → state.* updates
//!         emit_any(AgentHarnessEvent) ────────────┴──────────►     (signal writes →
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
use flown_agent::AgentHarnessEvent;

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
    let (harness, mcp_manager, built_in_tools) =
        match cli::build_agent(&model_str, api_key.clone(), &config).await {
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
        // Reconcile loop: flush runtime tool edits (MCP server
        // connect/disconnect) into the harness. Event-driven via each
        // `ToolStore`'s `Notify` — wakes immediately on an edit and stays idle
        // otherwise, instead of waking on a fixed timer. Cheap no-op when no
        // `ToolHandle` was dirtied.
        tokio::spawn(tool_side.clone().run_reconcile());
    }

    // Bind the main harness to its transcript ATOMICALLY: one call creates the
    // event channel, subscribes the harness to forward into it, and spawns the
    // driver task (owning the harness Arc). This is the single place the main
    // harness is wired to the UI, so the one-harness-one-transcript invariant
    // is structural. Returns None in session-only mode (no harness).
    //
    // The initial prompt — if any — is queued on the driver's priority channel
    // here, replacing the old `tokio::spawn(harness.prompt())`.
    let initial_busy = initial_prompt.is_some() && harness.is_some();
    let main_binding = harness.as_ref().map(|h| {
        crate::tui::conversation::bind_layer_driver(Arc::clone(h), initial_prompt.clone())
    });
    // Deconstruct the binding into independent handles. The event receiver is
    // single-consumer (one pump), so it is NOT held on the layer — it lives
    // only here and moves into the main pump at mount. The layer carries the
    // sender (dropping it ends the pump), the command sender (close sends
    // Shutdown), the unsubscribe token, and a harness clone (defensive abort).
    use crate::tui::conversation::LayerBinding;
    let (main_harness, main_cmd_tx, main_event_tx, main_event_rx, main_unsubscribe) =
        match main_binding {
            Some(LayerBinding {
                harness,
                cmd_tx,
                event_tx,
                event_rx,
                unsubscribe,
            }) => (
                Some(harness),
                Some(cmd_tx),
                Some(event_tx),
                Some(event_rx),
                Some(unsubscribe),
            ),
            None => (None, None, None, None, None),
        };

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

    // Build the overlap factory (async — model/system_prompt are async getters)
    // so extension overlaps can fork a fresh harness from the main session's
    // recipe. Only when a harness exists; session-only mode leaves it None.
    let overlap_factory: Option<Arc<crate::tui::conversation::AgentOverlapFactory>> = match &harness
    {
        Some(h) => {
            let model = h.model().await;
            let system_prompt = h.system_prompt().await;
            // built_in_tools already extracted for the extension layer; reuse
            // the harness's current tool set as the overlap recipe.
            let tools = h.tools();
            // api_key_fn is private on the harness; build_agent captured the
            // key closure separately. Reconstruct from the key we have.
            let api_key_for_factory = api_key.clone();
            let api_key_fn: flown_agent::GetApiKeyAndHeadersFn =
                std::sync::Arc::new(move |_model: &flown_ai::Model| {
                    api_key_for_factory.clone().map(|k| (k, None))
                });
            Some(Arc::new(crate::tui::conversation::AgentOverlapFactory {
                model,
                env: std::sync::Arc::new(crate::native_env::NativeExecutionEnv::new()),
                built_in_tools: tools,
                system_prompt,
                api_key_fn,
                main_harness: Arc::clone(h),
            }))
        }
        None => None,
    };

    // Build the iodilos renderer. From here on the main thread is owned by
    // iodilos's event loop until quit.
    let mut renderer = Renderer::new()?;

    // Captures for the mount closure: the binding parts (harness, cmd sender,
    // event sender, unsubscribe), the event receiver (for the pump), the status
    // snapshot, and the extension command registry. `status` seeds the initial
    // snapshot; `mount_command_table` is bound to the UiState sink inside the
    // mount closure (where Rc<UiState> exists).
    let mount_harness = main_harness;
    let mount_status = status.clone();
    let mount_busy = initial_busy;
    let mount_event_tx = main_event_tx;
    let mount_unsubscribe = std::cell::Cell::new(main_unsubscribe);
    let mount_config = config.clone();
    let mut mount_command_table = command_table;
    let mount_overlap_factory = overlap_factory.clone();
    let mount_cmd_tx = main_cmd_tx;
    let mount_event_rx = main_event_rx;

    renderer.mount(move || {
        // The shared reactive state, seeded with the initial status + busy flag.
        let state = Rc::new(UiState::new(TextAreaState::default()));
        state.status.update(|s| *s = mount_status.clone());
        state.busy.set(mount_busy);

        // The main harness binding (channel + subscriber + driver) was created
        // before mount in `bind_layer_driver`. Its event receiver feeds the main
        // pump spawned below; its sender + unsubscribe ride on the main layer.
        // Build the conversation stack. The main layer wraps the state, the
        // main harness binding's sender + unsubscribe, and the driver's command
        // sender; its senders stay alive for the app's lifetime (main is never
        // popped). Overlap layers are pushed by RuntimeControl::open_overlap.
        let main_layer = crate::tui::conversation::ConversationLayer {
            kind: crate::tui::conversation::LayerKind::Main,
            overlap: None,
            state: Rc::clone(&state),
            harness: mount_harness.clone(),
            event_tx: mount_event_tx
                .clone()
                .expect("main event_tx present when harness exists; binding created before mount"),
            unsubscribe: mount_unsubscribe.take(),
            cmd_tx: mount_cmd_tx.clone(),
        };
        let stack = crate::tui::conversation::ConversationStack::new(
            main_layer,
            mount_overlap_factory.clone(),
        );

        // RuntimeControl is the iodilos-side command interpreter behind the
        // extension-facing RuntimeCommandProxy. Built here (not during
        // register, which runs on tokio) because it holds the Rc<ConversationStack>
        // and the OverlayStack (both !Send). The overlay stack is provided as
        // context so App can render the active overlay on top of the main UI.
        let overlay_stack = crate::tui::overlay_stack::OverlayStack::new();
        let runtime_control = crate::tui::conversation::RuntimeControl::new(
            Rc::clone(&stack),
            Rc::clone(&overlay_stack),
            mount_harness.clone(),
            mount_config.clone(),
        );
        let (runtime_command_tx, runtime_command_rx) = flume::unbounded();
        let runtime_proxy = Arc::new(crate::core::extensions::RuntimeCommandProxy::new(
            runtime_command_tx,
        ));
        spawn_runtime_command_pump(Rc::clone(&runtime_control), runtime_command_rx);
        provide_context(Rc::clone(&overlay_stack));

        // Bind the extension command table to the live stack, producing the
        // dispatch-capable CommandSide. Commands receive an ExtensionContext
        // backed by RuntimeCommandProxy, so UI/conversation actions target the
        // active layer without exposing UiState to extensions.
        let command_side: Option<Rc<crate::core::extensions::CommandSide>> = mount_command_table
            .take()
            .map(|table| Rc::new(table.bind(Arc::clone(&runtime_proxy))));
        provide_context(command_side);

        // Provide the conversation stack (not a bare UiState) + handles. App
        // and components read `stack.active().state`. The event pump captures
        // the main state by move (it cannot use_context — no active owner).
        provide_context(Rc::clone(&stack));
        provide_context(mount_harness.clone());
        provide_context(mount_config.clone());

        // The event pump: drain the main channel and translate each event into
        // main UiState mutations. Overlap layers spawn their own identical pump
        // in RuntimeControl::open_overlap.
        let pump_state = Rc::clone(&state);
        // The main pump consumes the binding's event_rx (single-consumer). Only
        // spawn it when a harness (and thus a receiver) exists; in session-only
        // mode there are no events to pump.
        if let Some(pump_rx) = mount_event_rx {
            spawn_local(async move {
                let mut accumulated_text = String::new();
                let mut in_thinking = false;
                while let Ok(event) = pump_rx.recv_async().await {
                    translate_event(event, &pump_state, &mut accumulated_text, &mut in_thinking);
                }
            });
        }

        // Busy-spinner tick: advance the spinner frame while the active layer's
        // agent is running. Registered once at mount; reads the stack so it
        // tracks whichever view is visible.
        let spinner_stack = Rc::clone(&stack);
        every(std::time::Duration::from_millis(500), move || {
            let active = spinner_stack.active();
            if active.state.busy.get() {
                active.state.status.update(|s| {
                    s.frame = (s.frame + 1) % BUSY_FRAMES.len();
                });
            } else {
                // Reset the frame when idle so the spinner resumes from the top.
                active.state.status.update(|s| s.frame = 0);
            }
        });

        view! { App() }
    });

    renderer.run().await?;
    Ok(())
}

fn spawn_runtime_command_pump(
    runtime_control: Rc<crate::tui::conversation::RuntimeControl>,
    rx: flume::Receiver<crate::core::extensions::RuntimeCommand>,
) {
    spawn_local(async move {
        while let Ok(command) = rx.recv_async().await {
            use crate::core::extensions::RuntimeCommand;
            match command {
                RuntimeCommand::OpenOverlap { options, reply } => {
                    runtime_control.open_overlap(options);
                    let _ = reply.send(Ok(()));
                }
                RuntimeCommand::CloseActiveOverlap { reply } => {
                    runtime_control.close_active_overlap();
                    let _ = reply.send(Ok(()));
                }
                RuntimeCommand::SendToActive { text, reply } => {
                    runtime_control.send_to_active(text);
                    let _ = reply.send(Ok(()));
                }
                RuntimeCommand::NotifyActive { text } => {
                    runtime_control.notify_active(text);
                }
                RuntimeCommand::NotifyErrorActive { text } => {
                    runtime_control.notify_error_active(text);
                }
                RuntimeCommand::ClearActive => {
                    runtime_control.clear_active();
                }
                RuntimeCommand::ForkConversation { prompt, reply } => {
                    runtime_control.fork_conversation(prompt, reply);
                }
                RuntimeCommand::OpenModelOverlay { reply } => {
                    runtime_control.handle_open_model_overlay(reply);
                }
            }
        }
    });
}

/// Translate one `AgentHarnessEvent` into `UiState` mutations. A near-verbatim port
/// of the old `interactive_mode.rs` event-match block, but every `transcript.*`
/// call becomes a `state.*` call. Persistence is now owned by the harness
/// (MessageEnd/TurnEnd → session.append_message), so this function no longer
/// ships messages to a separate persistence task.
pub(crate) fn translate_event(
    event: AgentHarnessEvent,
    state: &UiState,
    accumulated_text: &mut String,
    in_thinking: &mut bool,
) {
    use flown_ai::AssistantMessageEvent;

    match event {
        AgentHarnessEvent::AgentStart => {}

        AgentHarnessEvent::MessageUpdate {
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
            AssistantMessageEvent::Done { message, .. }
                if !assistant_is_waiting_for_tool(&message) =>
            {
                accumulated_text.clear();
                *in_thinking = false;
                state.busy.set(false);
                state.status.update(|s| s.busy = false);
            }
            AssistantMessageEvent::Error { .. } => {
                accumulated_text.clear();
                *in_thinking = false;
                state.busy.set(false);
                state.status.update(|s| s.busy = false);
            }
            _ => {}
        },

        AgentHarnessEvent::MessageEnd { message } => match &message {
            flown_agent::AgentMessage::Assistant(message)
                if !assistant_is_waiting_for_tool(message) =>
            {
                // The finalized assistant message is persisted by the harness.
                // Reset the streaming accumulator for the next message. A
                // ToolUse stop reason is still active work, so it is cleared by
                // AgentEnd/Abort/Settled instead.
                accumulated_text.clear();
                *in_thinking = false;
                state.busy.set(false);
                state.status.update(|s| s.busy = false);
            }
            flown_agent::AgentMessage::ToolResult(result) if result.is_error => {
                let output: String = result
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        flown_ai::ToolResultContent::Text(t) => Some(t.text.as_str()),
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

        AgentHarnessEvent::ToolExecutionStart {
            tool_name, args, ..
        } => {
            state.push_tool_call(&tool_name, &args);
        }

        AgentHarnessEvent::ToolExecutionEnd {
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

        AgentHarnessEvent::TurnEnd {
            message: flown_agent::AgentMessage::Assistant(message),
            ..
        } if !assistant_is_waiting_for_tool(&message) => {
            accumulated_text.clear();
            *in_thinking = false;
            state.busy.set(false);
            state.status.update(|s| s.busy = false);
        }

        AgentHarnessEvent::AgentEnd { .. }
        | AgentHarnessEvent::Abort { .. }
        | AgentHarnessEvent::Settled { .. } => {
            accumulated_text.clear();
            *in_thinking = false;
            state.busy.set(false);
            state.status.update(|s| s.busy = false);
        }

        AgentHarnessEvent::ModelUpdate { model, .. } => {
            state.status.update(|s| {
                s.model = format!("{}/{}", model.provider, model.id);
                s.provider = model.provider.to_string();
            });
        }
        AgentHarnessEvent::ThinkingLevelUpdate { level, .. } => {
            state.status.update(|s| {
                s.thinking_level = format!("{:?}", level).to_lowercase();
            });
        }

        _ => {}
    }
}

fn assistant_is_waiting_for_tool(message: &flown_ai::AssistantMessage) -> bool {
    matches!(message.stop_reason, flown_ai::StopReason::ToolUse)
}

#[cfg(test)]
mod tests {
    use super::*;

    use flown_agent::AgentMessage;
    use flown_ai::{
        Api, AssistantContent, AssistantMessage, AssistantMessageEvent, KnownApi, KnownProvider,
        Provider, StopReason, TextContent, ToolCall, Usage,
    };
    use iodilos::prelude::TextAreaState;

    fn assistant(content: Vec<AssistantContent>, stop_reason: StopReason) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content,
            api: Api::Known(KnownApi::OpenAiCompletions),
            provider: Provider::Known(KnownProvider::OpenAi),
            model: "test-model".to_string(),
            response_model: None,
            response_id: None,
            usage: Usage::default(),
            stop_reason,
            error_message: None,
            diagnostics: None,
            timestamp: chrono::Utc::now(),
        }
    }

    fn assistant_message(content: Vec<AssistantContent>, stop_reason: StopReason) -> AgentMessage {
        AgentMessage::Assistant(assistant(content, stop_reason))
    }

    fn busy_state() -> UiState {
        let state = UiState::new(TextAreaState::default());
        state.busy.set(true);
        state.status.update(|status| status.busy = true);
        state
    }

    #[test]
    fn assistant_message_end_clears_busy_for_final_text() {
        let state = busy_state();
        let mut accumulated_text = "partial".to_string();
        let mut in_thinking = true;

        translate_event(
            AgentHarnessEvent::MessageEnd {
                message: assistant_message(
                    vec![AssistantContent::Text(TextContent {
                        content_type: "text".to_string(),
                        text: "done".to_string(),
                        text_signature: None,
                    })],
                    StopReason::Stop,
                ),
            },
            &state,
            &mut accumulated_text,
            &mut in_thinking,
        );

        assert!(!state.busy.get());
        assert!(!state.status.get().busy);
        assert!(accumulated_text.is_empty());
        assert!(!in_thinking);
    }

    #[test]
    fn assistant_message_end_keeps_busy_for_tool_call() {
        let state = busy_state();
        let mut accumulated_text = "partial".to_string();
        let mut in_thinking = true;

        translate_event(
            AgentHarnessEvent::MessageEnd {
                message: assistant_message(
                    vec![AssistantContent::ToolCall(ToolCall {
                        content_type: "toolCall".to_string(),
                        id: "call-1".to_string(),
                        name: "bash".to_string(),
                        arguments: serde_json::json!({}),
                        thought_signature: None,
                    })],
                    StopReason::ToolUse,
                ),
            },
            &state,
            &mut accumulated_text,
            &mut in_thinking,
        );

        assert!(state.busy.get());
        assert!(state.status.get().busy);
        assert_eq!(accumulated_text, "partial");
        assert!(in_thinking);
    }

    #[test]
    fn assistant_done_clears_busy_for_final_text() {
        let state = busy_state();
        let mut accumulated_text = "partial".to_string();
        let mut in_thinking = true;

        translate_event(
            AgentHarnessEvent::MessageUpdate {
                message: assistant_message(Vec::new(), StopReason::Stop),
                assistant_message_event: AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: assistant(
                        vec![AssistantContent::Text(TextContent {
                            content_type: "text".to_string(),
                            text: "done".to_string(),
                            text_signature: None,
                        })],
                        StopReason::Stop,
                    ),
                },
            },
            &state,
            &mut accumulated_text,
            &mut in_thinking,
        );

        assert!(!state.busy.get());
        assert!(!state.status.get().busy);
        assert!(accumulated_text.is_empty());
        assert!(!in_thinking);
    }

    #[test]
    fn assistant_done_keeps_busy_for_tool_call() {
        let state = busy_state();
        let mut accumulated_text = "partial".to_string();
        let mut in_thinking = true;

        translate_event(
            AgentHarnessEvent::MessageUpdate {
                message: assistant_message(Vec::new(), StopReason::ToolUse),
                assistant_message_event: AssistantMessageEvent::Done {
                    reason: StopReason::ToolUse,
                    message: assistant(
                        vec![AssistantContent::ToolCall(ToolCall {
                            content_type: "toolCall".to_string(),
                            id: "call-1".to_string(),
                            name: "bash".to_string(),
                            arguments: serde_json::json!({}),
                            thought_signature: None,
                        })],
                        StopReason::ToolUse,
                    ),
                },
            },
            &state,
            &mut accumulated_text,
            &mut in_thinking,
        );

        assert!(state.busy.get());
        assert!(state.status.get().busy);
        assert_eq!(accumulated_text, "partial");
        assert!(in_thinking);
    }

    #[test]
    fn assistant_message_end_stop_reason_decides_finality() {
        let state = busy_state();
        let mut accumulated_text = "partial".to_string();
        let mut in_thinking = true;

        translate_event(
            AgentHarnessEvent::MessageEnd {
                message: assistant_message(
                    vec![
                        AssistantContent::Text(TextContent {
                            content_type: "text".to_string(),
                            text: "done".to_string(),
                            text_signature: None,
                        }),
                        AssistantContent::ToolCall(ToolCall {
                            content_type: "toolCall".to_string(),
                            id: "call-1".to_string(),
                            name: "bash".to_string(),
                            arguments: serde_json::json!({}),
                            thought_signature: None,
                        }),
                    ],
                    StopReason::Stop,
                ),
            },
            &state,
            &mut accumulated_text,
            &mut in_thinking,
        );

        assert!(!state.busy.get());
        assert!(!state.status.get().busy);
        assert!(accumulated_text.is_empty());
        assert!(!in_thinking);
    }

    #[test]
    fn final_turn_end_clears_busy() {
        let state = busy_state();
        let mut accumulated_text = "partial".to_string();
        let mut in_thinking = true;

        translate_event(
            AgentHarnessEvent::TurnEnd {
                message: assistant_message(
                    vec![AssistantContent::Text(TextContent {
                        content_type: "text".to_string(),
                        text: "done".to_string(),
                        text_signature: None,
                    })],
                    StopReason::Stop,
                ),
                tool_results: Vec::new(),
            },
            &state,
            &mut accumulated_text,
            &mut in_thinking,
        );

        assert!(!state.busy.get());
        assert!(!state.status.get().busy);
        assert!(accumulated_text.is_empty());
        assert!(!in_thinking);
    }

    #[test]
    fn model_update_syncs_status_model_and_provider() {
        // After a ModelUpdate event, the status snapshot's `model` carries the
        // model id and `provider` is set from the model's provider. Today this
        // is discarded by the `_ => {}` fallthrough in translate_event.
        let state = UiState::new(TextAreaState::default());
        let mut acc = String::new();
        let mut thinking = false;
        let model: flown_ai::Model = serde_json::from_str(
            r#"{"id":"glm-5.1","name":"GLM 5.1","api":"openai-completions","provider":"openrouter","baseUrl":"","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":1,"maxTokens":1}"#,
        )
        .expect("minimal Model JSON parses");
        translate_event(
            AgentHarnessEvent::ModelUpdate {
                model: model.clone(),
                previous_model: None,
                source: flown_agent::ModelUpdateSource::Set,
            },
            &state,
            &mut acc,
            &mut thinking,
        );
        let snap = state.status.get();
        assert!(
            snap.model.contains("glm-5.1"),
            "status.model should contain the model id, got {}",
            snap.model
        );
        // Provider::Display for Known(OpenRouter) is lowercase "openrouter".
        assert_eq!(snap.provider, "openrouter");
    }

    #[test]
    fn thinking_level_update_syncs_status_thinking_level() {
        let state = UiState::new(TextAreaState::default());
        let mut acc = String::new();
        let mut thinking = false;
        translate_event(
            AgentHarnessEvent::ThinkingLevelUpdate {
                level: flown_ai::ThinkingLevel::High,
                previous_level: flown_ai::ThinkingLevel::Off,
            },
            &state,
            &mut acc,
            &mut thinking,
        );
        let snap = state.status.get();
        assert!(
            snap.thinking_level.contains("high"),
            "status.thinking_level should reflect the new level, got {}",
            snap.thinking_level
        );
    }
}

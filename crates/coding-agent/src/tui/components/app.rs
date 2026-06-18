//! App — the root component.
//!
//! Owns the vertical layout (transcript / status line / editor / hint bar) and
//! the global `on_key` router. Reads the shared [`UiState`] plus the agent
//! handle, event sender, and persistence sender from iodilos context.
//!
//! Key handling: the editor's `handle_key` composes iodilos `TextAreaState`
//! with agent-specific slash completion, then App reacts to `Submit`.

use std::rc::Rc;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use iodilos::prelude::*;

use crate::core::extensions::SlashCommandScope;
use crate::tui::components::editor::InputEditor;
use crate::tui::components::status_line::StatusLine;
use crate::tui::components::transcript::Transcript;
// The `view!` macro expands `Transcript()` into
// `Transcript::new(TranscriptProps { … })`, so the generated prop structs must
// be in scope at the call site.
use crate::tui::components::editor::InputEditorProps;
use crate::tui::components::status_line::StatusLineProps;
use crate::tui::components::transcript::TranscriptProps;
use crate::tui::editor::{self, EditorAction};
use crate::tui::state::UiState;
use flown_agent::AgentHarness;

#[component]
pub fn App() -> Node {
    let stack = use_context::<Rc<crate::tui::conversation::ConversationStack>>();
    let agent = use_context::<Option<Arc<AgentHarness>>>();
    let config = use_context::<crate::config::Config>();
    let overlay_stack = use_context::<Rc<crate::tui::overlay_stack::OverlayStack>>();

    // The global key router. Runs under this component's owner (registered at
    // mount); captures the handles by move.
    let key_stack = Rc::clone(&stack);
    let key_overlay = Rc::clone(&overlay_stack);
    let key_agent = agent;
    let key_config = config;
    on_key(move |key: KeyEvent| -> bool {
        handle_app_key(key, &key_stack, &key_overlay, key_agent.as_ref(), &key_config)
    });

    view! {
        View(
            flex_direction: FlexDirection::Column,
            width_percent: 100.0,
            height_percent: 100.0,
            background: Color::Reset,
        ) {
            Transcript()
            StatusLine()
            InputEditor()
            OverlayLayer(overlay: overlay_stack)
        }
    }
}

/// Renders the active overlay (if any) on top of the main UI. Reads the
/// OverlayStack's active signal so it re-runs when an overlay is pushed/popped.
/// Built as a `#[component]` child of the root View so it participates in the
/// layout tree as the last (topmost) sibling.
#[component]
fn OverlayLayer(overlay: Rc<crate::tui::overlay_stack::OverlayStack>) -> Node {
    // Track the active overlay reactively. `active_signal` is an RwSignal; reading
    // it inside an effect registers the dependency. We render the OverlayBox only
    // while an overlay is present.
    let active = overlay.active_signal();
    let host = Node::new_view();
    let host_for_effect = host.clone();
    create_effect(move || {
        let current = active.get();
        match current {
            Some(o) => {
                let geometry = o.geometry;
                let content = (o.content)();
                let props = iodilos::OverlayBoxProps {
                    geometry,
                    background: Color::Reset,
                    border: Borders::ALL,
                    border_style: iodilos::BorderStyle::Round,
                    border_color: Color::Rgb(80, 80, 96),
                    content,
                };
                host_for_effect.set_children(vec![iodilos::OverlayBox::new(props)]);
            }
            None => {
                host_for_effect.set_children(vec![]);
            }
        }
    });
    host
}

/// The global key router. Returns `true` if the key was consumed. Operates on
/// the conversation stack's **active** layer, so all input/streaming targets
/// whichever view is visible.
fn handle_app_key(
    key: KeyEvent,
    stack: &Rc<crate::tui::conversation::ConversationStack>,
    overlay_stack: &Rc<crate::tui::overlay_stack::OverlayStack>,
    _agent: Option<&Arc<AgentHarness>>,
    config: &crate::config::Config,
) -> bool {
    let state = Rc::clone(&stack.active().state);

    // Esc cancels the current transient state in priority order:
    //   1. slash popup open  → close it, stay in input
    //   2. input non-empty   → clear it
    //   3. agent running     → abort the turn
    //   4. idle + empty      → no-op (Esc never quits; use /quit or Ctrl-C)
    if key.code == KeyCode::Esc {
        if state.slash_popup.with(|p| p.is_some()) {
            state.slash_popup.set(None);
            return true;
        }
        let has_input = state.input.with(|input| !input.is_empty());
        if has_input {
            state.input.update(|input| input.clear());
            return true;
        }
        if state.busy.get() {
            state.busy.set(false);
            state.status.update(|s| s.busy = false);
            return true;
        }
        return true;
    }
    // Ctrl-Q always quits.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
        iodilos::quit();
        return true;
    }
    // Ctrl-C: an open overlay (model picker / btw fork) is closed first. With
    // empty input, close a dismissible/pending overlap. Otherwise clear
    // non-empty input, or quit the app (main layer, empty input).
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        if overlay_stack.is_active() {
            tracing::info!(target: "flown::overlay", "Ctrl+C with active overlay, popping it");
            overlay_stack.pop();
            return true;
        }
        let has_input = state.input.with(|input| !input.is_empty());
        let has_overlap = stack.overlap_is_active_or_pending();
        tracing::info!(target: "flown::overlap", has_input, has_overlap, "Ctrl+C received");
        if has_input {
            state.input.update(|input| input.clear());
            state.slash_popup.set(None);
        } else if has_overlap {
            // CommandSide holds the bound runtime command proxy; expose overlap
            // close through it so App stays independent of extension-specific
            // logic.
            let command_side = use_context::<Option<Rc<crate::core::extensions::CommandSide>>>();
            tracing::info!(target: "flown::overlap", has_side = command_side.is_some(), "Ctrl+C in overlap, dispatching close");
            if let Some(cs) = command_side.as_ref() {
                cs.close_active_overlap();
            }
        } else {
            tracing::info!(target: "flown::overlap", "Ctrl+C in main, quitting app");
            iodilos::quit();
        }
        return true;
    }
    if key.code == KeyCode::PageUp {
        state.scroll_up(10);
        return true;
    }
    if key.code == KeyCode::PageDown {
        state.scroll_down(10);
        return true;
    }

    // Route everything else to the editor glue.
    // Build the merged command view for autocomplete when the active surface
    // participates in global slash commands.
    let command_side = use_context::<Option<Rc<crate::core::extensions::CommandSide>>>();
    let slash_commands_enabled = stack.active_slash_command_scope() == SlashCommandScope::Global;
    let mut commands = Vec::new();
    if slash_commands_enabled {
        commands = crate::tui::slash_commands::static_command_entries();
        if let Some(cs) = command_side.as_ref() {
            for cmd in cs.commands() {
                commands.push(crate::tui::slash_commands::CommandEntry {
                    name: cmd.name.clone(),
                    description: cmd.meta.description.clone(),
                    subcommands: cmd
                        .meta
                        .subcommands
                        .iter()
                        .map(|s| crate::tui::slash_commands::SubcommandEntry {
                            name: s.name.clone(),
                            description: s.description.clone(),
                        })
                        .collect(),
                });
            }
        }
    }
    let mut input = state.input.get();
    let mut popup = state.slash_popup.get();
    let action = editor::handle_key(
        &mut input,
        &mut popup,
        key,
        config,
        &commands,
        slash_commands_enabled,
    );
    state.input.set(input);
    state.slash_popup.set(popup);

    match action {
        EditorAction::Submit => {
            let text = state.input.with(|es| es.text());
            let text = text.trim().to_string();
            if text.is_empty() {
                return true;
            }
            state.input.update(|input| input.clear());
            state.slash_popup.set(None);

            let skill_invocation = slash_commands_enabled
                .then(|| crate::tui::slash_commands::parse_skill_command(&text))
                .flatten();
            if let Some(inv) = skill_invocation {
                // `/skill:<name> [<request>]` is a parameterized family handled
                // up front: it needs the agent handle to trigger a turn, which
                // the generic slash dispatcher lacks. The transcript shows the
                // raw input line; the model receives the transformed prompt.
                match crate::tui::slash_commands::validate_skill_name(&inv.skill_name, config) {
                    Ok(()) => {
                        if state.busy.get() {
                            return true;
                        }
                        state.push_user(&text);
                        let prompt = crate::tui::slash_commands::build_skill_prompt(&inv);
                        // Route through the MAIN layer's driver (skills run on
                        // the main agent, not whatever overlap is active).
                        let main_layer = stack.main_layer();
                        match main_layer.submit_prompt(prompt) {
                            crate::tui::conversation::SubmitOutcome::Queued => {
                                state.busy.set(true);
                                state.status.update(|s| s.busy = true);
                            }
                            crate::tui::conversation::SubmitOutcome::NoAgent => {
                                state.push_error("No LLM agent available. Check your config.");
                            }
                            crate::tui::conversation::SubmitOutcome::DriverGone => {
                                state.push_error("Layer driver exited. Cannot send prompt.");
                            }
                            crate::tui::conversation::SubmitOutcome::ChannelFull => {
                                state.push_error("Layer driver busy (command queue full).");
                            }
                        }
                    }
                    Err(available) => {
                        if available.is_empty() {
                            state.push_error(format!(
                                "Skill '{}' not found. No skills are installed. Use /skills for help.",
                                inv.skill_name
                            ));
                        } else {
                            state.push_error(format!(
                                "Skill '{}' not found. Available: {}",
                                inv.skill_name,
                                available.join(", ")
                            ));
                        }
                    }
                }
            } else if slash_commands_enabled && text.starts_with('/') {
                // Extension commands get first crack at dispatch. CommandSide
                // runs async handlers with an ExtensionContext backed by the
                // runtime command proxy.
                if let Some(cs) = command_side.as_ref()
                    && cs.dispatch(&text)
                {
                    return true;
                }
                let mut handle: Rc<UiState> = Rc::clone(&state);
                let should_quit = crate::tui::slash_commands::handle_slash_command(
                    &text,
                    &mut handle,
                    config,
                    &commands,
                );
                if should_quit {
                    iodilos::quit();
                }
            } else {
                // Guard against a second submit while an agent turn is running.
                // The harness enforces single-flight via phase==Idle (assert_idle),
                // so a concurrent prompt() would return HarnessError::Busy silently.
                if state.busy.get() {
                    return true;
                }
                state.push_user(&text);
                // Submit through the ACTIVE layer's driver command channel.
                // The driver (a per-layer tokio task) awaits prompt() inline —
                // no per-prompt tokio::spawn. try_send is non-blocking, so this
                // is safe to call from the iodilos on_key thread.
                let active_layer = stack.active();
                let active_kind = active_layer.kind;
                tracing::info!(
                    target: "flown::prompt",
                    layer = ?active_kind,
                    text_len = text.len(),
                    "ui prompt submitted"
                );
                match active_layer.submit_prompt(text) {
                    crate::tui::conversation::SubmitOutcome::Queued => {
                        state.busy.set(true);
                        state.status.update(|s| s.busy = true);
                    }
                    crate::tui::conversation::SubmitOutcome::NoAgent => {
                        state.push_error("No LLM agent available. Check your config.");
                    }
                    crate::tui::conversation::SubmitOutcome::DriverGone => {
                        state.push_error("Layer driver exited. Cannot send prompt.");
                    }
                    crate::tui::conversation::SubmitOutcome::ChannelFull => {
                        state.push_error("Layer driver busy (command queue full).");
                    }
                }
            }
            true
        }
        EditorAction::None => true,
    }
}

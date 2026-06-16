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

use crate::tui::components::editor::InputEditor;
use crate::tui::components::hint_bar::HintBar;
use crate::tui::components::status_line::StatusLine;
use crate::tui::components::transcript::Transcript;
// The `view!` macro expands `Transcript()` into
// `Transcript::new(TranscriptProps { … })`, so the generated prop structs must
// be in scope at the call site.
use crate::tui::components::editor::InputEditorProps;
use crate::tui::components::hint_bar::HintBarProps;
use crate::tui::components::status_line::StatusLineProps;
use crate::tui::components::transcript::TranscriptProps;
use crate::tui::editor::{self, EditorAction};
use crate::tui::state::UiState;
use flown_agent::harness::AgentHarness;

#[component]
pub fn App() -> Node {
    let stack = use_context::<Rc<crate::tui::conversation::ConversationStack>>();
    let agent = use_context::<Option<Arc<AgentHarness>>>();
    let config = use_context::<crate::config::Config>();

    // The global key router. Runs under this component's owner (registered at
    // mount); captures the handles by move.
    let key_stack = Rc::clone(&stack);
    let key_agent = agent;
    let key_config = config;
    on_key(move |key: KeyEvent| -> bool {
        handle_app_key(key, &key_stack, key_agent.as_ref(), &key_config)
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
            HintBar()
        }
    }
}

/// The global key router. Returns `true` if the key was consumed. Operates on
/// the conversation stack's **active** layer, so all input/streaming targets
/// whichever view is visible (main or btw).
fn handle_app_key(
    key: KeyEvent,
    stack: &Rc<crate::tui::conversation::ConversationStack>,
    agent: Option<&Arc<AgentHarness>>,
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
    // Ctrl-C: in a btw layer with empty input, exit btw (discard). Otherwise
    // clear non-empty input, or quit the app (main layer, empty input).
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        let has_input = state.input.with(|input| !input.is_empty());
        if has_input {
            state.input.update(|input| input.clear());
            state.slash_popup.set(None);
        } else if stack.active_is_btw() {
            // In a btw layer: Ctrl+C exits btw (discards it). CommandSide
            // holds the bound RuntimeControl; expose exit through it.
            let command_side = use_context::<Option<Rc<crate::core::extensions::CommandSide>>>();
            if let Some(cs) = command_side.as_ref() {
                cs.exit_active_btw();
            }
        } else {
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
    // Build the merged command view for autocomplete: static commands first,
    // then extension-registered commands (e.g. /mcp, /btw) appended in
    // registration order. The popup captures a snapshot of this each time it
    // opens.
    let command_side = use_context::<Option<Rc<crate::core::extensions::CommandSide>>>();
    let mut commands = crate::tui::slash_commands::static_command_entries();
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
    let mut input = state.input.get();
    let mut popup = state.slash_popup.get();
    let action = editor::handle_key(&mut input, &mut popup, key, config, &commands);
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

            if let Some(inv) = crate::tui::slash_commands::parse_skill_command(&text) {
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
                        if let Some(agent) = agent {
                            state.busy.set(true);
                            state.status.update(|s| s.busy = true);
                            let agent = Arc::clone(agent);
                            let prompt = crate::tui::slash_commands::build_skill_prompt(&inv);
                            tokio::spawn(async move {
                                let _ = agent.prompt(&prompt, None).await;
                            });
                        } else {
                            state.push_error("No LLM agent available. Check your config.");
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
            } else if text.starts_with('/') {
                // Extension commands (e.g. /mcp, /btw) get first crack at
                // dispatch. CommandSide routes control commands (/btw) to the
                // bound RuntimeControl and effect commands (/mcp) to their
                // handler. If it claims the line, skip the static path.
                let command_side =
                    use_context::<Option<Rc<crate::core::extensions::CommandSide>>>();
                if let Some(cs) = command_side.as_ref() {
                    if cs.dispatch(&text) {
                        return true;
                    }
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
                // Use the ACTIVE layer's harness (may differ from the main
                // agent handle when in a btw layer). Fall back to the main
                // agent only when the active layer has no harness.
                let active_layer = stack.active();
                let target = active_layer.harness.as_ref().or(agent);
                if let Some(agent) = target {
                    state.busy.set(true);
                    state.status.update(|s| s.busy = true);
                    let agent = Arc::clone(agent);
                    let prompt = text;
                    // Spawn the harness driver on tokio. We're on the iodilos
                    // thread; tokio::spawn hands the future to the runtime.
                    // Events arrive via the subscriber registered for this
                    // layer (runtime.rs for main; enter_btw for btw).
                    tokio::spawn(async move {
                        let _ = agent.prompt(&prompt, None).await;
                    });
                } else {
                    state.push_error("No LLM agent available. Check your config.");
                }
            }
            true
        }
        EditorAction::None => true,
    }
}

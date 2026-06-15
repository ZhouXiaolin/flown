//! App — the root component.
//!
//! Owns the vertical layout (transcript / status line / editor / hint bar) and
//! the global `on_key` router. Reads the shared [`UiState`] plus the agent
//! handle, event sender, and persistence sender from iodilos context.
//!
//! Key handling: the editor's `handle_key` is a pure function over
//! `EditorState`; the App calls it (capturing the result via a `Cell` since
//! `RwSignal::update` returns `()`), writes the result back to the `input`
//! signal, and reacts to `Submit` (spawn agent prompt + ship a persistence
//! request). `Esc` aborts a running agent or quits when idle; `Ctrl-Q` always
//! quits. The full cursor-provider editor rendering lands in Phase 3.

use std::cell::Cell;
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
use crate::tui::editor::EditorAction;
use crate::tui::runtime::PersistReq;
use crate::tui::state::UiState;

#[component]
pub fn App() -> Node {
    let state = use_context::<Rc<UiState>>();
    let agent = use_context::<Option<Arc<flown_agent::Agent>>>();
    let event_tx = use_context::<flume::Sender<flown_agent::AgentEvent>>();
    let persist_tx = use_context::<flume::Sender<PersistReq>>();
    let config = use_context::<crate::config::Config>();

    // The global key router. Runs under this component's owner (registered at
    // mount); captures the handles by move.
    let key_state = Rc::clone(&state);
    let key_agent = agent;
    let key_event_tx = event_tx;
    let key_persist_tx = persist_tx;
    let key_config = config;
    on_key(move |key: KeyEvent| -> bool {
        handle_app_key(
            key,
            &key_state,
            key_agent.as_ref(),
            &key_event_tx,
            &key_persist_tx,
            &key_config,
        )
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

/// The global key router. Returns `true` if the key was consumed.
fn handle_app_key(
    key: KeyEvent,
    state: &Rc<UiState>,
    agent: Option<&Arc<flown_agent::Agent>>,
    event_tx: &flume::Sender<flown_agent::AgentEvent>,
    persist_tx: &flume::Sender<PersistReq>,
    config: &crate::config::Config,
) -> bool {
    // Esc: abort a running agent, or quit when idle.
    if key.code == KeyCode::Esc {
        if state.busy.get() {
            state.busy.set(false);
            state.status.update(|s| s.busy = false);
            return true;
        } else {
            iodilos::quit();
            return true;
        }
    }
    // Ctrl-Q always quits.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
        iodilos::quit();
        return true;
    }
    // Ctrl-C clears a non-empty input first; when the editor is already empty it
    // falls back to application exit.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        let has_input = state
            .input
            .with(|es| es.lines.iter().any(|line| !line.is_empty()));
        if has_input {
            state.input.update(|es| es.clear());
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

    // Route everything else to the editor. `update` returns `()`, so capture
    // the action via a Cell shared into the closure.
    let action_cell = Cell::new(EditorAction::None);
    let action_ref = &action_cell;
    state.input.update(|es| {
        action_ref.set(es.handle_key(key));
    });
    let action = action_cell.get();

    match action {
        EditorAction::Submit => {
            let text = state.input.with(|es| es.text());
            let text = text.trim().to_string();
            if text.is_empty() {
                return true;
            }
            state.input.update(|es| es.clear());

            if text.starts_with('/') {
                let mut handle: Rc<UiState> = Rc::clone(state);
                let should_quit =
                    crate::slash_commands::handle_slash_command(&text, &mut handle, config);
                if should_quit {
                    iodilos::quit();
                }
            } else {
                state.push_user(&text);
                let _ = persist_tx.send(PersistReq::User(text.clone()));
                if let Some(agent) = agent {
                    state.busy.set(true);
                    state.status.update(|s| s.busy = true);
                    let agent = Arc::clone(agent);
                    let tx = event_tx.clone();
                    let prompt = text;
                    // Spawn the agent driver on tokio. We're on the iodilos
                    // thread; tokio::spawn hands the future to the runtime.
                    tokio::spawn(async move {
                        let mut stream = agent.prompt(prompt);
                        use futures::stream::StreamExt;
                        while let Some(event) = stream.next().await {
                            if tx.send_async(event).await.is_err() {
                                break;
                            }
                        }
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

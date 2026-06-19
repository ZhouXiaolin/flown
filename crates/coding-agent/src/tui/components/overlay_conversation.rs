//! Full-bleed conversation overlay used by `/btw`.

use std::rc::Rc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use iodilos::prelude::*;

use crate::config::Config;
use crate::tui::components::editor::input_editor_for_state;
use crate::tui::components::status_line::status_line_for_state;
use crate::tui::components::transcript::transcript_for_state;
use crate::tui::editor::{self, EditorAction};
use crate::tui::state::UiState;

pub struct OverlayConversationProps {
    pub state: Rc<UiState>,
    pub badge: Option<String>,
    pub config: Config,
    pub submit: Rc<dyn Fn(String)>,
    pub close: Rc<dyn Fn()>,
}

pub fn overlay_conversation(props: OverlayConversationProps) -> Node {
    let state_for_key = Rc::clone(&props.state);
    let config_for_key = props.config.clone();
    let submit = Rc::clone(&props.submit);
    let close = Rc::clone(&props.close);

    on_key(move |key: KeyEvent| -> bool {
        handle_overlay_key(
            key,
            &state_for_key,
            &config_for_key,
            Rc::clone(&submit),
            Rc::clone(&close),
        )
    });

    let root = Node::new_view();
    root.set_flex_direction(FlexDirection::Column);
    root.set_width_percent(100.0);
    root.set_height_percent(100.0);
    root.set_background(Color::Reset);

    root.set_children(vec![
        transcript_for_state(Rc::clone(&props.state)),
        status_line_for_state(Rc::clone(&props.state), props.badge),
        input_editor_for_state(props.state),
    ]);
    root
}

fn handle_overlay_key(
    key: KeyEvent,
    state: &Rc<UiState>,
    config: &Config,
    submit: Rc<dyn Fn(String)>,
    close: Rc<dyn Fn()>,
) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        close();
        return true;
    }

    if key.code == KeyCode::Esc {
        if state.slash_popup.with(|p| p.is_some()) {
            state.slash_popup.set(None);
            return true;
        }
        if state.input.with(|input| !input.is_empty()) {
            state.input.update(|input| input.clear());
            return true;
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

    let mut input = state.input.get();
    let mut popup = state.slash_popup.get();
    let commands = Vec::new();
    let action = editor::handle_key(&mut input, &mut popup, key, config, &commands, false);
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
            submit(text);
            true
        }
        EditorAction::None => true,
    }
}

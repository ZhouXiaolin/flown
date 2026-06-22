//! Full-bleed conversation overlay used by `/btw`.

use std::rc::Rc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use iodilos::prelude::*;

use crate::config::Config;
use crate::tui::components::editor::input_editor_for_state;
use crate::tui::components::transcript::transcript_for_state;
use crate::tui::editor::{self, EditorAction};
use crate::tui::state::UiState;

pub struct OverlayConversationProps {
    pub state: Rc<UiState>,
    /// Rightmost status-line marker for this overlay's prompt (e.g. "BTW").
    /// Rendered at the tail slot, not as a left-side field.
    pub tail_label: Option<String>,
}

pub fn overlay_conversation(props: OverlayConversationProps) -> View {
    View::from(
        tags::div()
            .flex_direction(FlexDirection::Column)
            .width(Size::Percent(100.0))
            .height(Size::Percent(100.0))
            .background_color(Color::Reset)
            .children((
                transcript_for_state(Rc::clone(&props.state)),
                input_editor_for_state(props.state, props.tail_label),
            )),
    )
}

pub fn handle_overlay_key(
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

    let mut input = state.input.get_clone();
    let mut popup = state.slash_popup.get_clone();
    let commands = Vec::new();
    let action = editor::handle_key(&mut input, &mut popup, key, config, &commands, false);
    state.input.set(input);
    state.slash_popup.set(popup);

    match action {
        EditorAction::Submit => {
            let text = state.input.with(|es| es.text()).trim().to_string();
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

//! Input editor — agent-specific composition around iodilos primitives.

use std::rc::Rc;

use iodilos::prelude::*;

use crate::tui::editor;
use crate::tui::state::UiState;

#[component]
pub fn InputEditor() -> Node {
    let state = use_context::<Rc<UiState>>();
    let input = state.input;
    let slash_popup = state.slash_popup;

    let root = Node::new_view();
    root.set_flex_direction(FlexDirection::Column);
    root.set_width_percent(100.0);

    let popup_items =
        Signal::derive(move || slash_popup.with(|popup| editor::completion_items(popup.as_ref())));
    let popup_selected = Signal::derive(move || {
        slash_popup.with(|popup| popup.as_ref().map_or(0, |popup| popup.selected))
    });

    let mut menu_props = CompletionMenuProps::new(popup_items, popup_selected);
    menu_props.border_color = Color::DarkGray;
    let popup = CompletionMenu::new(menu_props);

    let mut text_props = TextAreaProps::new(input);
    text_props.style = Style::default().fg(Color::White);
    text_props.border_color = Color::Cyan;
    let editor = TextArea::new(text_props);

    let root_for_effect = root.clone();
    let popup_for_effect = popup.clone();
    let editor_for_effect = editor.clone();
    create_effect(move || {
        if slash_popup.with(|popup| popup.is_some()) {
            root_for_effect.set_children(vec![popup_for_effect.clone(), editor_for_effect.clone()]);
        } else {
            root_for_effect.set_children(vec![editor_for_effect.clone()]);
        }
    });

    root
}

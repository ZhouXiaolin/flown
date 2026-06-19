//! Input editor — agent-specific composition around iodilos primitives.
//!
//! Wrapped in a single-element `For` keyed by `active_index`: each layer switch
//! (e.g. overlap push / Ctrl+C pop) disposes the old editor (which was bound to
//! the previous layer's `input`/`slash_popup` signals) and mounts a fresh one
//! bound to the new layer's signals. `TextAreaProps` takes a fixed
//! `RwSignal<TextAreaState>` handle, so it cannot retarget mid-life — `For`
//! gives us the clean mount/dispose cycle the binding model needs.

use std::rc::Rc;

use iodilos::prelude::*;

use crate::tui::editor;
use crate::tui::state::UiState;

#[component]
pub fn InputEditor() -> Node {
    let stack = use_context::<Rc<crate::tui::conversation::ConversationStack>>();
    let active_index = stack.active_index_signal();

    // Drive a 1-element list from active_index. The key IS the index, so any
    // layer switch departs the old key (disposes its editor) and mounts a new
    // one (fresh editor bound to the new layer's signals).
    let each = Signal::derive(move || vec![active_index.get()]);
    For::new(ForProps {
        each,
        key: |i: &usize| *i,
        render: {
            let stack = Rc::clone(&stack);
            move |_idx: Signal<usize>, _i: usize| {
                let state = Rc::clone(&stack.active().state);
                input_editor_for_state(state)
            }
        },
    })
}

pub fn input_editor_for_state(state: Rc<UiState>) -> Node {
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
    let editor_node = TextArea::new(text_props);

    let root_for_effect = root.clone();
    let popup_for_effect = popup.clone();
    let editor_for_effect = editor_node.clone();
    create_effect(move || {
        if slash_popup.with(|popup| popup.is_some()) {
            root_for_effect.set_children(vec![popup_for_effect.clone(), editor_for_effect.clone()]);
        } else {
            root_for_effect.set_children(vec![editor_for_effect.clone()]);
        }
    });

    root
}

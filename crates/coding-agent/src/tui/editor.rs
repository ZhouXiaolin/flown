//! Agent editor glue: slash completion on top of iodilos `TextAreaState`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use iodilos::prelude::{CompletionItem, TextAreaAction, TextAreaState, TextAreaSubmitMode};

use crate::tui::slash_commands::SLASH_COMMANDS;

/// What kind of items the popup is showing.
#[derive(Debug, Clone)]
pub enum PopupKind {
    /// Completing top-level command names (items = indices into SLASH_COMMANDS).
    Command,
    /// Completing subcommands for a specific command.
    Subcommand(usize),
}

#[derive(Debug, Clone)]
pub struct SlashPopup {
    /// Filtered items matching current input (indices depend on kind).
    pub items: Vec<usize>,
    /// Currently selected index in the filtered list.
    pub selected: usize,
    /// What kind of completion is active.
    pub kind: PopupKind,
}

/// Actions the editor can signal to the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorAction {
    None,
    Submit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcceptOutcome {
    None,
    CompletedCommand,
    EnteredSubcommands,
    CompletedSubcommand,
}

pub fn handle_key(
    input: &mut TextAreaState,
    slash_popup: &mut Option<SlashPopup>,
    key: KeyEvent,
) -> EditorAction {
    if slash_popup.is_some() {
        return handle_slash_popup_key(input, slash_popup, key);
    }

    let before = input.text();
    let action = input.handle_key(key, TextAreaSubmitMode::SubmitOnEnter);
    match action {
        TextAreaAction::Submit => EditorAction::Submit,
        TextAreaAction::None => {
            if input.text() != before {
                try_open_slash_popup(input, slash_popup);
            } else if closes_popup(key) {
                *slash_popup = None;
            }
            EditorAction::None
        }
    }
}

pub fn completion_items(popup: Option<&SlashPopup>) -> Vec<CompletionItem> {
    let Some(popup) = popup else {
        return Vec::new();
    };

    popup
        .items
        .iter()
        .map(|item_idx| match popup.kind {
            PopupKind::Command => {
                let cmd = &SLASH_COMMANDS[*item_idx];
                CompletionItem::new(cmd.name, cmd.description)
            }
            PopupKind::Subcommand(cmd_idx) => {
                let cmd = &SLASH_COMMANDS[cmd_idx];
                let sub = &cmd.subcommands[*item_idx];
                CompletionItem::new(format!("{} {}", cmd.name, sub.name), sub.description)
            }
        })
        .collect()
}

fn try_open_slash_popup(input: &TextAreaState, slash_popup: &mut Option<SlashPopup>) {
    if input.cursor_row != 0 {
        *slash_popup = None;
        return;
    }

    let Some(line) = input.lines.first() else {
        *slash_popup = None;
        return;
    };
    if !line.starts_with('/') {
        *slash_popup = None;
        return;
    }

    if let Some(space_idx) = line.find(' ') {
        let cmd_name = &line[..space_idx];
        let after_space = line[space_idx + 1..].trim();

        let Some(cmd_idx) = SLASH_COMMANDS
            .iter()
            .position(|command| command.name == cmd_name)
        else {
            *slash_popup = None;
            return;
        };
        let cmd = &SLASH_COMMANDS[cmd_idx];
        if cmd.subcommands.is_empty() {
            *slash_popup = None;
            return;
        }

        let lower_filter = after_space.to_lowercase();
        let items: Vec<usize> = cmd
            .subcommands
            .iter()
            .enumerate()
            .filter(|(_, sub)| {
                lower_filter.is_empty()
                    || sub.name.starts_with(&lower_filter)
                    || sub.description.to_lowercase().contains(&lower_filter)
            })
            .map(|(idx, _)| idx)
            .collect();

        *slash_popup = (!items.is_empty()).then_some(SlashPopup {
            items,
            selected: 0,
            kind: PopupKind::Subcommand(cmd_idx),
        });
        return;
    }

    let lower_filter = line[1..].to_lowercase();
    let items: Vec<usize> = SLASH_COMMANDS
        .iter()
        .enumerate()
        .filter(|(_, cmd)| {
            lower_filter.is_empty()
                || cmd.name[1..].starts_with(&lower_filter)
                || cmd.description.to_lowercase().contains(&lower_filter)
        })
        .map(|(idx, _)| idx)
        .collect();

    *slash_popup = (!items.is_empty()).then_some(SlashPopup {
        items,
        selected: 0,
        kind: PopupKind::Command,
    });
}

fn handle_slash_popup_key(
    input: &mut TextAreaState,
    slash_popup: &mut Option<SlashPopup>,
    key: KeyEvent,
) -> EditorAction {
    if key.code == KeyCode::Enter && is_newline_modifier(key.modifiers) {
        *slash_popup = None;
        input.handle_key(key, TextAreaSubmitMode::SubmitOnEnter);
        return EditorAction::None;
    }
    if key.code == KeyCode::Char('j') && key.modifiers.contains(KeyModifiers::CONTROL) {
        *slash_popup = None;
        input.handle_key(key, TextAreaSubmitMode::SubmitOnEnter);
        return EditorAction::None;
    }

    match key.code {
        KeyCode::Esc => {
            *slash_popup = None;
            EditorAction::None
        }
        KeyCode::Up => {
            if let Some(popup) = slash_popup {
                popup.selected = if popup.selected == 0 {
                    popup.items.len() - 1
                } else {
                    popup.selected - 1
                };
            }
            EditorAction::None
        }
        KeyCode::Down => {
            if let Some(popup) = slash_popup {
                popup.selected = (popup.selected + 1) % popup.items.len();
            }
            EditorAction::None
        }
        KeyCode::Enter => match accept_slash_popup(input, slash_popup) {
            AcceptOutcome::CompletedCommand | AcceptOutcome::CompletedSubcommand => {
                EditorAction::Submit
            }
            AcceptOutcome::EnteredSubcommands | AcceptOutcome::None => EditorAction::None,
        },
        KeyCode::Tab => {
            accept_slash_popup(input, slash_popup);
            EditorAction::None
        }
        _ => {
            *slash_popup = None;
            handle_key(input, slash_popup, key)
        }
    }
}

fn accept_slash_popup(
    input: &mut TextAreaState,
    slash_popup: &mut Option<SlashPopup>,
) -> AcceptOutcome {
    let Some(popup) = slash_popup.take() else {
        return AcceptOutcome::None;
    };

    match popup.kind {
        PopupKind::Command => {
            let Some(&cmd_idx) = popup.items.get(popup.selected) else {
                return AcceptOutcome::None;
            };
            let cmd = &SLASH_COMMANDS[cmd_idx];
            input.set_text(&format!("{} ", cmd.name));

            if cmd.subcommands.is_empty() {
                AcceptOutcome::CompletedCommand
            } else {
                *slash_popup = Some(SlashPopup {
                    items: (0..cmd.subcommands.len()).collect(),
                    selected: 0,
                    kind: PopupKind::Subcommand(cmd_idx),
                });
                AcceptOutcome::EnteredSubcommands
            }
        }
        PopupKind::Subcommand(cmd_idx) => {
            let cmd = &SLASH_COMMANDS[cmd_idx];
            let Some(&sub_idx) = popup.items.get(popup.selected) else {
                return AcceptOutcome::None;
            };
            let sub = &cmd.subcommands[sub_idx];
            input.set_text(&format!("{} {} ", cmd.name, sub.name));
            AcceptOutcome::CompletedSubcommand
        }
    }
}

fn closes_popup(key: KeyEvent) -> bool {
    matches!(
        key.code,
        KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End
    )
}

fn is_newline_modifier(modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::SHIFT)
        || modifiers.contains(KeyModifiers::ALT)
        || modifiers.contains(KeyModifiers::CONTROL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_and_submit() {
        let mut input = TextAreaState::default();
        let mut popup = None;
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        );
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
        );
        assert_eq!(input.text(), "hi");
        assert_eq!(
            handle_key(
                &mut input,
                &mut popup,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            ),
            EditorAction::Submit
        );
    }

    #[test]
    fn slash_popup_opens_then_navigates() {
        let mut input = TextAreaState::default();
        let mut popup = None;
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
        );
        assert!(popup.is_some());
        let len = popup.as_ref().unwrap().items.len();
        assert!(len > 1);
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );
        assert_eq!(popup.as_ref().unwrap().selected, 1);
    }

    #[test]
    fn tab_accepts_slash_completion() {
        let mut input = TextAreaState::default();
        let mut popup = None;
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
        );
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        );
        assert!(input.text().ends_with(' '));
        assert!(input.text().starts_with('/'));
    }

    #[test]
    fn accepting_command_with_subcommands_enters_subcommand_popup() {
        let mut input = TextAreaState::default();
        let mut popup = None;
        input.set_text("/mc");
        try_open_slash_popup(&input, &mut popup);

        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        );

        assert_eq!(input.text(), "/mcp ");
        let popup = popup.as_ref().expect("subcommand popup should stay open");
        assert!(matches!(popup.kind, PopupKind::Subcommand(_)));
        assert!(popup.items.len() > 1);
    }

    #[test]
    fn enter_on_command_with_subcommands_does_not_submit() {
        let mut input = TextAreaState::default();
        let mut popup = None;
        input.set_text("/mc");
        try_open_slash_popup(&input, &mut popup);

        let action = handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        assert_eq!(action, EditorAction::None);
        assert_eq!(input.text(), "/mcp ");
        assert!(matches!(
            popup.as_ref().map(|popup| &popup.kind),
            Some(PopupKind::Subcommand(_))
        ));
    }

    #[test]
    fn ctrl_enter_in_slash_popup_inserts_newline() {
        let mut input = TextAreaState::default();
        let mut popup = None;
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
        );
        assert!(popup.is_some());
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
        );
        assert_eq!(input.lines.len(), 2);
        assert_eq!(input.cursor_row, 1);
        assert!(popup.is_none());
    }
}

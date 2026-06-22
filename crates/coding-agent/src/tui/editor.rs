//! Agent editor glue: slash completion on top of `iodilos-prompt`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use iodilos::prelude::CompletionItem;
use iodilos_prompt::PromptModel;

use crate::config::Config;
// CommandEntry / SubcommandEntry live in slash_commands so they can be shared
// with `/help` without a circular import (slash_commands is the command-
// metadata owner; editor re-uses its types). `static_command_entries` and
// `SLASH_COMMANDS` are pulled in only by tests, so they're imported there.
use crate::tui::slash_commands::{CommandEntry, list_installed_skills};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EditorState {
    model: PromptModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptAction {
    None,
    Submit,
}

impl EditorState {
    pub fn text(&self) -> String {
        self.model.buffer().to_string()
    }

    pub fn cursor_char(&self) -> usize {
        self.model.cursor_char()
    }

    pub fn is_empty(&self) -> bool {
        self.model.is_empty()
    }

    pub fn clear(&mut self) {
        self.model.submit();
    }

    pub fn set_text(&mut self, text: &str) {
        self.model = PromptModel::new();
        self.model.insert_str(text);
    }

    fn first_line(&self) -> Option<&str> {
        self.model.buffer().lines().next()
    }

    fn cursor_row(&self) -> usize {
        self.model
            .buffer()
            .chars()
            .take(self.model.cursor_char())
            .filter(|ch| *ch == '\n')
            .count()
    }

    fn handle_key(&mut self, key: KeyEvent) -> PromptAction {
        if key.code == KeyCode::Enter && !is_newline_modifier(key.modifiers) {
            return PromptAction::Submit;
        }
        if key.code == KeyCode::Char('j') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.model.newline();
            return PromptAction::None;
        }
        match key.code {
            KeyCode::Enter => self.model.newline(),
            KeyCode::Backspace => self.model.backspace(),
            KeyCode::Left => self.model.move_left(),
            KeyCode::Right => self.model.move_right(),
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.model.insert_char(ch);
            }
            _ => {}
        }
        PromptAction::None
    }
}

/// One selectable entry in the top-level (`/`) completion popup.
///
/// The popup mixes commands (`/help`, `/skills`, `/mcp`, …) with the dynamic
/// `/skill:<name>` family (one entry per installed skill), so a single entry
/// can reference either. This lets `/skill:docx` sit beside `/skills` in the
/// same list instead of only appearing once the user types `/skill:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupItem {
    /// Index into the popup's `commands` snapshot (a merged view of static
    /// commands and extension-registered commands — see [`CommandEntry`]).
    Command(usize),
    /// Index into the popup's `skills` snapshot.
    Skill(usize),
}

/// What kind of items the popup is showing.
#[derive(Debug, Clone)]
pub enum PopupKind {
    /// Top-level command list: mixes commands and `/skill:<name>`
    /// entries (see `PopupItem`).
    Command,
    /// Completing subcommands for a specific command. The index points into
    /// the popup's `commands` snapshot.
    Subcommand(usize),
}

#[derive(Debug, Clone)]
pub struct SlashPopup {
    /// Filtered items matching current input.
    pub items: Vec<PopupItem>,
    /// Currently selected index in the filtered list.
    pub selected: usize,
    /// What kind of completion is active.
    pub kind: PopupKind,
    /// Merged snapshot of all commands (static + extension). `PopupItem::Command`
    /// and `PopupKind::Subcommand` index into this. Captured once per popup
    /// open so filtering while typing doesn't re-query the sources.
    pub commands: Vec<CommandEntry>,
    /// Snapshot of installed skills `(name, description)`. Indexed by the
    /// `PopupItem::Skill(i)` variant in `items`. Captured once per popup open
    /// so filtering while typing doesn't re-scan the filesystem.
    pub skills: Vec<(String, String)>,
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
    input: &mut EditorState,
    slash_popup: &mut Option<SlashPopup>,
    key: KeyEvent,
    config: &Config,
    commands: &[CommandEntry],
    slash_commands_enabled: bool,
) -> EditorAction {
    if !slash_commands_enabled {
        *slash_popup = None;
        return match input.handle_key(key) {
            PromptAction::Submit => EditorAction::Submit,
            PromptAction::None => EditorAction::None,
        };
    }

    if slash_popup.is_some() {
        return handle_slash_popup_key(input, slash_popup, key, config, commands);
    }

    let before = input.text();
    let action = input.handle_key(key);
    match action {
        PromptAction::Submit => EditorAction::Submit,
        PromptAction::None => {
            if input.text() != before {
                try_open_slash_popup(input, slash_popup, config, commands);
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
        .map(|item| match &popup.kind {
            // Subcommand popup: items are positional indices into the
            // command's subcommand slice, stored in the Command variant.
            PopupKind::Subcommand(cmd_idx) => {
                let PopupItem::Command(sub_idx) = item else {
                    return CompletionItem::new("", "");
                };
                let cmd = &popup.commands[*cmd_idx];
                let sub = &cmd.subcommands[*sub_idx];
                CompletionItem::new(
                    format!("{} {}", cmd.name, sub.name),
                    sub.description.clone(),
                )
            }
            // Top-level popup: items are either commands or skills.
            PopupKind::Command => match item {
                PopupItem::Command(cmd_idx) => {
                    let cmd = &popup.commands[*cmd_idx];
                    CompletionItem::new(cmd.name.clone(), cmd.description.clone())
                }
                PopupItem::Skill(skill_idx) => {
                    let (name, desc) = &popup.skills[*skill_idx];
                    CompletionItem::new(format!("/skill:{name}"), desc.clone())
                }
            },
        })
        .collect()
}

fn try_open_slash_popup(
    input: &EditorState,
    slash_popup: &mut Option<SlashPopup>,
    config: &Config,
    commands: &[CommandEntry],
) {
    if input.cursor_row() != 0 {
        *slash_popup = None;
        return;
    }

    let Some(line) = input.first_line() else {
        *slash_popup = None;
        return;
    };
    if !line.starts_with('/') {
        *slash_popup = None;
        return;
    }

    // Subcommand completion (e.g. `/mcp list`): once a complete command name is
    // followed by a space, offer its subcommands. `/skill:<name>` entries live
    // in the top-level list but carry no subcommands, so typing past them just
    // closes the popup (no subcommand slice to complete).
    if let Some(space_idx) = line.find(' ') {
        let cmd_name = &line[..space_idx];
        let after_space = line[space_idx + 1..].trim();

        let Some(cmd_idx) = commands.iter().position(|c| c.name == cmd_name) else {
            *slash_popup = None;
            return;
        };
        let cmd = &commands[cmd_idx];
        if !cmd.has_subcommands() {
            *slash_popup = None;
            return;
        }

        let lower_filter = after_space.to_lowercase();
        let items: Vec<PopupItem> = cmd
            .subcommands
            .iter()
            .enumerate()
            .filter(|(_, sub)| {
                lower_filter.is_empty()
                    || sub.name.to_lowercase().starts_with(&lower_filter)
                    || sub.description.to_lowercase().contains(&lower_filter)
            })
            .map(|(idx, _)| PopupItem::Command(idx))
            .collect();

        *slash_popup = (!items.is_empty()).then_some(SlashPopup {
            items,
            selected: 0,
            kind: PopupKind::Subcommand(cmd_idx),
            commands: commands.to_vec(),
            skills: Vec::new(),
        });
        return;
    }

    // Top-level completion: merged commands (static + extension) + the dynamic
    // `/skill:<name>` family, filtered together by what the user typed after `/`.
    let lower_filter = line[1..].to_lowercase();
    let skills = list_installed_skills(config);

    let mut items: Vec<PopupItem> = Vec::new();

    // Commands from the merged view (static like `/help`, `/skills`, plus
    // extension-registered like `/mcp`).
    for (idx, cmd) in commands.iter().enumerate() {
        if lower_filter.is_empty()
            || cmd.name[1..].to_lowercase().starts_with(&lower_filter)
            || cmd.description.to_lowercase().contains(&lower_filter)
        {
            items.push(PopupItem::Command(idx));
        }
    }

    // Dynamic skill entries, shown alongside the commands. Their
    // "name" for filtering is `skill:<name>` (the part after `/`), so e.g.
    // typing `/skill:d` narrows to skills whose name starts with `d`.
    for (sidx, (name, desc)) in skills.iter().enumerate() {
        let skill_filter_target = format!("skill:{name}");
        if lower_filter.is_empty()
            || skill_filter_target.starts_with(&lower_filter)
            || desc.to_lowercase().contains(&lower_filter)
        {
            items.push(PopupItem::Skill(sidx));
        }
    }

    *slash_popup = (!items.is_empty()).then_some(SlashPopup {
        items,
        selected: 0,
        kind: PopupKind::Command,
        commands: commands.to_vec(),
        skills,
    });
}

fn handle_slash_popup_key(
    input: &mut EditorState,
    slash_popup: &mut Option<SlashPopup>,
    key: KeyEvent,
    config: &Config,
    commands: &[CommandEntry],
) -> EditorAction {
    if key.code == KeyCode::Enter && is_newline_modifier(key.modifiers) {
        *slash_popup = None;
        input.handle_key(key);
        return EditorAction::None;
    }
    if key.code == KeyCode::Char('j') && key.modifiers.contains(KeyModifiers::CONTROL) {
        *slash_popup = None;
        input.handle_key(key);
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
            handle_key(input, slash_popup, key, config, commands, true)
        }
    }
}

fn accept_slash_popup(
    input: &mut EditorState,
    slash_popup: &mut Option<SlashPopup>,
) -> AcceptOutcome {
    let Some(popup) = slash_popup.take() else {
        return AcceptOutcome::None;
    };

    match popup.kind {
        PopupKind::Command => {
            let Some(&item) = popup.items.get(popup.selected) else {
                return AcceptOutcome::None;
            };
            match item {
                PopupItem::Skill(skill_idx) => {
                    let (name, _) = &popup.skills[skill_idx];
                    // Fill `/skill:<name> ` with a trailing space for the
                    // optional request argument, then submit on Enter (Tab just
                    // fills). Skills have no subcommand level.
                    input.set_text(&format!("/skill:{name} "));
                    AcceptOutcome::CompletedCommand
                }
                PopupItem::Command(cmd_idx) => {
                    let cmd = &popup.commands[cmd_idx];

                    if !cmd.has_subcommands() {
                        input.set_text(&cmd.name);
                        AcceptOutcome::CompletedCommand
                    } else {
                        input.set_text(&format!("{} ", cmd.name));
                        *slash_popup = Some(SlashPopup {
                            items: (0..cmd.subcommands.len()).map(PopupItem::Command).collect(),
                            selected: 0,
                            kind: PopupKind::Subcommand(cmd_idx),
                            commands: popup.commands.clone(),
                            skills: Vec::new(),
                        });
                        AcceptOutcome::EnteredSubcommands
                    }
                }
            }
        }
        PopupKind::Subcommand(cmd_idx) => {
            let cmd = &popup.commands[cmd_idx];
            let Some(&PopupItem::Command(sub_idx)) = popup.items.get(popup.selected) else {
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
    use crate::tui::slash_commands::{SLASH_COMMANDS, SubcommandEntry, static_command_entries};

    /// A `Config` whose `skills_dir` points to a nonexistent path, so the skill
    /// scan returns empty and tests that don't care about skills aren't
    /// affected by whatever the host machine happens to have installed.
    fn empty_config() -> Config {
        Config {
            skills_dir: std::path::PathBuf::from("/nonexistent/skills-for-tests"),
            ..Config::default()
        }
    }

    /// The merged command view used by most tests: static commands plus a
    /// synthetic `/mcp` entry with subcommands, mirroring what app.rs assembles
    /// at runtime (static table + McpExtension's CommandSide entry).
    fn test_commands() -> Vec<CommandEntry> {
        let mut commands = static_command_entries();
        commands.push(CommandEntry {
            name: "/mcp".into(),
            description: "Manage MCP servers".into(),
            subcommands: vec![
                SubcommandEntry {
                    name: "list".into(),
                    description: "List configured servers".into(),
                },
                SubcommandEntry {
                    name: "status".into(),
                    description: "Show server connection status".into(),
                },
                SubcommandEntry {
                    name: "help".into(),
                    description: "Show MCP help".into(),
                },
            ],
        });
        commands
    }

    #[test]
    fn type_and_submit() {
        let cfg = empty_config();
        let commands = test_commands();
        let mut input = EditorState::default();
        let mut popup = None;
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
            &cfg,
            &commands,
            true,
        );
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
            &cfg,
            &commands,
            true,
        );
        assert_eq!(input.text(), "hi");
        assert_eq!(
            handle_key(
                &mut input,
                &mut popup,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &cfg,
                &commands,
                true,
            ),
            EditorAction::Submit
        );
    }

    #[test]
    fn slash_popup_opens_then_navigates() {
        let cfg = empty_config();
        let commands = test_commands();
        let mut input = EditorState::default();
        let mut popup = None;
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            &cfg,
            &commands,
            true,
        );
        assert!(popup.is_some());
        let len = popup.as_ref().unwrap().items.len();
        assert!(len > 1);
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &cfg,
            &commands,
            true,
        );
        assert_eq!(popup.as_ref().unwrap().selected, 1);
    }

    #[test]
    fn slash_disabled_treats_slash_as_plain_input() {
        let cfg = empty_config();
        let commands = test_commands();
        let mut input = EditorState::default();
        let mut popup = None;

        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            &cfg,
            &commands,
            false,
        );

        assert_eq!(input.text(), "/");
        assert!(popup.is_none());
    }

    #[test]
    fn tab_accepts_slash_completion() {
        let cfg = empty_config();
        let commands = test_commands();
        let mut input = EditorState::default();
        let mut popup = None;
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            &cfg,
            &commands,
            true,
        );
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &cfg,
            &commands,
            true,
        );
        assert!(input.text().starts_with('/'));
        assert!(popup.is_none() || input.text().ends_with(' '));
    }

    #[test]
    fn tab_completes_model_command_without_submitting_or_trailing_space() {
        let cfg = empty_config();
        let mut commands = static_command_entries();
        commands.push(CommandEntry {
            name: "/model".into(),
            description: "Select a model or thinking level".into(),
            subcommands: Vec::new(),
        });
        let mut input = EditorState::default();
        let mut popup = None;

        for ch in "/mod".chars() {
            let action = handle_key(
                &mut input,
                &mut popup,
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                &cfg,
                &commands,
                true,
            );
            assert_eq!(action, EditorAction::None);
        }
        let popup_before_tab = popup
            .as_ref()
            .expect("/mod should show the extension command popup");
        assert_eq!(popup_before_tab.items.len(), 1);

        let action = handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &cfg,
            &commands,
            true,
        );

        assert_eq!(action, EditorAction::None);
        assert_eq!(input.text(), "/model");
        assert!(popup.is_none());
    }

    #[test]
    fn accepting_command_with_subcommands_enters_subcommand_popup() {
        let cfg = empty_config();
        let commands = test_commands();
        let mut input = EditorState::default();
        let mut popup = None;
        input.set_text("/mc");
        try_open_slash_popup(&input, &mut popup, &cfg, &commands);

        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &cfg,
            &commands,
            true,
        );

        assert_eq!(input.text(), "/mcp ");
        let popup = popup.as_ref().expect("subcommand popup should stay open");
        assert!(matches!(popup.kind, PopupKind::Subcommand(_)));
        assert!(popup.items.len() > 1);
    }

    #[test]
    fn enter_on_command_with_subcommands_does_not_submit() {
        let cfg = empty_config();
        let commands = test_commands();
        let mut input = EditorState::default();
        let mut popup = None;
        input.set_text("/mc");
        try_open_slash_popup(&input, &mut popup, &cfg, &commands);

        let action = handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &cfg,
            &commands,
            true,
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
        let cfg = empty_config();
        let commands = test_commands();
        let mut input = EditorState::default();
        let mut popup = None;
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            &cfg,
            &commands,
            true,
        );
        assert!(popup.is_some());
        handle_key(
            &mut input,
            &mut popup,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            &cfg,
            &commands,
            true,
        );
        assert_eq!(input.text(), "/\n");
        assert_eq!(input.cursor_row(), 1);
        assert!(popup.is_none());
    }

    /// Bare `/` shows both static commands and dynamic skill entries in the
    /// same top-level popup. With no skills installed, only the commands
    /// appear, but the popup must still open (the merged view is non-empty).
    #[test]
    fn top_level_popup_has_static_commands_without_skills() {
        let cfg = empty_config();
        let commands = test_commands();
        let mut input = EditorState::default();
        let mut popup = None;
        input.set_text("/");
        try_open_slash_popup(&input, &mut popup, &cfg, &commands);
        let popup = popup.expect("bare / should open the command popup");
        assert!(matches!(popup.kind, PopupKind::Command));
        // All entries are commands (no skills installed).
        assert!(
            popup
                .items
                .iter()
                .all(|i| matches!(i, PopupItem::Command(_)))
        );
        assert!(popup.items.len() > 1);
    }

    /// `/skills` (plural, no colon) still matches the static `/skills` command
    /// and is NOT mistaken for a skill entry. The unified filter narrows to
    /// exactly that one command.
    #[test]
    fn skills_command_still_matches_after_skill_family_added() {
        let cfg = empty_config();
        let commands = test_commands();
        let mut input = EditorState::default();
        let mut popup = None;
        input.set_text("/skills");
        try_open_slash_popup(&input, &mut popup, &cfg, &commands);
        let popup = popup.expect("/skills should open a command popup");
        assert!(matches!(popup.kind, PopupKind::Command));
        assert_eq!(popup.items.len(), 1);
        let &PopupItem::Command(idx) = popup.items.first().unwrap() else {
            panic!("/skills should match a command, not a skill");
        };
        // The index resolves against the popup's merged snapshot, not the
        // global SLASH_COMMANDS table.
        assert_eq!(popup.commands[idx].name, "/skills");
    }

    /// `/skill:` with nothing after the colon and no installed skills: the
    /// filter target is `skill:`, which no command name starts with and no
    /// skill matches, so the popup closes. Submit-time parsing still handles
    /// it; this only governs autocomplete.
    #[test]
    fn skill_colon_with_no_skills_closes_popup() {
        let cfg = empty_config();
        let commands = test_commands();
        let mut input = EditorState::default();
        let mut popup = None;
        input.set_text("/skill:");
        try_open_slash_popup(&input, &mut popup, &cfg, &commands);
        assert!(popup.is_none());
    }

    /// The popup's merged snapshot includes extension-registered commands
    /// alongside the static ones. `/mcp` is no longer in SLASH_COMMANDS but
    /// must still appear in the top-level autocomplete (contributed by the
    /// extension layer at runtime).
    #[test]
    fn extension_command_appears_in_top_level_popup() {
        let cfg = empty_config();
        let commands = test_commands();
        let mut input = EditorState::default();
        input.set_text("/m");
        let mut popup = None;
        try_open_slash_popup(&input, &mut popup, &cfg, &commands);
        let popup = popup.expect("/m filter should match /mcp from the extension");
        let mcp_entry = popup
            .commands
            .iter()
            .find(|c| c.name == "/mcp")
            .expect("merged snapshot must contain the extension's /mcp");
        assert!(!mcp_entry.subcommands.is_empty());
        // The static table no longer carries /mcp.
        assert!(SLASH_COMMANDS.iter().all(|c| c.name != "/mcp"));
    }

    #[test]
    fn model_command_is_extension_only_in_completion() {
        let cfg = empty_config();
        let mut commands = static_command_entries();
        commands.push(CommandEntry {
            name: "/model".into(),
            description: "Select a model or thinking level".into(),
            subcommands: Vec::new(),
        });

        let mut input = EditorState::default();
        input.set_text("/model");
        let mut popup = None;
        try_open_slash_popup(&input, &mut popup, &cfg, &commands);

        let popup = popup.expect("/model should match the extension command");
        let model_matches = popup
            .items
            .iter()
            .filter(|item| match item {
                PopupItem::Command(idx) => popup.commands[*idx].name == "/model",
                PopupItem::Skill(_) => false,
            })
            .count();
        assert_eq!(model_matches, 1);
        assert!(
            SLASH_COMMANDS
                .iter()
                .all(|command| command.name != "/model")
        );
    }
}

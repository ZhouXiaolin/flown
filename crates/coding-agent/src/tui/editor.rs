//! Editor — pure state + key handling for the multi-line input editor.
//!
//! This module holds the editor's *logic* (buffer, cursor, slash-autocomplete
//! state) with no rendering. The key handler is a pure function over
//! `EditorState`, so it's testable without a renderer and callable from the
//! App's `on_key` router. Rendering (the `RichText` view of the display rows,
//! the slash popup overlay, and the CJK/wrap-aware cursor provider) lives in
//! `components/editor.rs` (Phase 3).
//!
//! Ported from the old hand-written `tui/editor.rs`. The buffer/cursor/popup
//! logic is unchanged; only the rendering (`render_frame`, `display_rows`→view)
//! moved out. `EditorState` replaces the old `Editor` struct so it can live
//! inside an iodilos `RwSignal`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A subcommand definition (e.g. `list`, `status` for `/mcp`).
#[derive(Debug, Clone)]
pub struct SubcommandDef {
    pub name: &'static str,
    pub description: &'static str,
}

/// Slash command definition for autocomplete.
#[derive(Debug, Clone)]
pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub subcommands: &'static [SubcommandDef],
}

/// Built-in slash commands.
pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/help",
        description: "Show available commands",
        subcommands: &[],
    },
    SlashCommand {
        name: "/clear",
        description: "Clear the transcript",
        subcommands: &[],
    },
    SlashCommand {
        name: "/model",
        description: "Switch model",
        subcommands: &[],
    },
    SlashCommand {
        name: "/compact",
        description: "Compact conversation",
        subcommands: &[],
    },
    SlashCommand {
        name: "/mcp",
        description: "Manage MCP servers",
        subcommands: &[
            SubcommandDef {
                name: "list",
                description: "List configured servers",
            },
            SubcommandDef {
                name: "status",
                description: "Show server connection status",
            },
            SubcommandDef {
                name: "help",
                description: "Show MCP help",
            },
        ],
    },
    SlashCommand {
        name: "/skills",
        description: "List available skills",
        subcommands: &[],
    },
    SlashCommand {
        name: "/quit",
        description: "Exit the application",
        subcommands: &[],
    },
];

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

/// The editor's reactive state. Lives inside `RwSignal<EditorState>` in
/// `UiState`; the App's `on_key` calls `handle_key` and writes the result back.
#[derive(Debug, Clone)]
pub struct EditorState {
    /// Lines of text (each line without a trailing newline).
    pub lines: Vec<String>,
    /// Current cursor row (logical line index).
    pub cursor_row: usize,
    /// Current cursor column (character index within line, NOT byte offset).
    pub cursor_col: usize,
    /// Whether the editor is focused (affects border rendering).
    pub focused: bool,
    /// Slash-autocomplete state, if a popup is open.
    pub slash_popup: Option<SlashPopup>,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            focused: true,
            slash_popup: None,
        }
    }
}

impl EditorState {
    /// Get the full text content (lines joined by `\n`).
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Set the text content (e.g. after accepting an autocomplete item).
    pub fn set_text(&mut self, text: &str) {
        self.lines = text.lines().map(String::from).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = self.lines[self.cursor_row].chars().count();
    }

    /// Clear the editor content.
    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.slash_popup = None;
    }

    /// Insert text (handles multi-line paste).
    pub fn insert_text(&mut self, text: &str) {
        for ch in text.chars() {
            if ch == '\r' {
                continue;
            }
            if ch == '\n' {
                self.insert_newline();
            } else if !ch.is_control() {
                self.insert_char(ch);
            }
        }
    }

    /// Handle a key event. Returns an action for the app to process.
    pub fn handle_key(&mut self, key: KeyEvent) -> EditorAction {
        // If slash popup is open, route navigation there.
        if self.slash_popup.is_some() {
            return self.handle_slash_popup_key(key);
        }

        match (key.code, key.modifiers) {
            // Submit on Enter (no modifiers)
            (KeyCode::Enter, KeyModifiers::NONE) => {
                if self.lines.join("\n").trim().is_empty() {
                    EditorAction::None
                } else {
                    EditorAction::Submit
                }
            }
            // Newline: Shift+Enter, Alt+Enter, Ctrl+Enter, or terminals that
            // report Ctrl+Enter as Ctrl+J.
            (KeyCode::Enter, m) if is_newline_modifier(m) => {
                self.insert_newline();
                EditorAction::None
            }
            (KeyCode::Char('j'), m) if m.contains(KeyModifiers::CONTROL) => {
                self.insert_newline();
                EditorAction::None
            }
            // Backspace
            (KeyCode::Backspace, _) => {
                let had_popup = self.slash_popup.is_some();
                self.delete_backward();
                if had_popup {
                    self.update_slash_popup();
                } else {
                    self.try_open_slash_popup();
                }
                EditorAction::None
            }
            // Delete
            (KeyCode::Delete, _) => {
                self.delete_forward();
                EditorAction::None
            }
            // Cursor movement
            (KeyCode::Left, _) => {
                self.move_left();
                self.slash_popup = None;
                EditorAction::None
            }
            (KeyCode::Right, _) => {
                self.move_right();
                self.slash_popup = None;
                EditorAction::None
            }
            (KeyCode::Up, _) => {
                self.move_up();
                self.slash_popup = None;
                EditorAction::None
            }
            (KeyCode::Down, _) => {
                self.move_down();
                self.slash_popup = None;
                EditorAction::None
            }
            (KeyCode::Home, _) => {
                self.cursor_col = 0;
                self.slash_popup = None;
                EditorAction::None
            }
            (KeyCode::End, _) => {
                self.cursor_col = self.lines[self.cursor_row].chars().count();
                self.slash_popup = None;
                EditorAction::None
            }
            // Tab — accept slash autocomplete if popup is open
            (KeyCode::Tab, _) => {
                self.accept_slash_popup();
                EditorAction::None
            }
            // Ctrl+U — clear line
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.lines[self.cursor_row].clear();
                self.cursor_col = 0;
                self.slash_popup = None;
                EditorAction::None
            }
            // Ctrl+K — kill to end of line
            (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                let line = &mut self.lines[self.cursor_row];
                let byte_offset = char_to_byte_offset(line, self.cursor_col);
                line.truncate(byte_offset);
                EditorAction::None
            }
            // Regular character input
            (KeyCode::Char(c), m) if m.is_empty() || m.contains(KeyModifiers::SHIFT) => {
                self.insert_char(c);
                self.try_open_slash_popup();
                EditorAction::None
            }
            _ => EditorAction::None,
        }
    }

    // ── Slash command autocomplete ─────────────────────────────────────

    fn try_open_slash_popup(&mut self) {
        // Only open at the very start of the first line.
        if self.cursor_row != 0 {
            self.slash_popup = None;
            return;
        }
        let line = &self.lines[0];
        if !line.starts_with('/') {
            self.slash_popup = None;
            return;
        }

        // Check if we're past the command name (space present).
        if let Some(space_idx) = line.find(' ') {
            let cmd_name = &line[..space_idx];
            let after_space = line[space_idx + 1..].trim();

            let cmd_idx = SLASH_COMMANDS.iter().position(|c| c.name == cmd_name);
            let Some(cmd_idx) = cmd_idx else {
                self.slash_popup = None;
                return;
            };
            let cmd = &SLASH_COMMANDS[cmd_idx];

            if cmd.subcommands.is_empty() {
                self.slash_popup = None;
                return;
            }

            let lower_filter = after_space.to_lowercase();
            let items: Vec<usize> = cmd
                .subcommands
                .iter()
                .enumerate()
                .filter(|(_, sub)| {
                    if lower_filter.is_empty() {
                        return true;
                    }
                    sub.name.starts_with(&lower_filter)
                        || sub.description.to_lowercase().contains(&lower_filter)
                })
                .map(|(i, _)| i)
                .collect();

            if items.is_empty() {
                self.slash_popup = None;
            } else {
                self.slash_popup = Some(SlashPopup {
                    items,
                    selected: 0,
                    kind: PopupKind::Subcommand(cmd_idx),
                });
            }
            return;
        }

        // No space — complete command names.
        let filter = line[1..].to_string(); // text after '/'
        let lower_filter = filter.to_lowercase();
        let items: Vec<usize> = SLASH_COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, cmd)| {
                if lower_filter.is_empty() {
                    return true;
                }
                let name = &cmd.name[1..]; // without '/'
                let desc = cmd.description.to_lowercase();
                name.starts_with(&lower_filter) || desc.contains(&lower_filter)
            })
            .map(|(i, _)| i)
            .collect();

        if items.is_empty() {
            self.slash_popup = None;
        } else {
            self.slash_popup = Some(SlashPopup {
                items,
                selected: 0,
                kind: PopupKind::Command,
            });
        }
    }

    fn update_slash_popup(&mut self) {
        self.try_open_slash_popup();
    }

    fn accept_slash_popup(&mut self) {
        if let Some(popup) = self.slash_popup.take() {
            match popup.kind {
                PopupKind::Command => {
                    if let Some(&cmd_idx) = popup.items.get(popup.selected) {
                        let cmd = &SLASH_COMMANDS[cmd_idx];
                        self.lines[0] = format!("{} ", cmd.name);
                        self.cursor_col = self.lines[0].chars().count();
                    }
                }
                PopupKind::Subcommand(cmd_idx) => {
                    let cmd = &SLASH_COMMANDS[cmd_idx];
                    if let Some(&sub_idx) = popup.items.get(popup.selected) {
                        let sub = &cmd.subcommands[sub_idx];
                        self.lines[0] = format!("{} {} ", cmd.name, sub.name);
                        self.cursor_col = self.lines[0].chars().count();
                    }
                }
            }
        }
    }

    fn handle_slash_popup_key(&mut self, key: KeyEvent) -> EditorAction {
        if key.code == KeyCode::Enter && is_newline_modifier(key.modifiers) {
            self.slash_popup = None;
            self.insert_newline();
            return EditorAction::None;
        }
        if key.code == KeyCode::Char('j') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.slash_popup = None;
            self.insert_newline();
            return EditorAction::None;
        }

        match key.code {
            KeyCode::Esc => {
                self.slash_popup = None;
                EditorAction::None
            }
            KeyCode::Up => {
                if let Some(ref mut popup) = self.slash_popup {
                    popup.selected = if popup.selected == 0 {
                        popup.items.len() - 1
                    } else {
                        popup.selected - 1
                    };
                }
                EditorAction::None
            }
            KeyCode::Down => {
                if let Some(ref mut popup) = self.slash_popup {
                    popup.selected = (popup.selected + 1) % popup.items.len();
                }
                EditorAction::None
            }
            KeyCode::Enter => {
                let was_subcommand = matches!(
                    self.slash_popup.as_ref().map(|p| &p.kind),
                    Some(PopupKind::Subcommand(_))
                );
                self.accept_slash_popup();
                // Commands without args (like /help, /clear, /quit) submit
                // immediately. Subcommand completions (like /mcp list) also
                // submit immediately.
                let text = self.text();
                let trimmed = text.trim();
                if was_subcommand || (!trimmed.contains(' ') && trimmed.starts_with('/')) {
                    return EditorAction::Submit;
                }
                EditorAction::None
            }
            KeyCode::Tab => {
                self.accept_slash_popup();
                EditorAction::None
            }
            _ => {
                // Forward other keys to normal handling.
                self.slash_popup = None;
                self.handle_key(key)
            }
        }
    }

    // ── Character operations (CJK-aware, char-index based) ─────────────

    fn insert_char(&mut self, c: char) {
        let byte_offset = char_to_byte_offset(&self.lines[self.cursor_row], self.cursor_col);
        self.lines[self.cursor_row].insert(byte_offset, c);
        self.cursor_col += 1;
    }

    fn insert_newline(&mut self) {
        let byte_offset = char_to_byte_offset(&self.lines[self.cursor_row], self.cursor_col);
        let rest = self.lines[self.cursor_row][byte_offset..].to_string();
        self.lines[self.cursor_row].truncate(byte_offset);
        self.lines.insert(self.cursor_row + 1, rest);
        self.cursor_row += 1;
        self.cursor_col = 0;
    }

    fn delete_backward(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            if let Some((offset, _)) = line.char_indices().nth(self.cursor_col - 1) {
                line.remove(offset);
                self.cursor_col -= 1;
            }
        } else if self.cursor_row > 0 {
            let current = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
            self.lines[self.cursor_row].push_str(&current);
        }
    }

    fn delete_forward(&mut self) {
        let line = &self.lines[self.cursor_row];
        let char_count = line.chars().count();
        if self.cursor_col < char_count {
            let line = &mut self.lines[self.cursor_row];
            if let Some((offset, _)) = line.char_indices().nth(self.cursor_col) {
                line.remove(offset);
            }
        } else if self.cursor_row + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].push_str(&next);
        }
    }

    fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
        }
    }

    fn move_right(&mut self) {
        let char_count = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < char_count {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            let char_count = self.lines[self.cursor_row].chars().count();
            self.cursor_col = self.cursor_col.min(char_count);
        }
    }

    fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            let char_count = self.lines[self.cursor_row].chars().count();
            self.cursor_col = self.cursor_col.min(char_count);
        }
    }
}

/// Actions the editor can signal to the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorAction {
    None,
    Submit,
    // (Quit is handled at the App level via Ctrl+C / Ctrl+Q, not the editor.)
}

/// Convert character index to byte offset within a string.
fn char_to_byte_offset(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
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
        let mut e = EditorState::default();
        e.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        e.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        assert_eq!(e.text(), "hi");
        let action = e.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, EditorAction::Submit);
    }

    #[test]
    fn multiline_via_shift_enter() {
        let mut e = EditorState::default();
        e.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        e.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        e.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(e.text(), "a\nb");
    }

    #[test]
    fn multiline_via_ctrl_enter() {
        let mut e = EditorState::default();
        e.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        e.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        e.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(e.text(), "a\nb");
    }

    #[test]
    fn multiline_via_ctrl_j() {
        let mut e = EditorState::default();
        e.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        e.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        e.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(e.text(), "a\nb");
    }

    #[test]
    fn empty_input_does_not_submit() {
        let mut e = EditorState::default();
        let action = e.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, EditorAction::None);
    }

    #[test]
    fn backspace_deletes() {
        let mut e = EditorState::default();
        e.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        e.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(e.text(), "");
    }

    #[test]
    fn slash_popup_opens_then_navigates() {
        let mut e = EditorState::default();
        // Type "/" → popup opens with all commands.
        e.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(e.slash_popup.is_some());
        let len = e.slash_popup.as_ref().unwrap().items.len();
        assert!(len > 1);
        // Down → selected advances.
        e.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(e.slash_popup.as_ref().unwrap().selected, 1);
    }

    #[test]
    fn tab_accepts_slash_completion() {
        let mut e = EditorState::default();
        e.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        e.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        // After accepting the first completion, the line is "<cmd> ".
        assert!(e.lines[0].ends_with(' '));
        assert!(e.lines[0].starts_with('/'));
    }

    #[test]
    fn ctrl_enter_in_slash_popup_inserts_newline() {
        let mut e = EditorState::default();
        e.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(e.slash_popup.is_some());
        e.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        assert_eq!(e.lines.len(), 2);
        assert_eq!(e.cursor_row, 1);
        assert!(e.slash_popup.is_none());
    }
}

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

/// Slash command definition for autocomplete.
#[derive(Debug, Clone)]
pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
}

/// Built-in slash commands.
const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand { name: "/help", description: "Show available commands" },
    SlashCommand { name: "/clear", description: "Clear the transcript" },
    SlashCommand { name: "/model", description: "Switch model" },
    SlashCommand { name: "/compact", description: "Compact conversation" },
    SlashCommand { name: "/quit", description: "Exit the application" },
];

/// Multi-line input editor with CJK-aware cursor and slash command autocomplete.
pub struct Editor {
    /// Lines of text (each line is a string without newline)
    lines: Vec<String>,
    /// Current cursor row
    cursor_row: usize,
    /// Current cursor column (byte offset within line)
    cursor_col: usize,
    /// Whether the editor is focused
    focused: bool,
    /// Slash autocomplete state
    slash_popup: Option<SlashPopup>,
}

struct SlashPopup {
    /// Filtered commands matching current input
    items: Vec<usize>,
    /// Currently selected index in filtered list
    selected: usize,
    /// Current filter text (after '/')
    filter: String,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            focused: true,
            slash_popup: None,
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Get the full text content.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Set the text content.
    pub fn set_text(&mut self, text: &str) {
        self.lines = text.lines().map(String::from).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = self.lines[self.cursor_row].len();
    }

    /// Clear the editor content.
    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.slash_popup = None;
    }

    /// Handle a key event. Returns an action for the app to process.
    pub fn handle_key(&mut self, key: KeyEvent) -> EditorAction {
        // If slash popup is open, route navigation there
        if self.slash_popup.is_some() {
            return self.handle_slash_popup_key(key);
        }

        match (key.code, key.modifiers) {
            // Submit on Enter (no modifiers)
            (KeyCode::Enter, KeyModifiers::NONE) => {
                if self.lines.join("\n").trim().is_empty() {
                    return EditorAction::None;
                }
                EditorAction::Submit
            }
            // Newline: Shift+Enter, Alt+Enter, or Ctrl+Enter
            (KeyCode::Enter, m) if m.contains(KeyModifiers::SHIFT) || m.contains(KeyModifiers::ALT) || m.contains(KeyModifiers::CONTROL) => {
                self.insert_char('\n');
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
                self.cursor_col = self.lines[self.cursor_row].len();
                self.slash_popup = None;
                EditorAction::None
            }
            // Tab — accept slash autocomplete if popup is open
            (KeyCode::Tab, _) => {
                self.accept_slash_popup();
                EditorAction::None
            }
            // Ctrl+C — cancel/clear
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if self.lines.join("\n").is_empty() {
                    EditorAction::Quit
                } else {
                    self.clear();
                    EditorAction::None
                }
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
                self.lines[self.cursor_row].truncate(self.cursor_col);
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

    // ── Slash command autocomplete ─────────────────────────────────

    fn try_open_slash_popup(&mut self) {
        // Only open at the very start of the first line
        if self.cursor_row != 0 {
            self.slash_popup = None;
            return;
        }
        let line = &self.lines[0];
        if !line.starts_with('/') {
            self.slash_popup = None;
            return;
        }
        // Don't open if there's a space (command already complete)
        if line.contains(' ') {
            self.slash_popup = None;
            return;
        }
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
            let selected = 0;
            self.slash_popup = Some(SlashPopup { items, selected, filter });
        }
    }

    fn update_slash_popup(&mut self) {
        // Re-filter based on current text
        self.try_open_slash_popup();
    }

    fn accept_slash_popup(&mut self) {
        if let Some(popup) = self.slash_popup.take() {
            if let Some(&cmd_idx) = popup.items.get(popup.selected) {
                let cmd = &SLASH_COMMANDS[cmd_idx];
                // Replace current line with the command name + space
                self.lines[0] = format!("{} ", cmd.name);
                self.cursor_col = self.lines[0].len();
            }
        }
    }

    fn handle_slash_popup_key(&mut self, key: KeyEvent) -> EditorAction {
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
                self.accept_slash_popup();
                // If it's a complete command (like /help, /clear, /quit), submit immediately
                let text = self.text();
                let trimmed = text.trim();
                if !trimmed.contains(' ') && trimmed.starts_with('/') {
                    return EditorAction::Submit;
                }
                EditorAction::None
            }
            KeyCode::Tab => {
                self.accept_slash_popup();
                EditorAction::None
            }
            _ => {
                // Forward other keys to normal handling
                self.slash_popup = None;
                self.handle_key(key)
            }
        }
    }

    // ── Character operations (CJK-aware) ───────────────────────────

    fn insert_char(&mut self, c: char) {
        if c == '\n' {
            let rest = self.lines[self.cursor_row][self.cursor_col..].to_string();
            self.lines[self.cursor_row].truncate(self.cursor_col);
            self.cursor_row += 1;
            self.lines.insert(self.cursor_row, rest);
            self.cursor_col = 0;
        } else {
            self.lines[self.cursor_row].insert(self.cursor_col, c);
            self.cursor_col += c.len_utf8();
        }
    }

    fn delete_backward(&mut self) {
        if self.cursor_col > 0 {
            let line = &self.lines[self.cursor_row];
            let mut prev_boundary = self.cursor_col - 1;
            while prev_boundary > 0 && !line.is_char_boundary(prev_boundary) {
                prev_boundary -= 1;
            }
            self.lines[self.cursor_row].remove(prev_boundary);
            self.cursor_col = prev_boundary;
        } else if self.cursor_row > 0 {
            let current = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
            self.lines[self.cursor_row].push_str(&current);
        }
    }

    fn delete_forward(&mut self) {
        let line = &self.lines[self.cursor_row];
        if self.cursor_col < line.len() {
            let mut next_boundary = self.cursor_col + 1;
            while next_boundary < line.len() && !line.is_char_boundary(next_boundary) {
                next_boundary += 1;
            }
            self.lines[self.cursor_row].remove(self.cursor_col);
        } else if self.cursor_row + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].push_str(&next);
        }
    }

    fn move_left(&mut self) {
        if self.cursor_col > 0 {
            let line = &self.lines[self.cursor_row];
            let mut prev_boundary = self.cursor_col - 1;
            while prev_boundary > 0 && !line.is_char_boundary(prev_boundary) {
                prev_boundary -= 1;
            }
            self.cursor_col = prev_boundary;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
        }
    }

    fn move_right(&mut self) {
        let line = &self.lines[self.cursor_row];
        if self.cursor_col < line.len() {
            let mut next_boundary = self.cursor_col + 1;
            while next_boundary < line.len() && !line.is_char_boundary(next_boundary) {
                next_boundary += 1;
            }
            self.cursor_col = next_boundary;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
        }
    }

    fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
        }
    }

    // ── Visual column helpers (CJK-aware) ──────────────────────────

    /// Compute the visual column (in terminal cells) for a byte offset in a string.
    fn visual_col_at(s: &str, byte_offset: usize) -> u16 {
        let mut col = 0u16;
        for (i, ch) in s.char_indices() {
            if i >= byte_offset {
                break;
            }
            col += ch.width().unwrap_or(0) as u16;
        }
        col
    }

    // ── Rendering ──────────────────────────────────────────────────

    /// Render the editor widget.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        // Reserve space for the popup above
        let popup_height = self.slash_popup.as_ref().map(|p| {
            let visible_items = p.items.len().min(6);
            (visible_items as u16) + 2 // +2 for border
        }).unwrap_or(0);

        let editor_area = Rect {
            x: area.x,
            y: area.y + popup_height,
            width: area.width,
            height: area.height.saturating_sub(popup_height),
        };

        // Render slash popup if active
        if let Some(ref popup) = self.slash_popup {
            self.render_slash_popup(f, area, popup);
        }

        // Editor border style
        let border_style = if self.focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style);

        // Build display lines with a minimal dot prefix (no "You" label)
        let display_lines: Vec<Line> = self
            .lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                if i == 0 {
                    Line::from(vec![
                        Span::styled(" ● ", Style::default().fg(Color::DarkGray)),
                        Span::raw(line.as_str()),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled("   ", Style::default()),
                        Span::raw(line.as_str()),
                    ])
                }
            })
            .collect();

        let paragraph = Paragraph::new(display_lines).block(block);
        f.render_widget(paragraph, editor_area);

        // Position cursor (CJK-aware)
        if self.focused {
            let prompt_width = 3u16; // " ● " = 3 cells
            let text_before = &self.lines[self.cursor_row][..self.cursor_col];
            let cursor_x = editor_area.x + prompt_width + Self::visual_col_at(text_before, self.cursor_col);
            let cursor_y = editor_area.y + 1 + self.cursor_row as u16; // +1 for top border
            f.set_cursor_position((cursor_x, cursor_y));
        }
    }

    fn render_slash_popup(&self, f: &mut Frame, area: Rect, popup: &SlashPopup) {
        let visible_count = popup.items.len().min(6) as u16;
        let popup_area = Rect {
            x: area.x + 1,
            y: area.y,
            width: area.width.saturating_sub(2).min(40),
            height: visible_count + 2, // +2 for border
        };

        // Clear background
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" Commands ");

        let inner = block.inner(popup_area);
        f.render_widget(block, popup_area);

        // Render items
        let mut lines = Vec::new();
        for (vis_idx, &cmd_idx) in popup.items.iter().take(visible_count as usize).enumerate() {
            let cmd = &SLASH_COMMANDS[cmd_idx];
            let is_selected = vis_idx == popup.selected;
            let prefix = if is_selected { "▸ " } else { "  " };
            let style = if is_selected {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let desc_style = if is_selected {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            lines.push(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(&cmd.name[1..], style), // without '/'
                Span::styled(format!("  {}", cmd.description), desc_style),
            ]));
        }

        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, inner);
    }
}

/// Actions the editor can signal to the app.
#[derive(Debug)]
pub enum EditorAction {
    None,
    Submit,
    Quit,
}

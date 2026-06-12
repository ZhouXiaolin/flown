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

const MAX_INPUT_BODY_LINES: usize = 8;
const INPUT_PROMPT_PRIMARY: &str = " ";
const INPUT_PROMPT_CONTINUE: &str = " ";

/// Multi-line input editor with CJK-aware cursor, visual line wrapping,
/// and slash command autocomplete.
pub struct Editor {
    /// Lines of text (each line is a string without newline)
    lines: Vec<String>,
    /// Current cursor row (logical line index)
    cursor_row: usize,
    /// Current cursor column (character index within line, NOT byte offset)
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

/// A single display row after visual line wrapping.
struct DisplayRow {
    prefix: &'static str,
    text: String,
    logical_line: usize,
    start_col: usize,
    end_col: usize,
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
            (KeyCode::Enter, m)
                if m.contains(KeyModifiers::SHIFT)
                    || m.contains(KeyModifiers::ALT)
                    || m.contains(KeyModifiers::CONTROL) =>
            {
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
                self.cursor_col = self.lines[0].chars().count();
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

    // ── Character operations (CJK-aware, char-index based) ────────

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

    // ── Visual line wrapping ───────────────────────────────────────

    /// Compute display rows with visual line wrapping for the given inner width.
    fn display_rows(&self, inner_width: usize) -> Vec<DisplayRow> {
        let inner_width = inner_width.max(1);
        let mut rows = Vec::new();
        for (logical_line, line) in self.lines.iter().enumerate() {
            rows.extend(Self::wrap_logical_line(logical_line, line, inner_width));
        }
        rows
    }

    /// Wrap a single logical line into multiple display rows.
    fn wrap_logical_line(
        logical_line: usize,
        line: &str,
        inner_width: usize,
    ) -> Vec<DisplayRow> {
        let mut rows = Vec::new();
        let mut seg_idx = 0usize;
        let mut col = 0usize;
        let char_count = line.chars().count();

        loop {
            let prefix = if logical_line == 0 && seg_idx == 0 {
                INPUT_PROMPT_PRIMARY
            } else {
                INPUT_PROMPT_CONTINUE
            };
            let avail = inner_width.saturating_sub(display_width_str(prefix)).max(1);
            let mut chunk = String::new();
            let mut used_width = 0usize;
            let mut end_col = col;

            for ch in line.chars().skip(col) {
                let ch_width = ch.width().unwrap_or(0);
                if !chunk.is_empty() && used_width + ch_width > avail {
                    break;
                }
                if chunk.is_empty() && ch_width > avail {
                    chunk.push(ch);
                    end_col += 1;
                    break;
                }
                chunk.push(ch);
                used_width += ch_width;
                end_col += 1;
            }

            rows.push(DisplayRow {
                prefix,
                text: chunk,
                logical_line,
                start_col: col,
                end_col,
            });

            if end_col >= char_count {
                break;
            }
            col = end_col;
            seg_idx += 1;
        }

        if rows.is_empty() {
            rows.push(DisplayRow {
                prefix: if logical_line == 0 {
                    INPUT_PROMPT_PRIMARY
                } else {
                    INPUT_PROMPT_CONTINUE
                },
                text: String::new(),
                logical_line,
                start_col: 0,
                end_col: 0,
            });
        }

        rows
    }

    /// Compute the visual column (terminal cells) for a char offset within a string.
    fn screen_col_for_char_offset(text: &str, char_offset: usize) -> usize {
        let mut col = 0usize;
        for c in text.chars().take(char_offset) {
            col += c.width().unwrap_or(0);
        }
        col
    }

    /// Compute the cursor's screen position (row, col) accounting for wrapping.
    fn cursor_screen_position(&self, inner_width: usize) -> (usize, usize) {
        let display_rows = self.display_rows(inner_width);
        for (screen_row, row) in display_rows.iter().enumerate() {
            if row.logical_line != self.cursor_row {
                continue;
            }
            let char_count = self.lines[self.cursor_row].chars().count();
            let contains = if row.end_col >= char_count {
                self.cursor_col >= row.start_col && self.cursor_col <= row.end_col
            } else {
                self.cursor_col >= row.start_col && self.cursor_col < row.end_col
            };
            if contains {
                let offset_in_chunk = self.cursor_col.saturating_sub(row.start_col);
                let col = display_width_str(row.prefix)
                    + Self::screen_col_for_char_offset(&row.text, offset_in_chunk);
                return (screen_row, col);
            }
        }
        (
            display_rows.len().saturating_sub(1),
            display_width_str(INPUT_PROMPT_PRIMARY),
        )
    }

    /// Compute the total height needed for the input widget (including slash popup if active).
    pub fn input_height(&self, width: u16) -> u16 {
        let inner_width = width.saturating_sub(2).max(1) as usize;
        let rows = self
            .display_rows(inner_width)
            .len()
            .clamp(1, MAX_INPUT_BODY_LINES);
        let editor_height = rows as u16 + 2; // +2 for top and bottom borders

        // Add slash popup height if active
        let popup_height = self
            .slash_popup
            .as_ref()
            .map(|p| {
                let visible_items = p.items.len().min(6);
                (visible_items as u16) + 2 // +2 for border
            })
            .unwrap_or(0);

        editor_height + popup_height
    }

    // ── Rendering ──────────────────────────────────────────────────

    /// Get the height of the slash popup if active.
    fn slash_popup_height(&self) -> u16 {
        self.slash_popup
            .as_ref()
            .map(|p| {
                let visible_items = p.items.len().min(6);
                (visible_items as u16) + 2 // +2 for border
            })
            .unwrap_or(0)
    }

    /// Render the editor widget.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let popup_height = self.slash_popup_height();

        // Popup area is at the top of the given area
        let popup_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: popup_height,
        };

        // Editor area is below the popup
        let editor_area = Rect {
            x: area.x,
            y: area.y + popup_height,
            width: area.width,
            height: area.height.saturating_sub(popup_height),
        };

        // Render slash popup if active
        if let Some(ref popup) = self.slash_popup {
            self.render_slash_popup(f, popup_area, popup);
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

        let inner_width = editor_area.width.saturating_sub(2) as usize;
        if inner_width == 0 {
            f.render_widget(block, editor_area);
            return;
        }

        // Build display lines with visual wrapping
        let prompt_style = Style::default().fg(Color::DarkGray);
        let input_style = Style::default().fg(Color::White);
        let wrapped_lines: Vec<Line> = self
            .display_rows(inner_width)
            .into_iter()
            .map(|row| {
                Line::from(vec![
                    Span::styled(row.prefix, prompt_style),
                    Span::styled(row.text, input_style),
                ])
            })
            .collect();

        let paragraph = Paragraph::new(wrapped_lines).block(block);
        f.render_widget(paragraph, editor_area);

        // Position cursor (CJK-aware, wrapping-aware)
        // Block with Borders::ALL: content starts at (x+1, y+1)
        if self.focused {
            let (cursor_row, cursor_col) = self.cursor_screen_position(inner_width);
            let max_row = editor_area.height.saturating_sub(2);
            let max_col = editor_area.width.saturating_sub(3);
            f.set_cursor_position((
                editor_area.x + 1 + (cursor_col as u16).min(max_col),
                editor_area.y + 1 + (cursor_row as u16).min(max_row),
            ));
        }
    }

    fn render_slash_popup(&self, f: &mut Frame, area: Rect, popup: &SlashPopup) {
        // Clear background
        f.render_widget(Clear, area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" Commands ");

        let inner = block.inner(area);
        f.render_widget(block, area);

        // Render items
        let visible_count = inner.height as usize;
        let mut lines = Vec::new();
        for (vis_idx, &cmd_idx) in popup.items.iter().take(visible_count).enumerate() {
            let cmd = &SLASH_COMMANDS[cmd_idx];
            let is_selected = vis_idx == popup.selected;
            let prefix = if is_selected { "▸ " } else { "  " };
            let style = if is_selected {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let desc_style = Style::default().fg(Color::DarkGray);
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

/// Compute the display width of a string in terminal cells.
fn display_width_str(text: &str) -> usize {
    text.chars().map(|ch| ch.width().unwrap_or(0)).sum()
}

/// Convert character index to byte offset within a string.
fn char_to_byte_offset(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

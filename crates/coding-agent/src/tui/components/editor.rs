//! InputEditor — reactive multi-line editor view.
//!
//! The editor logic lives in [`crate::tui::editor::EditorState`]. This component
//! is the iodilos rendering bridge: rich text rows, slash-completion panel, and
//! terminal cursor placement via `Node::set_cursor_provider`.

use std::rc::Rc;

use iodilos::prelude::*;
use unicode_width::UnicodeWidthChar;

use crate::tui::editor::{EditorState, PopupKind, SLASH_COMMANDS};
use crate::tui::state::UiState;

#[component]
pub fn InputEditor() -> Node {
    let state = use_context::<Rc<UiState>>();
    let input = state.input;
    let terminal_size = use_terminal_size();

    let root = Node::new_view();
    root.set_flex_direction(FlexDirection::Column);
    root.set_width_percent(100.0);

    let popup = build_slash_popup(input);
    let editor_box = build_editor_box(input, terminal_size);

    let root_for_effect = root.clone();
    let popup_for_effect = popup.clone();
    let editor_for_effect = editor_box.clone();
    create_effect(move || {
        if input.with(|es| es.slash_popup.is_some()) {
            root_for_effect.set_children(vec![popup_for_effect.clone(), editor_for_effect.clone()]);
        } else {
            root_for_effect.set_children(vec![editor_for_effect.clone()]);
        }
    });

    root
}

fn build_editor_box(input: RwSignal<EditorState>, terminal_size: RwSignal<(u16, u16)>) -> Node {
    let box_node = Node::new_view();
    box_node.set_border(Borders::ALL);
    box_node.set_border_color(Color::Cyan);
    box_node.set_padding_left(1.0);
    box_node.set_padding_right(1.0);
    box_node.set_min_height(3.0);
    box_node.set_width_percent(100.0);

    let content = Node::new_richtext();
    let content_for_effect = content.clone();
    create_effect(move || {
        let (terminal_width, _) = terminal_size.get();
        let content_width = editor_content_width(terminal_width);
        input.with(|es| {
            content_for_effect.set_lines(editor_lines(es, content_width));
        });
    });
    box_node.add_child(content);

    box_node.set_cursor_provider(move |rect| {
        input.with(|es| {
            if !es.focused || rect.width == 0 || rect.height == 0 {
                return None;
            }
            let content_width = editor_content_width(rect.width);
            let cursor = editor_cursor_position(es, content_width);

            let x = rect.x.saturating_add(2).saturating_add(cursor.col);
            let y = rect.y.saturating_add(1).saturating_add(cursor.row);
            if x < rect.right() && y < rect.bottom() {
                Some((x, y))
            } else {
                None
            }
        })
    });

    box_node
}

fn build_slash_popup(input: RwSignal<EditorState>) -> Node {
    let popup = Node::new_view();
    popup.set_border(Borders::ALL);
    popup.set_border_color(Color::DarkGray);
    popup.set_padding_left(1.0);
    popup.set_padding_right(1.0);
    popup.set_width_percent(100.0);

    let content = Node::new_richtext();
    let content_for_effect = content.clone();
    create_effect(move || {
        input.with(|es| {
            content_for_effect.set_lines(slash_popup_lines(es));
        });
    });
    popup.add_child(content);
    popup
}

fn editor_content_width(container_width: u16) -> usize {
    container_width.saturating_sub(4).max(1) as usize
}

fn editor_lines(es: &EditorState, width: usize) -> Vec<Line<'static>> {
    let body_style = Style::default().fg(Color::White);
    if es.lines.is_empty() {
        return vec![Line::from("")];
    }
    editor_visual_rows(es, width)
        .into_iter()
        .map(|line| Line::from(vec![Span::styled(line, body_style)]))
        .collect()
}

fn editor_visual_rows(es: &EditorState, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for (logical_row, line) in es.lines.iter().enumerate() {
        let mut wrapped = wrap_editor_line(line, width);
        if logical_row == es.cursor_row {
            let cursor = cursor_in_line(line, es.cursor_col, width);
            while wrapped.len() <= cursor.row as usize {
                wrapped.push(String::new());
            }
        }
        rows.extend(wrapped);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn wrap_editor_line(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if line.is_empty() {
        return vec![String::new()];
    }

    let mut rows = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for ch in line.chars() {
        let ch_width = ch.width().unwrap_or(0).max(1);
        if current_width > 0 && current_width + ch_width > width {
            rows.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width += ch_width;
    }

    rows.push(current);
    rows
}

#[derive(Clone, Copy)]
struct EditorCursorPosition {
    row: u16,
    col: u16,
}

fn editor_cursor_position(es: &EditorState, width: usize) -> EditorCursorPosition {
    let width = width.max(1);
    let target_row = es.cursor_row.min(es.lines.len().saturating_sub(1));
    let mut visual_row = 0usize;

    for (row, line) in es.lines.iter().enumerate() {
        if row == target_row {
            let cursor = cursor_in_line(line, es.cursor_col, width);
            return EditorCursorPosition {
                row: visual_row.saturating_add(cursor.row as usize) as u16,
                col: cursor.col,
            };
        }
        visual_row = visual_row.saturating_add(wrap_editor_line(line, width).len());
    }

    EditorCursorPosition { row: 0, col: 0 }
}

fn cursor_in_line(line: &str, cursor_col: usize, width: usize) -> EditorCursorPosition {
    let width = width.max(1);
    let char_count = line.chars().count();
    let cursor_col = cursor_col.min(char_count);
    let mut row = 0usize;
    let mut col = 0usize;

    for ch in line.chars().take(cursor_col) {
        let ch_width = ch.width().unwrap_or(0).max(1);
        if col > 0 && col + ch_width > width {
            row += 1;
            col = 0;
        }
        col += ch_width;
    }

    if col >= width {
        row += 1;
        col = 0;
    }

    EditorCursorPosition {
        row: row as u16,
        col: col as u16,
    }
}

fn slash_popup_lines(es: &EditorState) -> Vec<Line<'static>> {
    let Some(popup) = &es.slash_popup else {
        return Vec::new();
    };

    let mut lines = Vec::new();
    let max_items = 8usize;
    let start = popup.selected.saturating_sub(max_items.saturating_sub(1));
    for (visible_idx, item_idx) in popup.items.iter().enumerate().skip(start).take(max_items) {
        let selected = visible_idx == popup.selected;
        let (name, description) = match popup.kind {
            PopupKind::Command => {
                let cmd = &SLASH_COMMANDS[*item_idx];
                (cmd.name.to_string(), cmd.description.to_string())
            }
            PopupKind::Subcommand(cmd_idx) => {
                let cmd = &SLASH_COMMANDS[cmd_idx];
                let sub = &cmd.subcommands[*item_idx];
                (
                    format!("{} {}", cmd.name, sub.name),
                    sub.description.to_string(),
                )
            }
        };
        lines.push(completion_line(selected, &name, &description));
    }

    if start + max_items < popup.items.len() {
        lines.push(Line::from(vec![Span::styled(
            format!("  +{} more", popup.items.len() - start - max_items),
            Style::default().fg(Color::DarkGray),
        )]));
    }

    lines
}

fn completion_line(selected: bool, name: &str, description: &str) -> Line<'static> {
    let marker = if selected { ">" } else { " " };
    let base = if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Cyan)
    };
    let desc = if selected {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Line::from(vec![
        Span::styled(format!("{marker} "), base),
        Span::styled(name.to_string(), base),
        Span::styled("  ", desc),
        Span::styled(description.to_string(), desc),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_visual_rows_soft_wrap_long_input() {
        let mut editor = EditorState::default();
        editor.set_text("abcdef");

        let rows = editor_visual_rows(&editor, 3);

        assert_eq!(
            rows,
            vec!["abc".to_string(), "def".to_string(), String::new()]
        );
    }

    #[test]
    fn editor_cursor_tracks_soft_wraps() {
        let mut editor = EditorState::default();
        editor.set_text("abcdef");

        let cursor = editor_cursor_position(&editor, 3);

        assert_eq!(cursor.row, 2);
        assert_eq!(cursor.col, 0);
    }

    #[test]
    fn editor_cursor_tracks_multiline_soft_wraps() {
        let mut editor = EditorState::default();
        editor.set_text("abcd\nef");

        let cursor = editor_cursor_position(&editor, 3);

        assert_eq!(cursor.row, 2);
        assert_eq!(cursor.col, 2);
    }
}

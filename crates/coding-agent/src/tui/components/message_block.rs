//! MessageBlock — renders a transcript entry into iodilos text rows.

use iodilos::prelude::{Color, Modifier, TextRow, TextSegment};
use iodilos::text::SpanStyle;
use iodilos_md::{MarkdownTheme, StreamingParser, markdown_surface};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::state::{ConversationEntry, EntryKind};

const THINKING_BLOCK_MAX_ROWS: usize = 5;
const THINKING_BLOCK_MIN_WIDTH: usize = 8;

pub fn render_entry(
    entry: &ConversationEntry,
    render_width: usize,
    parser: Option<&mut StreamingParser>,
) -> Vec<TextRow> {
    let width = render_width.max(1);
    match &entry.kind {
        EntryKind::User(text) => render_plain(
            "> ",
            Color::Rgb {
                r: 140,
                g: 190,
                b: 255,
            },
            text,
        ),
        EntryKind::Assistant(text) => render_markdown(
            "* ",
            Color::Rgb {
                r: 118,
                g: 205,
                b: 255,
            },
            text,
            width,
            parser,
        ),
        EntryKind::Thinking(text) => render_thinking_block(text, width),
        EntryKind::Tool(text) => render_plain(
            "tool ",
            Color::Rgb {
                r: 160,
                g: 190,
                b: 200,
            },
            text,
        ),
        EntryKind::Error(text) => render_plain("error ", Color::Red, text),
        EntryKind::System(text) => render_markdown("info ", Color::DarkGrey, text, width, None),
    }
}

fn render_plain(prefix: &'static str, color: Color, body: &str) -> Vec<TextRow> {
    let style = fg(color);
    let mut rows = Vec::new();
    for (i, line) in body.lines().enumerate() {
        if i == 0 {
            rows.push(TextRow::from_segments(vec![
                TextSegment::styled(prefix, style),
                TextSegment::styled(line.to_string(), style),
            ]));
        } else {
            rows.push(TextRow::from(TextSegment::styled(line.to_string(), style)));
        }
    }
    if rows.is_empty() {
        rows.push(TextRow::from(TextSegment::styled(prefix, style)));
    }
    rows
}

fn render_thinking_block(body: &str, render_width: usize) -> Vec<TextRow> {
    if render_width < THINKING_BLOCK_MIN_WIDTH {
        return render_plain("thinking ", Color::DarkGrey, body);
    }

    let block_width = render_width;
    let inner_width = block_width.saturating_sub(4).max(1);
    let content_rows = THINKING_BLOCK_MAX_ROWS.saturating_sub(2).max(1);
    let wrapped = wrap_plain_text(body, inner_width);
    let start = wrapped.len().saturating_sub(content_rows);
    let visible = &wrapped[start..];
    let border = fg(Color::DarkGrey);
    let label = fg(Color::Yellow);
    let text = fg(Color::Grey);

    let mut rows = Vec::with_capacity(visible.len() + 2);
    rows.push(thinking_top_row(block_width, border, label));
    for line in visible {
        rows.push(TextRow::from_segments(vec![
            TextSegment::styled("│ ", border),
            TextSegment::styled(pad_to_width(line, inner_width), text),
            TextSegment::styled(" │", border),
        ]));
    }
    rows.push(TextRow::from(TextSegment::styled(
        format!("╰{}╯", "─".repeat(block_width.saturating_sub(2))),
        border,
    )));
    rows
}

fn thinking_top_row(width: usize, border: SpanStyle, label: SpanStyle) -> TextRow {
    let title = " thinking ";
    let prefix = "╭─";
    let suffix = "╮";
    let title_width = display_width(title);
    let prefix_width = display_width(prefix);
    let suffix_width = display_width(suffix);

    if width >= prefix_width + title_width + suffix_width {
        let fill = width - prefix_width - title_width - suffix_width;
        return TextRow::from_segments(vec![
            TextSegment::styled(prefix, border),
            TextSegment::styled(title, label),
            TextSegment::styled("─".repeat(fill), border),
            TextSegment::styled(suffix, border),
        ]);
    }

    TextRow::from(TextSegment::styled(
        format!("╭{}╮", "─".repeat(width.saturating_sub(2))),
        border,
    ))
}

fn wrap_plain_text(body: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();

    for source_line in body.split('\n') {
        if source_line.is_empty() {
            rows.push(String::new());
            continue;
        }

        let mut current = String::new();
        let mut col = 0usize;
        for ch in source_line.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0).max(1);
            if col + ch_width > width && !current.is_empty() {
                rows.push(std::mem::take(&mut current));
                col = 0;
            }
            current.push(ch);
            col += ch_width;
        }
        rows.push(current);
    }

    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn pad_to_width(text: &str, width: usize) -> String {
    let used = display_width(text);
    if used >= width {
        return text.to_string();
    }
    format!("{text}{}", " ".repeat(width - used))
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn render_markdown(
    prefix: &'static str,
    color: Color,
    body: &str,
    render_width: usize,
    parser: Option<&mut StreamingParser>,
) -> Vec<TextRow> {
    let theme = MarkdownTheme::default();
    let surface = match parser {
        Some(parser) => parser.feed_to_surface(body, render_width, &theme),
        None => markdown_surface(body, render_width, &theme),
    };
    let mut rows = surface.rows().to_vec();
    if rows.is_empty() {
        return vec![TextRow::from(TextSegment::styled(prefix, fg(color)))];
    }
    rows[0]
        .segments
        .insert(0, TextSegment::styled(prefix, fg(color)));
    rows
}

fn fg(color: Color) -> SpanStyle {
    SpanStyle {
        fg: Some(color),
        ..SpanStyle::default()
    }
}

#[allow(dead_code)]
fn bold(color: Color) -> SpanStyle {
    SpanStyle {
        fg: Some(color),
        add_modifier: Modifier::BOLD,
        ..SpanStyle::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: EntryKind) -> ConversationEntry {
        ConversationEntry { kind }
    }

    fn row_text(row: &TextRow) -> String {
        row.segments
            .iter()
            .map(|segment| segment.content.as_ref())
            .collect()
    }

    #[test]
    fn thinking_entry_renders_as_bounded_block() {
        let rows = render_entry(
            &entry(EntryKind::Thinking(
                "line 1\nline 2\nline 3\nline 4\nline 5\nline 6".to_string(),
            )),
            24,
            None,
        );

        assert_eq!(rows.len(), 5);
        assert!(row_text(&rows[0]).starts_with("╭─ thinking "));
        assert!(row_text(&rows[1]).contains("line 4"));
        assert!(row_text(&rows[2]).contains("line 5"));
        assert!(row_text(&rows[3]).contains("line 6"));
        assert!(row_text(&rows[4]).starts_with("╰"));
    }

    #[test]
    fn thinking_entry_wraps_plain_text_without_markdown() {
        let rows = render_entry(
            &entry(EntryKind::Thinking("# heading is plain".to_string())),
            14,
            None,
        );

        let body = rows.iter().map(row_text).collect::<Vec<_>>().join("\n");
        assert!(body.contains("# heading"));
        assert!(rows.len() <= THINKING_BLOCK_MAX_ROWS);
    }
}

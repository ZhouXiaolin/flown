//! MessageBlock — renders a single transcript entry.

use std::sync::OnceLock;

use iodilos::prelude::{Color, Style};
use iodilos::prelude::{Line, Span};
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

use crate::tui::markdown::parse_markdown_with_width;
use crate::tui::state::{ConversationEntry, EntryKind};
use crate::tui::theme::{app_theme, current_syntect_theme};

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

/// Build the lines for one entry. Assistant/system text uses the preserved
/// markdown renderer; operational rows stay compact and plain.
///
/// `render_width` is the usable column count for the markdown body (excluding
/// the `ScrollableViewport` chrome). Passing the real terminal-derived width
/// keeps the rendered line widths aligned with what `Paragraph::wrap` draws, so
/// the `RichText` node never overflows and content scrolls instead of being
/// clipped.
pub fn render_entry(entry: &ConversationEntry, render_width: usize) -> Vec<Line<'static>> {
    let render_width = render_width.max(1);
    match &entry.kind {
        EntryKind::User(text) => render_plain("> ", Color::Rgb(140, 190, 255), text),
        EntryKind::Assistant(text) => {
            render_markdown("● ", Color::Rgb(118, 205, 255), text, render_width)
        }
        EntryKind::Thinking(text) => render_plain("💭 ", Color::DarkGray, text),
        EntryKind::Tool(text) => render_plain("🔧 ", Color::Rgb(160, 190, 200), text),
        EntryKind::Error(text) => render_plain("✗ ", Color::Red, text),
        EntryKind::System(text) => render_markdown("ℹ ", Color::DarkGray, text, render_width),
    }
}

fn render_plain(prefix: &'static str, color: Color, body: &str) -> Vec<Line<'static>> {
    let style = Style::default().fg(color);
    let mut lines = Vec::new();
    for (i, body_line) in body.lines().enumerate() {
        if i == 0 {
            lines.push(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(body_line.to_string(), style),
            ]));
        } else {
            lines.push(Line::from(vec![Span::styled(body_line.to_string(), style)]));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(vec![Span::styled(prefix, style)]));
    }
    lines
}

fn render_markdown(
    prefix: &'static str,
    color: Color,
    body: &str,
    render_width: usize,
) -> Vec<Line<'static>> {
    let ss = SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines);
    let themes = THEME_SET.get_or_init(ThemeSet::load_defaults);
    let theme = current_syntect_theme(themes);
    let app_theme = app_theme();
    let (mut lines, _, _) =
        parse_markdown_with_width(body, ss, theme, render_width, &app_theme.markdown, false);
    if lines.is_empty() {
        return vec![Line::from(vec![Span::styled(
            prefix,
            Style::default().fg(color),
        )])];
    }

    lines[0]
        .spans
        .insert(0, Span::styled(prefix, Style::default().fg(color)));
    lines
}

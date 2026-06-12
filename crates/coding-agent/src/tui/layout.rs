use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::editor::Editor;
use super::status_line::StatusLine;
use super::transcript::Transcript;

/// Main application layout: transcript + status line + editor + hint bar
pub fn render_layout(
    f: &mut Frame,
    transcript: &mut Transcript,
    status_line: &StatusLine,
    editor: &Editor,
    agent_busy: bool,
) {
    let area = f.area();

    // Main vertical layout:
    // [transcript (fill)]
    // [status line (1 row)]
    // [editor (3 rows)]
    // [hint bar (1 row)]
    let chunks = Layout::vertical([
        Constraint::Min(10),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);

    // Transcript panel
    transcript.render(f, chunks[0]);

    // Status line (as the border between transcript and editor)
    status_line.render(f, chunks[1]);

    // Editor input box
    editor.render(f, chunks[2]);

    // Hint bar
    render_hint_bar(f, chunks[3], agent_busy);
}

fn render_hint_bar(f: &mut Frame, area: Rect, agent_busy: bool) {
    let hints = if agent_busy {
        Line::from(vec![
            Span::styled("  ⟳ ", Style::default().fg(Color::Yellow)),
            Span::styled("thinking…", Style::default().fg(Color::Yellow)),
            Span::styled("  Esc ", Style::default().fg(Color::DarkGray)),
            Span::styled("abort", Style::default().fg(Color::Red)),
        ])
    } else {
        Line::from(vec![
            Span::styled("  ⏎ ", Style::default().fg(Color::DarkGray)),
            Span::styled("send", Style::default().fg(Color::Green)),
            Span::styled("  ⇧⏎ ", Style::default().fg(Color::DarkGray)),
            Span::styled("newline", Style::default().fg(Color::Green)),
            Span::styled("  / ", Style::default().fg(Color::DarkGray)),
            Span::styled("commands", Style::default().fg(Color::Green)),
            Span::styled("  Tab ", Style::default().fg(Color::DarkGray)),
            Span::styled("accept", Style::default().fg(Color::Green)),
            Span::styled("  Esc ", Style::default().fg(Color::DarkGray)),
            Span::styled("cancel", Style::default().fg(Color::Green)),
        ])
    };
    f.render_widget(Paragraph::new(hints), area);
}

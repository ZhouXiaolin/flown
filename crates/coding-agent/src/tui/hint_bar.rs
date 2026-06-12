//! HintBar component — bottom bar showing keyboard shortcuts.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::component::Component;

/// Bottom hint bar showing keyboard shortcuts.
pub struct HintBar {
    pub busy: bool,
}

impl HintBar {
    pub fn new() -> Self {
        Self { busy: false }
    }
}

impl Component for HintBar {
    fn render(&mut self, _width: u16) -> Vec<Line<'static>> {
        let hints = if self.busy {
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
        vec![hints]
    }
}

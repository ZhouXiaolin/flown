use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Renders inline tool execution status in the transcript.
pub struct ToolView;

impl ToolView {
    /// Render a running tool indicator.
    pub fn render_running(name: &str) -> Line<'static> {
        Line::from(vec![
            Span::styled("🔧 ", Style::default().fg(Color::Magenta)),
            Span::styled(
                format!("[{name}]"),
                Style::default().fg(Color::Magenta),
            ),
            Span::styled(" ⟳ running...", Style::default().fg(Color::DarkGray)),
        ])
    }

    /// Render a completed tool result summary.
    pub fn render_completed(name: &str, duration_ms: u64, success: bool) -> Line<'static> {
        let (icon, color) = if success {
            ("✓", Color::Green)
        } else {
            ("✗", Color::Red)
        };
        let duration_str = if duration_ms >= 1000 {
            format!("{:.1}s", duration_ms as f64 / 1000.0)
        } else {
            format!("{duration_ms}ms")
        };
        Line::from(vec![
            Span::styled("🔧 ", Style::default().fg(color)),
            Span::styled(format!("[{name}]"), Style::default().fg(color)),
            Span::styled(format!(" {icon} ({duration_str})"), Style::default().fg(Color::DarkGray)),
        ])
    }
}

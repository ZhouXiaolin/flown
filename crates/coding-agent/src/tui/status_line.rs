use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

/// Status line segments configuration.
pub struct StatusLine {
    pub model: String,
    pub provider: String,
    pub cwd: String,
    pub git_branch: Option<String>,
    pub git_dirty: bool,
    pub context_pct: f64,
    pub context_total: String,
    pub session_name: Option<String>,
}

impl StatusLine {
    pub fn new() -> Self {
        Self {
            model: String::new(),
            provider: String::new(),
            cwd: String::new(),
            git_branch: None,
            git_dirty: false,
            context_pct: 0.0,
            context_total: String::new(),
            session_name: None,
        }
    }

    /// Render the status line as the top border of the editor.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let width = area.width as usize;
        let mut spans = Vec::new();

        // Left segments
        // App icon
        spans.push(Span::styled(
            " ✦ ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

        // Model + provider
        spans.push(Span::styled(
            format!("{} ", self.model),
            Style::default().fg(Color::White),
        ));
        if !self.provider.is_empty() {
            spans.push(Span::styled(
                format!("·{} ", self.provider),
                Style::default().fg(Color::DarkGray),
            ));
        }

        // Working directory
        spans.push(Span::styled(
            format!("📁 {} ", self.cwd),
            Style::default().fg(Color::Blue),
        ));

        // Git branch
        if let Some(branch) = &self.git_branch {
            let git_color = if self.git_dirty {
                Color::Yellow
            } else {
                Color::Green
            };
            spans.push(Span::styled(
                format!("{} ", branch),
                Style::default().fg(git_color),
            ));
        }

        // Context percentage
        let ctx_color = if self.context_pct >= 0.9 {
            Color::Red
        } else if self.context_pct >= 0.7 {
            Color::Magenta
        } else if self.context_pct >= 0.5 {
            Color::Yellow
        } else {
            Color::Green
        };
        spans.push(Span::styled(
            format!("{:.1}%/{}", self.context_pct * 100.0, self.context_total),
            Style::default().fg(ctx_color),
        ));

        // Calculate left content width
        let left_text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        let left_width = unicode_width::UnicodeWidthStr::width(left_text.as_str());

        // Fill with horizontal line
        let fill_width = width.saturating_sub(left_width + 2); // 2 for borders
        spans.push(Span::styled(
            "─".repeat(fill_width),
            Style::default().fg(Color::DarkGray),
        ));

        // Right segment: session name
        if let Some(name) = &self.session_name {
            spans.push(Span::styled(
                format!(" {} ", name),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::ITALIC),
            ));
        }

        let line = Line::from(spans);
        let paragraph = ratatui::widgets::Paragraph::new(line);
        f.render_widget(paragraph, area);
    }
}

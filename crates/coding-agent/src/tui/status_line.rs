use std::time::Instant;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::component::Component;

/// Animated snake spinner frames for busy state
const BUSY_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];

/// Animation interval in milliseconds (500ms = 0.5s per frame)
const ANIMATION_INTERVAL_MS: u128 = 500;

/// Status line segments configuration.
///
/// Layout: pi > model thinking-level > project path > git branch > cache
pub struct StatusLine {
    pub model: String,
    pub provider: String,
    pub thinking_level: String,
    pub cwd: String,
    pub git_branch: Option<String>,
    pub git_dirty: bool,
    pub context_pct: f64,
    pub context_total: String,
    pub session_name: Option<String>,
    pub cache_read: u64,
    pub cache_write: u64,
    pub busy: bool,
    frame: usize,
    last_tick: Instant,
}

impl StatusLine {
    pub fn new() -> Self {
        Self {
            model: String::new(),
            provider: String::new(),
            thinking_level: String::new(),
            cwd: String::new(),
            git_branch: None,
            git_dirty: false,
            context_pct: 0.0,
            context_total: String::new(),
            session_name: None,
            cache_read: 0,
            cache_write: 0,
            busy: false,
            frame: 0,
            last_tick: Instant::now(),
        }
    }

    /// Advance animation frame if enough time has passed.
    pub fn tick(&mut self) {
        if self.busy {
            let now = Instant::now();
            if now.duration_since(self.last_tick).as_millis() >= ANIMATION_INTERVAL_MS {
                self.frame = (self.frame + 1) % BUSY_FRAMES.len();
                self.last_tick = now;
            }
        }
    }

    /// Build the status line as a single `Line`.
    fn build_line(&self, width: usize) -> Line<'static> {
        let mut spans = Vec::new();

        // Separator style: " · " (dot separator like oh-my-pi)
        let sep = " · ";
        let sep_style = Style::default().fg(Color::Rgb(80, 80, 90));

        // ── Left segments ──────────────────────────────────────────

        // 1. Pi icon (animated when busy)
        let pi_icon = if self.busy {
            BUSY_FRAMES[self.frame]
        } else {
            "●"
        };
        let pi_color = if self.busy {
            Color::Yellow
        } else {
            Color::Cyan
        };
        spans.push(Span::styled(
            format!(" {pi_icon} "),
            Style::default()
                .fg(pi_color)
                .add_modifier(Modifier::BOLD),
        ));

        // 2. Model + thinking level
        if !self.model.is_empty() {
            let model_text = if self.model.starts_with("Claude ") {
                self.model[7..].to_string()
            } else {
                self.model.clone()
            };
            spans.push(Span::styled(
                model_text,
                Style::default().fg(Color::White),
            ));
            if !self.thinking_level.is_empty() {
                spans.push(Span::styled(sep, sep_style));
                let thinking_color = if self.thinking_level == "off" {
                    Color::Rgb(100, 100, 110)
                } else {
                    Color::Rgb(180, 140, 255)
                };
                spans.push(Span::styled(
                    self.thinking_level.clone(),
                    Style::default().fg(thinking_color),
                ));
            }
        }

        // 3. Project path
        if !self.cwd.is_empty() {
            spans.push(Span::styled(sep, sep_style));
            let display_path = shorten_path(&self.cwd);
            spans.push(Span::styled(
                display_path,
                Style::default().fg(Color::Rgb(100, 160, 220)),
            ));
        }

        // 4. Git branch
        if let Some(branch) = &self.git_branch {
            spans.push(Span::styled(sep, sep_style));
            let git_color = if self.git_dirty {
                Color::Rgb(220, 180, 60)
            } else {
                Color::Rgb(100, 200, 120)
            };
            spans.push(Span::styled(
                branch.clone(),
                Style::default().fg(git_color),
            ));
        }

        // 5. Cache / Context
        spans.push(Span::styled(sep, sep_style));
        let ctx_color = if self.context_pct >= 0.9 {
            Color::Red
        } else if self.context_pct >= 0.7 {
            Color::Magenta
        } else if self.context_pct >= 0.5 {
            Color::Yellow
        } else {
            Color::Rgb(100, 200, 120)
        };
        let ctx_text = format!("{:.0}%", self.context_pct * 100.0);
        spans.push(Span::styled(ctx_text, Style::default().fg(ctx_color)));

        if self.cache_read > 0 || self.cache_write > 0 {
            spans.push(Span::styled(
                format!(
                    " (↓{}/↑{})",
                    format_tokens(self.cache_read),
                    format_tokens(self.cache_write)
                ),
                Style::default().fg(Color::Rgb(80, 80, 90)),
            ));
        }

        // ── Fill and right segments ────────────────────────────────

        let left_text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        let left_width = unicode_width::UnicodeWidthStr::width(left_text.as_str());

        let fill_width = width.saturating_sub(left_width + 2);
        if fill_width > 0 {
            spans.push(Span::styled(
                "─".repeat(fill_width),
                Style::default().fg(Color::Rgb(50, 50, 60)),
            ));
        }

        if let Some(name) = &self.session_name {
            spans.push(Span::styled(
                format!(" {} ", name),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::ITALIC),
            ));
        }

        Line::from(spans)
    }

    /// Render the status line to a ratatui `Frame`.
    pub fn render_frame(&self, f: &mut Frame, area: Rect) {
        let line = self.build_line(area.width as usize);
        f.render_widget(Paragraph::new(line), area);
    }
}

impl Component for StatusLine {
    fn render(&mut self, width: u16) -> Vec<Line<'static>> {
        vec![self.build_line(width as usize)]
    }
}

/// Shorten a path by replacing $HOME with ~
fn shorten_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if path.starts_with(home_str.as_ref()) {
            return format!("~{}", &path[home_str.len()..]);
        }
    }
    path.to_string()
}

/// Format token count for display (e.g., 1234567 -> "1.2M")
fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        format!("{}", tokens)
    }
}

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

/// A recent session entry for the welcome screen.
pub struct RecentSession {
    pub id: String,
    pub name: Option<String>,
    pub created_at: String,
    pub path: String,
}

/// Welcome screen shown on startup.
pub struct Welcome {
    pub version: String,
    pub model: String,
    pub provider: String,
    pub recent_sessions: Vec<RecentSession>,
}

impl Welcome {
    pub fn render(&self, f: &mut Frame, area: Rect) {
        // Center the welcome box
        let outer = centered_rect(60, 50, area);
        f.render_widget(Clear, outer);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(format!(" flown v{} ", self.version));

        let inner = block.inner(outer);
        f.render_widget(block, outer);

        let chunks = Layout::horizontal([
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .split(inner);

        // Left column: logo + model info
        let left_lines = vec![
            Line::default(),
            Line::from(Span::styled(
                "  Welcome!",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::default(),
            Line::from(Span::styled(
                "  ✦ Terminal Coding Agent",
                Style::default().fg(Color::White),
            )),
            Line::default(),
            Line::from(vec![
                Span::styled("  Model: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&self.model, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  Provider: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&self.provider, Style::default().fg(Color::White)),
            ]),
        ];
        f.render_widget(Paragraph::new(left_lines), chunks[0]);

        // Right column: tips + recent sessions
        let mut right_lines = vec![
            Line::default(),
            Line::from(Span::styled(
                "  Tips",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "  ? for help",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  / for commands",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  Esc to cancel",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        if !self.recent_sessions.is_empty() {
            right_lines.push(Line::default());
            right_lines.push(Line::from(Span::styled(
                "  Recent Sessions",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            for session in self.recent_sessions.iter().take(5) {
                let label = session
                    .name
                    .as_deref()
                    .unwrap_or(&session.id[..8.min(session.id.len())]);
                let time = &session.created_at[..19.min(session.created_at.len())];
                right_lines.push(Line::from(vec![
                    Span::styled(
                        format!("  • {label}"),
                        Style::default().fg(Color::White),
                    ),
                    Span::styled(
                        format!("  {time}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }

        f.render_widget(Paragraph::new(right_lines), chunks[1]);

        // Tip line below
        let tip_area = Rect {
            x: outer.x,
            y: outer.y + outer.height,
            width: outer.width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "  Tip: ",
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::ITALIC),
                ),
                Span::styled(
                    "Type your prompt and press Enter to start",
                    Style::default().fg(Color::Blue),
                ),
            ])),
            tip_area,
        );
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(r);

    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup_layout[1])[1]
}

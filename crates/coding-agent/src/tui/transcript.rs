use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation};
use ratatui::Frame;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthStr;

use super::markdown::{parse_markdown_with_width, width::line_plain_text};
use super::theme::{OCEAN_DARK_MARKDOWN, current_syntect_theme};

/// Message kind determines styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    User,
    Assistant,
    Thinking,
    Tool,
    Error,
    System,
}

/// A single entry in the transcript.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    pub kind: MessageKind,
    pub body: String,
}

impl TranscriptEntry {
    pub fn user(text: impl Into<String>) -> Self {
        Self { kind: MessageKind::User, body: text.into() }
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self { kind: MessageKind::Assistant, body: text.into() }
    }
    pub fn thinking(text: impl Into<String>) -> Self {
        Self { kind: MessageKind::Thinking, body: text.into() }
    }
    pub fn tool(text: impl Into<String>) -> Self {
        Self { kind: MessageKind::Tool, body: text.into() }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self { kind: MessageKind::Error, body: text.into() }
    }
    pub fn system(text: impl Into<String>) -> Self {
        Self { kind: MessageKind::System, body: text.into() }
    }
}

/// Cached rendered lines for a single entry, with a content hash for invalidation.
struct CachedEntry {
    body_hash: u64,
    kind: MessageKind,
    lines: Vec<Line<'static>>,
}

/// The conversation transcript panel.
pub struct Transcript {
    entries: Vec<TranscriptEntry>,
    scroll_offset: usize,
    total_lines: usize,
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    /// Per-entry rendered line cache. Index matches `entries`.
    cache: Vec<CachedEntry>,
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            scroll_offset: 0,
            total_lines: 0,
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            cache: Vec::new(),
        }
    }

    pub fn push(&mut self, entry: TranscriptEntry) {
        self.entries.push(entry);
        self.scroll_to_bottom();
    }

    /// Get a mutable reference to the last entry (for streaming updates).
    pub fn last_mut(&mut self) -> Option<&mut TranscriptEntry> {
        self.entries.last_mut()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.scroll_offset = 0;
        self.total_lines = 0;
        self.cache.clear();
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = usize::MAX;
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL);

        let inner = block.inner(area);
        let viewport_height = inner.height as usize;
        let render_width = inner.width as usize;

        let all_lines = self.build_lines(render_width);
        self.total_lines = all_lines.len();

        if self.scroll_offset == usize::MAX || self.scroll_offset + viewport_height >= self.total_lines {
            self.scroll_offset = self.total_lines.saturating_sub(viewport_height);
        }

        let visible: Vec<Line> = all_lines
            .into_iter()
            .skip(self.scroll_offset)
            .take(viewport_height)
            .collect();

        let paragraph = Paragraph::new(visible).block(block);
        f.render_widget(paragraph, area);

        if self.total_lines > viewport_height {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_symbol("█")
                .track_symbol(Some("░"));
            let mut state = ratatui::widgets::ScrollbarState::new(self.total_lines)
                .position(self.scroll_offset);
            f.render_stateful_widget(scrollbar, area, &mut state);
        }
    }

    fn build_lines(&mut self, render_width: usize) -> Vec<Line<'static>> {
        let syntect_theme = current_syntect_theme(&self.theme_set);
        let md_theme = &OCEAN_DARK_MARKDOWN;

        // Sync cache length to entries length
        self.cache.truncate(self.entries.len());
        while self.cache.len() < self.entries.len() {
            // Placeholder; will be filled below
            self.cache.push(CachedEntry {
                body_hash: 0,
                kind: MessageKind::System,
                lines: Vec::new(),
            });
        }

        // Invalidate cache for entries whose body or kind changed.
        // The last entry is always invalidated (streaming updates).
        let last_idx = self.entries.len().saturating_sub(1);
        for (i, entry) in self.entries.iter().enumerate() {
            let hash = hash_content(&entry.body);
            let dirty = i == last_idx
                || self.cache[i].body_hash != hash
                || self.cache[i].kind != entry.kind;
            if dirty {
                let lines = render_entry(entry, &self.syntax_set, syntect_theme, md_theme, render_width);
                self.cache[i] = CachedEntry {
                    body_hash: hash,
                    kind: entry.kind,
                    lines,
                };
            }
        }

        // Concatenate cached lines with inter-entry gaps
        let estimated: usize = self.cache.iter().map(|c| c.lines.len() + 1).sum();
        let mut all_lines = Vec::with_capacity(estimated);
        for (i, cached) in self.cache.iter().enumerate() {
            if i > 0 {
                all_lines.push(Line::from(""));
            }
            all_lines.extend(cached.lines.iter().cloned());
        }
        all_lines
    }
}

/// Simple FNV-1a hash for cache invalidation.
fn hash_content(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Style for a message kind.
fn message_style(kind: MessageKind) -> Style {
    match kind {
        MessageKind::User => Style::default().fg(Color::Rgb(140, 190, 255)),    // light blue
        MessageKind::Assistant => Style::default().fg(Color::Rgb(214, 218, 226)), // light gray
        MessageKind::Thinking => Style::default().fg(Color::DarkGray),
        MessageKind::Tool => Style::default().fg(Color::Rgb(160, 190, 200)),     // teal
        MessageKind::Error => Style::default().fg(Color::Red),
        MessageKind::System => Style::default().fg(Color::DarkGray),
    }
}

/// Render a single transcript entry into lines.
fn render_entry(
    entry: &TranscriptEntry,
    ss: &SyntaxSet,
    syntect_theme: &syntect::highlighting::Theme,
    md_theme: &crate::tui::theme::MarkdownTheme,
    render_width: usize,
) -> Vec<Line<'static>> {
    if entry.kind == MessageKind::Thinking {
        return render_thinking_block(&entry.body, render_width);
    }

    let style = message_style(entry.kind);
    let bullet = Span::styled("● ", style.add_modifier(Modifier::BOLD));
    let first_prefix_width = 2; // "● " = 2 cells
    let body_width = render_width.saturating_sub(first_prefix_width).max(8);

    // Parse body as markdown for assistant/tool messages, plain for user/error/system
    let (mut rendered, _, _) = parse_markdown_with_width(
        &entry.body,
        ss,
        syntect_theme,
        body_width,
        md_theme,
        false,
    );
    while rendered
        .last()
        .is_some_and(|line| line_plain_text(line).trim().is_empty())
    {
        rendered.pop();
    }
    if rendered.is_empty() {
        rendered.push(Line::from(""));
    }

    let mut out = Vec::new();
    for (line_idx, line) in rendered.into_iter().enumerate() {
        let mut spans = if line_idx == 0 {
            vec![bullet.clone()]
        } else {
            vec![Span::raw("  ")]
        };

        // Apply message-level styling
        for span in line.spans.into_iter() {
            let patched = if span.style.fg.is_some() || span.style.bg.is_some() {
                span.style
            } else {
                span.style.patch(style)
            };
            spans.push(Span::styled(span.content, patched));
        }

        // Style tool call lines (first line only)
        if entry.kind == MessageKind::Tool && line_idx == 0 {
            style_tool_call_line(&mut spans);
        }

        out.push(Line::from(spans));
    }
    out
}

/// Style the first line of a tool call message.
/// Detects "Read", "Write", "Edit", "Bash" and applies blue color to the tool name.
/// For Edit, applies green to +N and red to -M.
fn style_tool_call_line(spans: &mut Vec<Span<'static>>) {
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();

    let tool = ["Write", "Read", "Edit", "Bash"]
        .iter()
        .find(|tool| {
            let prefix = format!("● {tool}");
            text.starts_with(&format!("{prefix} "))
                || text.starts_with(&format!("{prefix}("))
                || text == *prefix
        });

    let Some(tool) = tool else { return };

    let tool_style = Style::default().fg(Color::Rgb(140, 190, 255));
    style_text_range(spans, 2..2 + tool.len(), tool_style);

    // For Edit: color +N green and -M red
    if *tool == "Edit" {
        if let Some(open) = text.find("(+") {
            let added_style = Style::default().fg(Color::Rgb(95, 200, 148));
            let removed_style = Style::default().fg(Color::Rgb(218, 95, 95));
            if let Some(space) = text[open + 1..].find(' ') {
                let added_start = char_count(&text, open + 1);
                let added_end = char_count(&text, open + 1 + space);
                style_text_range(spans, added_start..added_end, added_style);
                if let Some(close) = text[open + 1 + space + 1..].find(')') {
                    let removed_byte_start = open + 1 + space + 1;
                    let removed_start = char_count(&text, removed_byte_start);
                    let removed_end = char_count(&text, removed_byte_start + close);
                    style_text_range(spans, removed_start..removed_end, removed_style);
                }
            }
        }
    }
}

fn char_count(text: &str, byte_index: usize) -> usize {
    text[..byte_index].chars().count()
}

/// Apply a style to a character range within a span list.
fn style_text_range(
    spans: &mut Vec<Span<'static>>,
    range: std::ops::Range<usize>,
    style: Style,
) {
    let mut offset = 0usize;
    let mut styled = Vec::with_capacity(spans.len() + 2);
    for span in spans.drain(..) {
        let len = span.content.chars().count();
        let span_start = offset;
        let span_end = offset + len;
        if range.start >= span_end || range.end <= span_start {
            offset = span_end;
            styled.push(span);
            continue;
        }
        let local_start = range.start.saturating_sub(span_start);
        let local_end = (range.end - span_start).min(len);
        let chars: Vec<char> = span.content.chars().collect();
        if local_start > 0 {
            styled.push(Span::styled(
                chars[..local_start].iter().collect::<String>(),
                span.style,
            ));
        }
        styled.push(Span::styled(
            chars[local_start..local_end].iter().collect::<String>(),
            style,
        ));
        if local_end < chars.len() {
            styled.push(Span::styled(
                chars[local_end..].iter().collect::<String>(),
                span.style,
            ));
        }
        offset = span_end;
    }
    *spans = styled;
}

const MAX_THINKING_VISIBLE_LINES: usize = 5;

fn render_thinking_block(body: &str, render_width: usize) -> Vec<Line<'static>> {
    let inner_width = render_width.saturating_sub(2);
    if inner_width == 0 {
        return vec![Line::from("")];
    }

    let mut all_lines: Vec<String> = Vec::new();
    for logical_line in body.lines() {
        for visual_line in wrap_plain_line(logical_line, inner_width.saturating_sub(2)) {
            all_lines.push(visual_line.to_string());
        }
    }
    if body.ends_with('\n') {
        all_lines.push(String::new());
    }
    if all_lines.is_empty() {
        all_lines.push(String::new());
    }

    let line_count = all_lines.len();
    let has_overflow = line_count > MAX_THINKING_VISIBLE_LINES;
    let visible: Vec<String> = if has_overflow {
        all_lines[line_count - MAX_THINKING_VISIBLE_LINES..].to_vec()
    } else {
        all_lines
    };

    let border = Style::default()
        .fg(Color::Rgb(70, 75, 85))
        .bg(Color::Rgb(30, 30, 36));
    let text_style = Style::default()
        .fg(Color::Rgb(160, 165, 175))
        .bg(Color::Rgb(30, 30, 36));
    let title_style = Style::default()
        .fg(Color::Rgb(120, 130, 150))
        .add_modifier(Modifier::ITALIC)
        .bg(Color::Rgb(30, 30, 36));
    let indicator_style = Style::default()
        .fg(Color::Rgb(100, 100, 110))
        .add_modifier(Modifier::ITALIC)
        .bg(Color::Rgb(30, 30, 36));

    let title_text = " \u{1F914} thinking ";
    let title_width = UnicodeWidthStr::width(title_text);

    let mut top_spans = vec![
        Span::styled("\u{250C}".to_string(), border),
        Span::styled(title_text.to_string(), title_style),
    ];

    if has_overflow {
        let hidden = line_count - MAX_THINKING_VISIBLE_LINES;
        let indicator_text = format!(" \u{25B2} {} more lines ", hidden);
        let indicator_width = UnicodeWidthStr::width(indicator_text.as_str());
        let dash_count = inner_width.saturating_sub(title_width + indicator_width);
        top_spans.push(Span::styled("\u{2500}".repeat(dash_count), border));
        top_spans.push(Span::styled(indicator_text, indicator_style));
    } else {
        let dash_count = inner_width.saturating_sub(title_width);
        top_spans.push(Span::styled("\u{2500}".repeat(dash_count), border));
    }
    top_spans.push(Span::styled("\u{2510}".to_string(), border));

    let mut lines = Vec::new();
    lines.push(Line::from(top_spans));

    for content in visible.iter() {
        let current_width = UnicodeWidthStr::width(content.as_str());
        let padding = inner_width.saturating_sub(current_width);
        let padded = format!("{}{}", content, " ".repeat(padding));
        lines.push(Line::from(vec![
            Span::styled("\u{2502}".to_string(), border),
            Span::styled(padded, text_style),
            Span::styled("\u{2502}".to_string(), border),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("\u{2514}".to_string(), border),
        Span::styled("\u{2500}".repeat(inner_width), border),
        Span::styled("\u{2518}".to_string(), border),
    ]));

    lines
}

fn wrap_plain_line(line: &str, width: usize) -> Vec<&str> {
    if width == 0 || line.is_empty() {
        return vec![line];
    }
    let line_width = UnicodeWidthStr::width(line);
    if line_width <= width {
        return vec![line];
    }

    let mut result = Vec::new();
    let mut start = 0usize;
    let mut col = 0usize;

    for (offset, grapheme) in unicode_segmentation::UnicodeSegmentation::grapheme_indices(line, true) {
        let gw = UnicodeWidthStr::width(grapheme);
        if col + gw > width && offset > start {
            result.push(&line[start..offset]);
            start = offset;
            col = 0;
        }
        col += gw;
    }
    if start < line.len() {
        result.push(&line[start..]);
    }
    result
}

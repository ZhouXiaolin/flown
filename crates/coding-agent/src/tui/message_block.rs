//! MessageBlock component — renders a single conversation entry.
//!
//! Extracted from transcript.rs `render_entry` logic. Each message is an
//! independent `Component` with its own render cache, so streaming updates
//! only invalidate the last message.

use std::sync::Arc;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthStr;

use super::component::Component;
use super::markdown::{parse_markdown_with_width, width::line_plain_text};
use super::theme::{OCEAN_DARK_MARKDOWN, current_syntect_theme};

// ── Public types ──────────────────────────────────────────────────────

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

/// Shared rendering assets (syntax definitions, color themes).
/// Wrapped in `Arc` for cheap sharing across many `MessageBlock` instances.
pub struct RenderAssets {
    pub syntax_set: SyntaxSet,
    pub theme_set: ThemeSet,
}

impl RenderAssets {
    pub fn new() -> Self {
        Self {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
        }
    }
}

// ── MessageBlock component ────────────────────────────────────────────

/// Cached render output.
struct RenderCache {
    hash: u64,
    width: u16,
    lines: Vec<Line<'static>>,
}

/// A single message component with its own render cache.
///
/// Implements `Component`: `render(width)` returns styled lines.
/// The cache is keyed on `(body_hash, width)` — appending to `body`
/// invalidates the cache automatically on the next render call.
pub struct MessageBlock {
    kind: MessageKind,
    body: String,
    cache: Option<RenderCache>,
    assets: Arc<RenderAssets>,
}

impl MessageBlock {
    pub fn new(kind: MessageKind, body: String, assets: Arc<RenderAssets>) -> Self {
        Self {
            kind,
            body,
            cache: None,
            assets,
        }
    }

    pub fn kind(&self) -> MessageKind {
        self.kind
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn push_body(&mut self, text: &str) {
        self.body.push_str(text);
    }

    pub fn set_body(&mut self, body: String) {
        self.body = body;
    }
}

impl Component for MessageBlock {
    fn render(&mut self, width: u16) -> Vec<Line<'static>> {
        let current_hash = hash_content(&self.body);

        // Return cached lines if content and width unchanged
        if let Some(ref cache) = self.cache {
            if cache.hash == current_hash && cache.width == width && !cache.lines.is_empty() {
                return cache.lines.clone();
            }
        }

        let syntect_theme = current_syntect_theme(&self.assets.theme_set);
        let md_theme = &OCEAN_DARK_MARKDOWN;

        let lines = if self.kind == MessageKind::Thinking {
            render_thinking_block(&self.body, width as usize)
        } else {
            render_message(
                self.kind,
                &self.body,
                &self.assets.syntax_set,
                syntect_theme,
                md_theme,
                width as usize,
            )
        };

        self.cache = Some(RenderCache {
            hash: current_hash,
            width,
            lines: lines.clone(),
        });

        lines
    }

    fn invalidate(&mut self) {
        self.cache = None;
    }
}

// ── Rendering helpers (private) ───────────────────────────────────────

/// FNV-1a hash for cache invalidation.
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
        MessageKind::User => Style::default().fg(Color::Rgb(140, 190, 255)),
        MessageKind::Assistant => Style::default().fg(Color::Rgb(214, 218, 226)),
        MessageKind::Thinking => Style::default().fg(Color::DarkGray),
        MessageKind::Tool => Style::default().fg(Color::Rgb(160, 190, 200)),
        MessageKind::Error => Style::default().fg(Color::Red),
        MessageKind::System => Style::default().fg(Color::DarkGray),
    }
}

/// Render a non-thinking message entry into styled lines.
fn render_message(
    kind: MessageKind,
    body: &str,
    ss: &SyntaxSet,
    syntect_theme: &syntect::highlighting::Theme,
    md_theme: &crate::tui::theme::MarkdownTheme,
    render_width: usize,
) -> Vec<Line<'static>> {
    let style = message_style(kind);

    // User messages use ">", others use "●"
    let (prefix, first_prefix_width) = if kind == MessageKind::User {
        let prefix = Span::styled("> ", Style::default().fg(Color::Rgb(100, 100, 110)));
        (prefix, 2)
    } else {
        let prefix = Span::styled("● ", style.add_modifier(Modifier::BOLD));
        (prefix, 2)
    };
    let body_width = render_width.saturating_sub(first_prefix_width).max(8);

    let (mut rendered, _, _) = parse_markdown_with_width(body, ss, syntect_theme, body_width, md_theme, false);
    while rendered
        .last()
        .is_some_and(|line| line_plain_text(line).trim().is_empty())
    {
        rendered.pop();
    }
    if rendered.is_empty() {
        rendered.push(Line::from(""));
    }

    let mut out = Vec::with_capacity(rendered.len());
    for (line_idx, line) in rendered.into_iter().enumerate() {
        let mut spans = if line_idx == 0 {
            vec![prefix.clone()]
        } else {
            vec![Span::raw("  ")]
        };

        for span in line.spans.into_iter() {
            let patched = if span.style.fg.is_some() || span.style.bg.is_some() {
                span.style
            } else {
                span.style.patch(style)
            };
            spans.push(Span::styled(span.content, patched));
        }

        if kind == MessageKind::Tool && line_idx == 0 {
            style_tool_call_line(&mut spans);
        }

        out.push(Line::from(spans));
    }
    out
}

/// Style the first line of a tool call message.
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

// ── Thinking block ────────────────────────────────────────────────────

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

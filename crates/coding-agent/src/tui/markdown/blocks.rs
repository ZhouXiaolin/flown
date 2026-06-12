use crate::tui::theme::MarkdownTheme;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use syntect::{highlighting::Theme, parsing::SyntaxSet};
use unicode_width::UnicodeWidthStr;

use super::LastBlock;
use super::latex;
use super::lists::{ItemState, ListKind, list_item_prefix};
use super::mermaid;
use super::syntax::highlight_code;
use super::toc::TocEntry;
use super::width::display_width;
use super::wrapping::{
    push_wrapped_code_lines, push_wrapped_code_lines_open, push_wrapped_prefixed_lines,
};

pub(super) fn block_prefix(
    in_bq: bool,
    theme: &MarkdownTheme,
    marker_color: Option<Color>,
) -> Vec<Span<'static>> {
    if in_bq {
        let color = marker_color.unwrap_or(theme.blockquote_marker);
        vec![Span::styled("▏ ", Style::default().fg(color))]
    } else {
        vec![]
    }
}

pub(super) fn push_wrapped_blockquote_lines(
    lines: &mut Vec<Line<'static>>,
    body_spans: &mut Vec<Span<'static>>,
    render_width: usize,
    theme: &MarkdownTheme,
    marker_color: Option<Color>,
) {
    let prefix = block_prefix(true, theme, marker_color);
    push_wrapped_prefixed_lines(lines, body_spans, prefix.clone(), prefix, render_width);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn flush_wrapped_spans(
    lines: &mut Vec<Line<'static>>,
    spans: &mut Vec<Span<'static>>,
    blockquote_depth: usize,
    list_stack: &[ListKind],
    item_stack: &mut [ItemState],
    render_width: usize,
    theme: &MarkdownTheme,
    marker_color: Option<Color>,
) {
    if blockquote_depth > 0 && item_stack.is_empty() {
        push_wrapped_blockquote_lines(lines, spans, render_width, theme, marker_color);
    } else if !item_stack.is_empty() {
        let first_prefix = list_item_prefix(
            blockquote_depth > 0,
            list_stack,
            item_stack,
            theme,
            marker_color,
        );
        let continuation_prefix = list_item_prefix(
            blockquote_depth > 0,
            list_stack,
            item_stack,
            theme,
            marker_color,
        );
        push_wrapped_prefixed_lines(
            lines,
            spans,
            first_prefix,
            continuation_prefix,
            render_width,
        );
    } else if !spans.is_empty() {
        push_wrapped_prefixed_lines(lines, spans, vec![], vec![], render_width);
    }
}

pub(super) fn trim_paragraph_gap_before_block(
    lines: &mut Vec<Line<'static>>,
    last_block: LastBlock,
) {
    if last_block == LastBlock::Paragraph
        && lines
            .last()
            .is_some_and(|line| super::width::line_plain_text(line).is_empty())
    {
        lines.pop();
    }
}

pub(super) fn push_heading_lines(
    lines: &mut Vec<Line<'static>>,
    toc: &mut Vec<TocEntry>,
    spans: &mut Vec<Span<'static>>,
    level: u8,
    render_width: usize,
    theme: &MarkdownTheme,
) {
    let color: Color = match level {
        1 => theme.heading_1,
        2 => theme.heading_2,
        3 => theme.heading_3,
        4 => theme.heading_4,
        _ => theme.heading_other,
    };
    let modifier = match level {
        1..=5 => Modifier::BOLD,
        _ => Modifier::ITALIC,
    };
    let heading_style = Style::default().fg(color).add_modifier(modifier);
    let title: String = spans.iter().map(|s| s.content.as_ref()).collect();
    toc.push(TocEntry {
        level,
        title: title.clone(),
        line: lines.len(),
    });
    let mut styled_spans: Vec<Span<'static>> = spans
        .drain(..)
        .map(|span| {
            let mut style = heading_style;
            if span.style.bg.is_some() {
                style.fg = span.style.fg;
                style.bg = span.style.bg;
                style.sub_modifier = modifier;
            } else if span.style.fg == Some(theme.link_text)
                || span.style.fg == Some(theme.link_icon)
            {
                style.fg = span.style.fg;
            }
            Span::styled(span.content, style)
        })
        .collect();
    if styled_spans.is_empty() {
        lines.push(Line::from(""));
    } else {
        push_wrapped_prefixed_lines(lines, &mut styled_spans, vec![], vec![], render_width);
    }

    match level {
        1 => lines.push(Line::from(Span::styled(
            "═".repeat(display_width(&title).min(super::rule_width(render_width, 0))),
            Style::default().fg(theme.heading_underline),
        ))),
        2 => lines.push(Line::from(Span::styled(
            "─".repeat(display_width(&title).min(super::rule_width(render_width, 0))),
            Style::default().fg(theme.heading_underline),
        ))),
        _ => {}
    }
}

fn code_frame_color(theme: &MarkdownTheme) -> Color {
    match theme.code_frame {
        Color::Rgb(r, g, b) if color_luma(r, g, b) < 128 => lighten_rgb(r, g, b, 38),
        Color::Rgb(r, g, b) => darken_rgb(r, g, b, 38),
        color => color,
    }
}

fn color_luma(r: u8, g: u8, b: u8) -> u16 {
    (u16::from(r) * 299 + u16::from(g) * 587 + u16::from(b) * 114) / 1000
}

fn lighten_rgb(r: u8, g: u8, b: u8, amount: u8) -> Color {
    Color::Rgb(
        r.saturating_add(amount),
        g.saturating_add(amount),
        b.saturating_add(amount),
    )
}

fn darken_rgb(r: u8, g: u8, b: u8, amount: u8) -> Color {
    Color::Rgb(
        r.saturating_sub(amount),
        g.saturating_sub(amount),
        b.saturating_sub(amount),
    )
}

pub(super) struct CodeBlockRenderContext<'a> {
    pub(super) ss: &'a SyntaxSet,
    pub(super) theme: &'a Theme,
    pub(super) render_width: usize,
    pub(super) theme_colors: &'a MarkdownTheme,
    pub(super) blockquote_depth: usize,
    pub(super) list_stack: &'a [ListKind],
    pub(super) file_mode: bool,
}

pub(super) fn push_code_block_lines(
    lines: &mut Vec<Line<'static>>,
    code_buf: &mut String,
    code_lang: &mut String,
    ctx: CodeBlockRenderContext<'_>,
    item_stack: &mut [ItemState],
) {
    let prefix = if !item_stack.is_empty() {
        list_item_prefix(
            ctx.blockquote_depth > 0,
            ctx.list_stack,
            item_stack,
            ctx.theme_colors,
            None,
        )
    } else if ctx.blockquote_depth > 0 {
        block_prefix(true, ctx.theme_colors, None)
    } else {
        Vec::new()
    };
    let prefix_width: usize = prefix
        .iter()
        .map(|span| display_width(span.content.as_ref()))
        .sum();
    let label = if code_lang.is_empty() {
        "text".to_string()
    } else {
        code_lang.clone()
    };
    let available_width = ctx.render_width.saturating_sub(prefix_width);
    let (code_lines, inner_width) = highlight_code(
        code_buf,
        code_lang,
        ctx.ss,
        ctx.theme,
        available_width,
        ctx.file_mode,
    );
    let frame_style = Style::default().fg(code_frame_color(ctx.theme_colors));
    let content_width = inner_width.saturating_sub(2);

    let header_width = UnicodeWidthStr::width(label.as_str()) + 3;
    let top_bar = "─".repeat(inner_width.saturating_sub(header_width));
    let mut header = prefix.clone();
    header.extend([
        Span::styled("┌─ ".to_string(), frame_style),
        Span::styled(
            format!("{label} "),
            Style::default().fg(ctx.theme_colors.code_label),
        ),
        Span::styled(format!("{top_bar}┐"), frame_style),
    ]);
    lines.push(Line::from(header));

    for code_line in code_lines {
        let mut first_prefix = prefix.clone();
        first_prefix.push(Span::styled("│".to_string(), frame_style));

        let mut cont_prefix = prefix.clone();
        cont_prefix.push(Span::styled("│".to_string(), frame_style));

        push_wrapped_code_lines(
            lines,
            code_line.content_spans,
            first_prefix,
            cont_prefix,
            frame_style,
            content_width,
        );
    }

    let mut footer = prefix;
    footer.push(Span::styled(
        format!("└{}┘", "─".repeat(inner_width)),
        frame_style,
    ));
    lines.push(Line::from(footer));
    lines.push(Line::from(""));
    code_lang.clear();
    code_buf.clear();
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_diff_block_lines(
    lines: &mut Vec<Line<'static>>,
    diff: &str,
    render_width: usize,
    theme: &MarkdownTheme,
    blockquote_depth: usize,
    list_stack: &[ListKind],
    item_stack: &mut [ItemState],
) {
    let prefix = if !item_stack.is_empty() {
        list_item_prefix(blockquote_depth > 0, list_stack, item_stack, theme, None)
    } else if blockquote_depth > 0 {
        block_prefix(true, theme, None)
    } else {
        Vec::new()
    };
    let prefix_width: usize = prefix
        .iter()
        .map(|span| display_width(span.content.as_ref()))
        .sum();
    let available_width = render_width.saturating_sub(prefix_width);
    let frame_style = Style::default().fg(code_frame_color(theme));
    let label_style = Style::default().fg(theme.code_label);
    let gutter_style = Style::default().fg(Color::Rgb(140, 190, 255));
    let removed_style = Style::default().fg(Color::Rgb(218, 95, 95));
    let added_style = Style::default().fg(Color::Rgb(95, 200, 148));
    let context_style = Style::default().fg(theme.text);

    let diff_rows = if diff.is_empty() {
        vec![DiffDisplayRow {
            line: None,
            text: "",
            kind: DiffDisplayKind::Context,
        }]
    } else {
        diff_display_rows(diff)
    };
    let max_line = diff_rows
        .iter()
        .filter_map(|row| row.line)
        .max()
        .unwrap_or(0);
    let digit_width = max_line.max(1).to_string().len().max(3);
    let gutter_width = digit_width + 1;
    let max_text = diff_rows
        .iter()
        .map(|row| display_width(row.text) + 1)
        .max()
        .unwrap_or(0);
    let label = "diff";
    let max_inner_width = available_width
        .saturating_sub(2)
        .max(UnicodeWidthStr::width(label) + 3);
    let min_inner = (UnicodeWidthStr::width(label) + 3)
        .max(44)
        .min(max_inner_width);
    let inner_width = (max_text + 2 + gutter_width)
        .max(min_inner)
        .min(max_inner_width);
    let content_width = inner_width.saturating_sub(gutter_width + 2);

    let header_width = UnicodeWidthStr::width(label) + 3;
    let top_bar = "─".repeat(inner_width.saturating_sub(header_width));
    let mut header = prefix.clone();
    header.extend([
        Span::styled("┌─ ".to_string(), frame_style),
        Span::styled(format!("{label} "), label_style),
        Span::styled(format!("{top_bar}┐"), frame_style),
    ]);
    lines.push(Line::from(header));

    for row in diff_rows {
        let line_style = match row.kind {
            DiffDisplayKind::Added => added_style,
            DiffDisplayKind::Removed => removed_style,
            DiffDisplayKind::Context => context_style,
        };
        let mut first_prefix = prefix.clone();
        first_prefix.push(Span::styled(
            format!(
                "│{:>w$}",
                row.line.map(|line| line.to_string()).unwrap_or_default(),
                w = digit_width
            ),
            gutter_style,
        ));
        let mut cont_prefix = prefix.clone();
        cont_prefix.push(Span::styled(
            format!("│{:>w$}", "", w = digit_width),
            gutter_style,
        ));
        push_wrapped_code_lines_open(
            lines,
            vec![Span::styled(row.display_text(), line_style)],
            first_prefix,
            cont_prefix,
            content_width,
        );
    }

    let mut footer = prefix;
    footer.push(Span::styled(
        format!(
            "└{}{}┘",
            "─".repeat(gutter_width),
            "─".repeat(inner_width.saturating_sub(gutter_width))
        ),
        frame_style,
    ));
    lines.push(Line::from(footer));
    lines.push(Line::from(""));
}

#[derive(Clone, Copy)]
struct DiffDisplayRow<'a> {
    line: Option<usize>,
    text: &'a str,
    kind: DiffDisplayKind,
}

impl DiffDisplayRow<'_> {
    fn display_text(&self) -> String {
        let sign = match self.kind {
            DiffDisplayKind::Added => '+',
            DiffDisplayKind::Removed => '-',
            DiffDisplayKind::Context => ' ',
        };
        format!("{sign}{}", self.text)
    }
}

#[derive(Clone, Copy)]
enum DiffDisplayKind {
    Added,
    Removed,
    Context,
}

fn diff_display_rows(diff: &str) -> Vec<DiffDisplayRow<'_>> {
    let mut fallback_old_line = 1usize;
    let mut fallback_new_line = 1usize;
    diff.lines()
        .map(|text| {
            if let Some(rest) = text.strip_prefix('+') {
                let (old_line, new_line, text) =
                    parse_diff_line_metadata(rest).unwrap_or((None, Some(fallback_new_line), rest));
                if let Some(line) = new_line {
                    fallback_new_line = line + 1;
                }
                DiffDisplayRow {
                    line: new_line.or(old_line),
                    text,
                    kind: DiffDisplayKind::Added,
                }
            } else if let Some(rest) = text.strip_prefix('-') {
                let (old_line, new_line, text) =
                    parse_diff_line_metadata(rest).unwrap_or((Some(fallback_old_line), None, rest));
                if let Some(line) = old_line {
                    fallback_old_line = line + 1;
                }
                DiffDisplayRow {
                    line: old_line.or(new_line),
                    text,
                    kind: DiffDisplayKind::Removed,
                }
            } else if let Some(rest) = text.strip_prefix(' ') {
                let (old_line, new_line, text) = parse_diff_line_metadata(rest).unwrap_or((
                    Some(fallback_old_line),
                    Some(fallback_new_line),
                    rest,
                ));
                if let Some(line) = old_line {
                    fallback_old_line = line + 1;
                }
                if let Some(line) = new_line {
                    fallback_new_line = line + 1;
                }
                DiffDisplayRow {
                    line: old_line.or(new_line),
                    text,
                    kind: DiffDisplayKind::Context,
                }
            } else {
                DiffDisplayRow {
                    line: None,
                    text,
                    kind: DiffDisplayKind::Context,
                }
            }
        })
        .collect()
}

fn parse_diff_line_metadata(line: &str) -> Option<(Option<usize>, Option<usize>, &str)> {
    let (numbers, rest) = line.split_once(' ')?;
    let (old, new) = numbers.split_once(',')?;

    let old_line = (!old.is_empty())
        .then(|| old.parse::<usize>().ok())
        .flatten();
    let new_line = (!new.is_empty())
        .then(|| new.parse::<usize>().ok())
        .flatten();
    if old_line.is_some() || new_line.is_some() {
        Some((old_line, new_line, rest))
    } else {
        None
    }
}

pub(super) struct SpecialBlockCtx<'a, F: Fn(&str) -> Vec<Span<'static>>> {
    pub(super) label: &'a str,
    pub(super) content_lines: &'a [&'a str],
    pub(super) show_line_numbers: bool,
    pub(super) center: bool,
    pub(super) make_spans: F,
}

pub(super) fn push_special_block_lines<F: Fn(&str) -> Vec<Span<'static>>>(
    lines: &mut Vec<Line<'static>>,
    render_width: usize,
    theme: &MarkdownTheme,
    blockquote_depth: usize,
    list_stack: &[ListKind],
    item_stack: &mut [ItemState],
    ctx: SpecialBlockCtx<'_, F>,
) {
    let label = ctx.label;
    let content_lines = ctx.content_lines;
    let show_line_numbers = ctx.show_line_numbers;
    let center = ctx.center;
    let prefix = if !item_stack.is_empty() {
        list_item_prefix(blockquote_depth > 0, list_stack, item_stack, theme, None)
    } else if blockquote_depth > 0 {
        block_prefix(true, theme, None)
    } else {
        Vec::new()
    };
    let prefix_width: usize = prefix
        .iter()
        .map(|span| display_width(span.content.as_ref()))
        .sum();
    let available_width = render_width.saturating_sub(prefix_width);
    let frame_style = Style::default().fg(code_frame_color(theme));
    let label_style = Style::default().fg(theme.code_label);
    let gutter_style = Style::default().fg(theme.code_gutter);

    let total_lines = content_lines.len().max(1);
    let (digit_width, gutter_width) = if show_line_numbers {
        let dw = total_lines.to_string().len();
        (dw, dw + 2)
    } else {
        (0, 1)
    };

    let max_text = content_lines
        .iter()
        .map(|l| display_width(l))
        .max()
        .unwrap_or(0);
    let max_inner_width = available_width
        .saturating_sub(2)
        .max(UnicodeWidthStr::width(label) + 3);
    let min_inner = (UnicodeWidthStr::width(label) + 3)
        .max(44)
        .min(max_inner_width);
    let inner_width = (max_text + 2 + gutter_width)
        .max(min_inner)
        .min(max_inner_width);
    let content_width = inner_width.saturating_sub(gutter_width + 1);

    let header_width = UnicodeWidthStr::width(label) + 3;
    let top_bar = "─".repeat(inner_width.saturating_sub(header_width));
    let mut header = prefix.clone();
    header.extend([
        Span::styled("┌─ ".to_string(), frame_style),
        Span::styled(format!("{label} "), label_style),
        Span::styled(format!("{top_bar}┐"), frame_style),
    ]);
    lines.push(Line::from(header));

    let center_pad = if center {
        " ".repeat(content_width.saturating_sub(max_text) / 2)
    } else {
        String::new()
    };
    let border_only = if !show_line_numbers {
        Some(Span::styled("│".to_string(), gutter_style))
    } else {
        None
    };

    for (i, content_line) in content_lines.iter().enumerate() {
        let mut content_spans = (ctx.make_spans)(content_line);
        if !center_pad.is_empty() {
            content_spans.insert(0, Span::raw(center_pad.clone()));
        }

        let mut first_prefix = prefix.clone();
        let mut cont_prefix = prefix.clone();
        if let Some(ref border) = border_only {
            first_prefix.push(border.clone());
            cont_prefix.push(border.clone());
        } else {
            let line_num = i + 1;
            first_prefix.push(Span::styled(
                format!("│{:>w$}│", line_num, w = digit_width),
                gutter_style,
            ));
            cont_prefix.push(Span::styled(
                format!("│{:>w$}│", "", w = digit_width),
                gutter_style,
            ));
        }

        push_wrapped_code_lines(
            lines,
            content_spans,
            first_prefix,
            cont_prefix,
            gutter_style,
            content_width,
        );
    }

    let mut footer = prefix;
    if show_line_numbers {
        footer.push(Span::styled(
            format!(
                "└{}┴{}┘",
                "─".repeat(gutter_width - 2),
                "─".repeat(inner_width.saturating_sub(gutter_width - 1))
            ),
            frame_style,
        ));
    } else {
        footer.push(Span::styled(
            format!("└{}┘", "─".repeat(inner_width)),
            frame_style,
        ));
    }
    lines.push(Line::from(footer));
    lines.push(Line::from(""));
}

pub(super) fn push_latex_block_lines(
    lines: &mut Vec<Line<'static>>,
    content: &str,
    render_width: usize,
    theme: &MarkdownTheme,
    blockquote_depth: usize,
    list_stack: &[ListKind],
    item_stack: &mut [ItemState],
) {
    let rendered = latex::to_unicode(content);
    let all_lines: Vec<&str> = rendered.lines().collect();
    let start = all_lines
        .iter()
        .position(|l| !l.trim().is_empty())
        .unwrap_or(0);
    let end = all_lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(start, |e| e + 1);
    let content_style = Style::default().fg(theme.latex_block_fg);
    push_special_block_lines(
        lines,
        render_width,
        theme,
        blockquote_depth,
        list_stack,
        item_stack,
        SpecialBlockCtx {
            label: "latex",
            content_lines: &all_lines[start..end],
            show_line_numbers: false,
            center: false,
            make_spans: |line| vec![Span::styled(line.to_string(), content_style)],
        },
    );
}

pub(super) fn push_mermaid_block_lines(
    lines: &mut Vec<Line<'static>>,
    content: &str,
    render_width: usize,
    theme: &MarkdownTheme,
    blockquote_depth: usize,
    list_stack: &[ListKind],
    item_stack: &mut [ItemState],
) {
    let rendered = mermaid::render(content);
    let use_rendered = rendered.is_some();
    let content_lines: Vec<&str> = if let Some(ref r) = rendered {
        r.lines().collect()
    } else {
        content.lines().collect()
    };
    let content_style = Style::default().fg(theme.mermaid_block_fg);
    push_special_block_lines(
        lines,
        render_width,
        theme,
        blockquote_depth,
        list_stack,
        item_stack,
        SpecialBlockCtx {
            label: "mermaid",
            content_lines: &content_lines,
            show_line_numbers: false,
            center: use_rendered,
            make_spans: |line| {
                if use_rendered {
                    vec![Span::styled(line.to_string(), content_style)]
                } else {
                    mermaid::colorize_line(line, theme)
                }
            },
        },
    );
}

pub(super) fn push_rule_line(
    lines: &mut Vec<Line<'static>>,
    render_width: usize,
    theme: &MarkdownTheme,
) {
    lines.push(Line::from(Span::styled(
        "─".repeat(super::rule_width(render_width, 0)),
        Style::default().fg(theme.rule),
    )));
    lines.push(Line::from(""));
}

#![allow(dead_code)]

mod blocks;
mod fences;
mod frontmatter;
mod latex;
mod links;
mod lists;
mod markers;
pub(crate) mod mermaid;
mod spans;
mod syntax;
mod table_layout;
mod tables;
pub(crate) mod toc;
pub(crate) mod width;
mod wrapping;

use tables::{TableBuf, handle_table_event, start_table};
use crate::tui::theme::MarkdownTheme;
use pulldown_cmark::{
    BlockQuoteKind, CodeBlockKind, Event as MdEvent, HeadingLevel, Options, Parser, Tag, TagEnd,
};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use toc::{TocEntry, normalize_toc};

use blocks::{
    CodeBlockRenderContext, flush_wrapped_spans, push_code_block_lines, push_diff_block_lines,
    push_heading_lines, push_latex_block_lines, push_mermaid_block_lines, push_rule_line,
    trim_paragraph_gap_before_block,
};
use fences::normalize_code_fences;
use links::LinkSpan;
use links::build_link_spans;
use lists::{
    ItemState, ListKind, end_item, end_list, flush_list_item_spans, start_item, start_list,
};
use markers::push_custom_marker_spans;
use spans::{
    InlineStyleState, handle_inline_style_event, inline_text_style, push_inline_code_span,
    push_inline_latex_span,
};

const LINK_MARKER: &str = "#";
const DIFF_BLOCK_START: &str = "<diff>";
const DIFF_BLOCK_END: &str = "</diff>";
const DIFF_BLOCK_LANG: &str = "diff-view";

#[derive(Clone, Copy, PartialEq, Eq)]
enum LastBlock {
    Other,
    Paragraph,
}

const DEFAULT_RENDER_WIDTH: usize = 80;

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn start_heading(in_heading: &mut Option<u8>, level: HeadingLevel) {
    *in_heading = Some(heading_level(level));
}

fn end_heading(
    lines: &mut Vec<Line<'static>>,
    toc: &mut Vec<TocEntry>,
    spans: &mut Vec<Span<'static>>,
    in_heading: &mut Option<u8>,
    render_width: usize,
    theme: &MarkdownTheme,
) {
    push_heading_lines(
        lines,
        toc,
        spans,
        in_heading.unwrap_or(1),
        render_width,
        theme,
    );
    *in_heading = None;
}

fn start_code_block(
    lines: &mut Vec<Line<'static>>,
    last_block: LastBlock,
    in_code: &mut bool,
    code_buf: &mut String,
    code_lang: &mut String,
    kind: &CodeBlockKind<'_>,
) {
    trim_paragraph_gap_before_block(lines, last_block);
    *in_code = true;
    code_buf.clear();
    *code_lang = match kind {
        CodeBlockKind::Fenced(lang) => lang.to_string(),
        CodeBlockKind::Indented => String::new(),
    };
}

#[allow(clippy::too_many_arguments)]
fn end_paragraph(
    lines: &mut Vec<Line<'static>>,
    spans: &mut Vec<Span<'static>>,
    blockquote_depth: usize,
    list_stack: &[ListKind],
    item_stack: &mut [ItemState],
    render_width: usize,
    theme: &MarkdownTheme,
    marker_color: Option<Color>,
) {
    flush_wrapped_spans(
        lines,
        spans,
        blockquote_depth,
        list_stack,
        item_stack,
        render_width,
        theme,
        marker_color,
    );
}

fn end_blockquote(
    lines: &mut Vec<Line<'static>>,
    spans: &mut Vec<Span<'static>>,
    blockquote_depth: &mut usize,
    render_width: usize,
    theme: &MarkdownTheme,
    marker_color: Option<Color>,
) {
    if !spans.is_empty() {
        blocks::push_wrapped_blockquote_lines(lines, spans, render_width, theme, marker_color);
    }
    *blockquote_depth = blockquote_depth.saturating_sub(1);
    lines.push(Line::from(""));
}

fn alert_icon_label(kind: BlockQuoteKind) -> (&'static str, &'static str) {
    match kind {
        BlockQuoteKind::Note => ("[i]", "Note"),
        BlockQuoteKind::Tip => ("[*]", "Tip"),
        BlockQuoteKind::Important => ("[!]", "Important"),
        BlockQuoteKind::Warning => ("[!]", "Warning"),
        BlockQuoteKind::Caution => ("[x]", "Caution"),
    }
}

fn alert_color(kind: BlockQuoteKind, theme: &MarkdownTheme) -> Color {
    match kind {
        BlockQuoteKind::Note => theme.alert_note,
        BlockQuoteKind::Tip => theme.alert_tip,
        BlockQuoteKind::Important => theme.alert_important,
        BlockQuoteKind::Warning => theme.alert_warning,
        BlockQuoteKind::Caution => theme.alert_caution,
    }
}

fn rule_width(render_width: usize, indent: usize) -> usize {
    render_width.saturating_sub(indent).max(8)
}

const CUSTOM_MARKERS: &[markers::CustomMarker] = &[markers::MARK_MARKER];

fn push_text_event(
    spans: &mut Vec<Span<'static>>,
    code_buf: &mut String,
    text: &str,
    in_code: bool,
    theme: &MarkdownTheme,
    blockquote_depth: usize,
    inline: InlineStyleState,
) {
    if in_code {
        code_buf.push_str(text);
        return;
    }

    let fallback = inline_text_style(theme, blockquote_depth, inline);
    push_custom_marker_spans(text, CUSTOM_MARKERS, fallback, theme, spans);
}

pub(crate) fn parse_markdown(
    src: &str,
    ss: &syntect::parsing::SyntaxSet,
    theme: &syntect::highlighting::Theme,
    md_theme: &MarkdownTheme,
    file_mode: bool,
) -> (Vec<Line<'static>>, Vec<TocEntry>, Vec<LinkSpan>) {
    parse_markdown_with_width(src, ss, theme, DEFAULT_RENDER_WIDTH, md_theme, file_mode)
}

pub(crate) fn parse_markdown_with_width(
    src: &str,
    ss: &syntect::parsing::SyntaxSet,
    theme: &syntect::highlighting::Theme,
    render_width: usize,
    theme_colors: &MarkdownTheme,
    file_mode: bool,
) -> (Vec<Line<'static>>, Vec<TocEntry>, Vec<LinkSpan>) {
    let (src, fm_pairs) = frontmatter::extract_frontmatter(src);
    let mut lines: Vec<Line<'static>> = Vec::new();

    if let Some(ref pairs) = fm_pairs {
        let vertical = frontmatter::is_vertical(pairs);
        let tb = TableBuf::from_key_value_pairs(pairs, vertical);
        lines.extend(tb.render(render_width));
    }
    let mut toc: Vec<TocEntry> = Vec::new();

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut in_heading: Option<u8> = None;
    let mut in_code = false;
    let mut code_lang = String::new();
    let mut code_buf = String::new();
    let mut blockquote_depth = 0usize;
    let mut inline = InlineStyleState::default();
    let mut list_stack: Vec<ListKind> = Vec::new();
    let mut item_stack: Vec<ItemState> = Vec::new();
    let mut table: Option<TableBuf> = None;
    let mut last_block = LastBlock::Other;
    let mut link_urls: Vec<String> = Vec::new();
    let mut blockquote_color: Option<Color> = None;

    let normalized = normalize_code_fences(src);
    let normalized = normalize_diff_blocks(&normalized);
    for ev in Parser::new_ext(&normalized, Options::all()) {
        if table.is_some()
            && handle_table_event(&mut table, &ev, &mut lines, render_width, &mut link_urls)
        {
            continue;
        }
        if handle_inline_style_event(
            &ev,
            &mut inline,
            &mut spans,
            theme_colors,
            blockquote_depth,
            &mut link_urls,
        ) {
            continue;
        }

        match ev {
            MdEvent::Start(Tag::Table(aligns)) => {
                start_table(&mut table, &aligns);
            }
            MdEvent::Start(Tag::Heading { level, .. }) => {
                start_heading(&mut in_heading, level);
            }
            MdEvent::End(TagEnd::Heading(_)) => {
                end_heading(
                    &mut lines,
                    &mut toc,
                    &mut spans,
                    &mut in_heading,
                    render_width,
                    theme_colors,
                );
                last_block = LastBlock::Other;
            }
            MdEvent::Start(Tag::Paragraph) => {}
            MdEvent::End(TagEnd::Paragraph) => {
                end_paragraph(
                    &mut lines,
                    &mut spans,
                    blockquote_depth,
                    &list_stack,
                    &mut item_stack,
                    render_width,
                    theme_colors,
                    blockquote_color,
                );
                last_block = LastBlock::Paragraph;
            }
            MdEvent::Start(Tag::CodeBlock(kind)) => {
                start_code_block(
                    &mut lines,
                    last_block,
                    &mut in_code,
                    &mut code_buf,
                    &mut code_lang,
                    &kind,
                );
                last_block = LastBlock::Other;
            }
            MdEvent::End(TagEnd::CodeBlock) => {
                in_code = false;
                if code_lang == "latex" || code_lang == "tex" {
                    push_latex_block_lines(
                        &mut lines,
                        &code_buf,
                        render_width,
                        theme_colors,
                        blockquote_depth,
                        &list_stack,
                        &mut item_stack,
                    );
                    code_buf.clear();
                    code_lang.clear();
                } else if code_lang == "mermaid" {
                    push_mermaid_block_lines(
                        &mut lines,
                        &code_buf,
                        render_width,
                        theme_colors,
                        blockquote_depth,
                        &list_stack,
                        &mut item_stack,
                    );
                    code_buf.clear();
                    code_lang.clear();
                } else if code_lang == DIFF_BLOCK_LANG {
                    push_diff_block_lines(
                        &mut lines,
                        &code_buf,
                        render_width,
                        theme_colors,
                        blockquote_depth,
                        &list_stack,
                        &mut item_stack,
                    );
                    code_buf.clear();
                    code_lang.clear();
                } else {
                    push_code_block_lines(
                        &mut lines,
                        &mut code_buf,
                        &mut code_lang,
                        CodeBlockRenderContext {
                            ss,
                            theme,
                            render_width,
                            theme_colors,
                            blockquote_depth,
                            list_stack: &list_stack,
                            file_mode,
                        },
                        &mut item_stack,
                    );
                }
                last_block = LastBlock::Other;
            }
            MdEvent::Code(text) => {
                push_inline_code_span(&mut spans, text.as_ref(), theme_colors);
            }
            MdEvent::Start(Tag::BlockQuote(kind)) => {
                blockquote_depth += 1;
                if let Some(k) = kind {
                    let color = alert_color(k, theme_colors);
                    blockquote_color = Some(color);
                    let (icon, label) = alert_icon_label(k);
                    lines.push(Line::from(vec![
                        Span::styled("▏ ", Style::default().fg(color)),
                        Span::styled(
                            format!("{icon} {label}"),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        ),
                    ]));
                }
            }
            MdEvent::End(TagEnd::BlockQuote(_)) => {
                end_blockquote(
                    &mut lines,
                    &mut spans,
                    &mut blockquote_depth,
                    render_width,
                    theme_colors,
                    blockquote_color.take(),
                );
                last_block = LastBlock::Other;
            }
            MdEvent::Start(Tag::List(start)) => {
                if !item_stack.is_empty() && !spans.is_empty() {
                    flush_list_item_spans(
                        &mut lines,
                        &mut spans,
                        &list_stack,
                        &mut item_stack,
                        blockquote_depth,
                        render_width,
                        theme_colors,
                        blockquote_color,
                    );
                }
                start_list(&mut lines, last_block, &mut list_stack, start);
                last_block = LastBlock::Other;
            }
            MdEvent::End(TagEnd::List(_)) => {
                end_list(&mut lines, &mut list_stack);
                last_block = LastBlock::Other;
            }
            MdEvent::Start(Tag::Item) => {
                start_item(&mut item_stack);
            }
            MdEvent::End(TagEnd::Item) => {
                end_item(
                    &mut lines,
                    &mut spans,
                    &mut list_stack,
                    &mut item_stack,
                    blockquote_depth,
                    render_width,
                    theme_colors,
                    blockquote_color,
                );
                last_block = LastBlock::Other;
            }
            MdEvent::Rule => {
                push_rule_line(&mut lines, render_width, theme_colors);
                last_block = LastBlock::Other;
            }
            MdEvent::Text(text) => {
                push_text_event(
                    &mut spans,
                    &mut code_buf,
                    text.as_ref(),
                    in_code,
                    theme_colors,
                    blockquote_depth,
                    inline,
                );
            }
            MdEvent::SoftBreak | MdEvent::HardBreak if !in_code => {
                flush_wrapped_spans(
                    &mut lines,
                    &mut spans,
                    blockquote_depth,
                    &list_stack,
                    &mut item_stack,
                    render_width,
                    theme_colors,
                    blockquote_color,
                );
            }
            MdEvent::SoftBreak | MdEvent::HardBreak => {}
            MdEvent::InlineMath(text) => {
                push_inline_latex_span(&mut spans, text.as_ref(), theme_colors);
            }
            MdEvent::DisplayMath(text) => {
                if !spans.is_empty() {
                    flush_wrapped_spans(
                        &mut lines,
                        &mut spans,
                        blockquote_depth,
                        &list_stack,
                        &mut item_stack,
                        render_width,
                        theme_colors,
                        blockquote_color,
                    );
                }
                trim_paragraph_gap_before_block(&mut lines, last_block);
                push_latex_block_lines(
                    &mut lines,
                    text.as_ref(),
                    render_width,
                    theme_colors,
                    blockquote_depth,
                    &list_stack,
                    &mut item_stack,
                );
                last_block = LastBlock::Other;
            }
            MdEvent::TaskListMarker(checked) => {
                if let Some(item) = item_stack.last_mut() {
                    item.checkbox = Some(checked);
                }
            }
            _ => {}
        }
    }

    if !spans.is_empty() {
        flush_wrapped_spans(
            &mut lines,
            &mut spans,
            blockquote_depth,
            &list_stack,
            &mut item_stack,
            render_width,
            theme_colors,
            blockquote_color,
        );
    }
    let link_spans = build_link_spans(&lines, &link_urls, theme_colors);
    (lines, normalize_toc(toc), link_spans)
}

fn normalize_diff_blocks(src: &str) -> String {
    src.replace(DIFF_BLOCK_START, &format!("```{DIFF_BLOCK_LANG}\n"))
        .replace(DIFF_BLOCK_END, "```")
}

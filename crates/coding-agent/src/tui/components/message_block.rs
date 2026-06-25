//! MessageBlock — renders a transcript entry into iodilos text rows.

use iodilos::prelude::{Color, Modifier};
use iodilos::text::SpanStyle;
use iodilos_md::{MarkdownTheme, StreamingSurface, markdown_surface};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::state::{ConversationEntry, EntryKind};

/// One rendered terminal row: a list of `(text, style)` runs. This is the shape
/// `iodilos::producer::Lines` consumes (each entry is one terminal row, the
/// runs painted left-to-right).
pub type Row = Vec<(String, SpanStyle)>;

const THINKING_BLOCK_MAX_ROWS: usize = 5;
const THINKING_BLOCK_MIN_WIDTH: usize = 8;

/// Color constants for tool indicators
const TOOL_READ_COLOR: Color = Color::Rgb { r: 100, g: 180, b: 255 };    // Blue
const TOOL_WRITE_COLOR: Color = Color::Rgb { r: 100, g: 220, b: 120 };   // Green
const TOOL_EDIT_COLOR: Color = Color::Rgb { r: 255, g: 200, b: 80 };     // Yellow/Orange
const TOOL_BASH_COLOR: Color = Color::Rgb { r: 180, g: 140, b: 255 };    // Purple

/// Colors for diff view
const DIFF_ADD_COLOR: Color = Color::Rgb { r: 80, g: 200, b: 120 };      // Green
const DIFF_REMOVE_COLOR: Color = Color::Rgb { r: 255, g: 100, b: 100 };  // Red
const DIFF_HUNK_COLOR: Color = Color::Rgb { r: 100, g: 160, b: 255 };    // Blue
const DIFF_META_COLOR: Color = Color::Rgb { r: 150, g: 150, b: 150 };    // Grey
const LINE_NUM_COLOR: Color = Color::Rgb { r: 100, g: 100, b: 100 };     // Dark grey

pub fn render_entry(
    entry: &ConversationEntry,
    render_width: usize,
    parser: Option<&mut StreamingSurface>,
) -> Vec<Row> {
    let width = render_width.max(1);
    match &entry.kind {
        EntryKind::User(text) => render_plain(
            "> ",
            Color::Rgb {
                r: 140,
                g: 190,
                b: 255,
            },
            text,
        ),
        EntryKind::Assistant(body) => {
            // Snapshot the streaming body via the per-item Signal. The
            // streaming-list path (forthcoming) reads this Signal inside its
            // own reactive region, but the current cached-renderer path is
            // synchronous and only needs the latest text.
            let text = body.get_clone();
            render_markdown(
                "● ",
                Color::Rgb {
                    r: 118,
                    g: 205,
                    b: 255,
                },
                &text,
                width,
                parser,
            )
        }
        // Note: render_thinking_block below does not take the streaming parser —
        // thinking blocks are capped/bounded views, so the incremental cache is
        // not worth threading through.
        EntryKind::Thinking(body) => {
            let text = body.get_clone();
            render_thinking_block(&text, width)
        }
        EntryKind::Tool { name, text } => {
            render_tool(name, text, width, parser)
        }
        EntryKind::ToolResult { tool, output } => match tool.as_str() {
            "bash" => render_bash_result(output, width),
            // Unknown tool: fall back to a plain warning-style line so the
            // result is still visible without a dedicated renderer.
            _ => render_plain("result ", Color::DarkGrey, output),
        },
        EntryKind::Error(text) => render_plain("error ", Color::Red, text),
        EntryKind::Warning(text) => render_plain("warn ", Color::Yellow, text),
        EntryKind::System(text) => render_markdown("info ", Color::DarkGrey, text, width, None),
    }
}

/// Get the colored dot prefix for a tool name
fn tool_dot(name: &str) -> (&'static str, Color) {
    match name {
        "read" => ("● ", TOOL_READ_COLOR),
        "write" => ("● ", TOOL_WRITE_COLOR),
        "edit" => ("● ", TOOL_EDIT_COLOR),
        "bash" => ("● ", TOOL_BASH_COLOR),
        _ => ("● ", Color::Rgb { r: 160, g: 190, b: 200 }),
    }
}

/// Render a tool entry with appropriate formatting based on tool type
fn render_tool(
    name: &str,
    text: &str,
    render_width: usize,
    parser: Option<&mut StreamingSurface>,
) -> Vec<Row> {
    match name {
        "write" => render_write_tool(text, render_width, parser),
        "edit" => render_edit_tool(text, render_width),
        // bash command line (`Bash(xxx)`) is capped to a single line: anything
        // beyond the available width is elided with "..." so a long command
        // never wraps and push the transcript down.
        "bash" => render_bash_command(text, render_width),
        _ => render_generic_tool(name, text),
    }
}

/// Maximum body lines shown for a bash result before truncating with "...".
const BASH_RESULT_MAX_LINES: usize = 5;

/// Render a bash command-line entry (`Bash(<command>)`) as a single row,
/// truncating with "..." when it overflows the available width. The command's
/// own output arrives separately as a `ToolResult` entry and is rendered by
/// `render_bash_result`.
fn render_bash_command(text: &str, render_width: usize) -> Vec<Row> {
    let (dot, color) = tool_dot("bash");
    let style = fg(color);
    let first_line = text.lines().next().unwrap_or(text);
    // The dot prefix ("● ") occupies 2 cells; the command fills the rest.
    let dot_width = UnicodeWidthStr::width(dot);
    let budget = render_width.saturating_sub(dot_width).max(1);
    let truncated = truncate_with_ellipsis(first_line, budget);
    vec![vec![
        (dot.to_string(), style),
        (truncated, style),
    ]]
}

/// Render a bash result body: each line indented two spaces, italic, light
/// grey, capped at [`BASH_RESULT_MAX_LINES`] lines with a trailing "..." row
/// when truncated.
fn render_bash_result(output: &str, render_width: usize) -> Vec<Row> {
    let style = SpanStyle {
        fg: Some(Color::Rgb { r: 170, g: 170, b: 170 }),
        add_modifier: Modifier::ITALIC,
        ..SpanStyle::default()
    };
    let indent = "  ";
    let indent_width = UnicodeWidthStr::width(indent);
    let budget = render_width.saturating_sub(indent_width).max(1);
    let mut rows = Vec::new();
    let mut lines = output.lines();
    for _ in 0..BASH_RESULT_MAX_LINES {
        match lines.next() {
            Some(line) => {
                let truncated = truncate_with_ellipsis(line, budget);
                rows.push(vec![
                    (indent.to_string(), style),
                    (truncated, style),
                ]);
            }
            None => return rows,
        }
    }
    if lines.next().is_some() {
        rows.push(vec![
            (indent.to_string(), style),
            ("...".to_string(), style),
        ]);
    }
    rows
}

/// Truncate `text` to fit within `budget` display cells, appending "..." when
/// it had to be cut. Wide characters are measured with `UnicodeWidthStr`, so
/// the result never exceeds `budget`.
fn truncate_with_ellipsis(text: &str, budget: usize) -> String {
    let ellipsis = "...";
    let ellipsis_w = UnicodeWidthStr::width(ellipsis);
    let full_w = UnicodeWidthStr::width(text);
    if full_w <= budget {
        return text.to_string();
    }
    // Need at least room for the ellipsis; otherwise show just the ellipsis
    // (clipped) so a very narrow terminal never overflows.
    if budget <= ellipsis_w {
        return ellipsis.chars().take(budget.max(1)).collect();
    }
    let target = budget - ellipsis_w;
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0).max(1);
        if width + cw > target {
            break;
        }
        out.push(ch);
        width += cw;
    }
    out.push_str(ellipsis);
    out
}

/// Render a generic tool call (read, bash, etc.)

/// Render a write tool call: colored dot + path, then content as markdown
fn render_write_tool(
    text: &str,
    render_width: usize,
    parser: Option<&mut StreamingSurface>,
) -> Vec<Row> {
    let (dot, color) = tool_dot("write");
    let style = fg(color);
    let mut rows = Vec::new();

    // First line: "● Write path"
    let first_line = text.lines().next().unwrap_or(text);
    rows.push(vec![
        (dot.to_string(), style),
        (first_line.to_string(), style),
    ]);

    // Rest: render as markdown (the code content)
    let body = text.lines().skip(1).collect::<Vec<_>>().join("\n");
    if !body.is_empty() {
        let theme = MarkdownTheme::default();
        let surface = match parser {
            Some(p) => p.render(&body, render_width, &theme),
            None => markdown_surface(&body, render_width, &theme),
        };
        for row in &surface.rows {
            rows.push(row.clone());
        }
    }

    rows
}

/// Render an edit tool call: colored dot + path, then diff view with line numbers
fn render_edit_tool(text: &str, render_width: usize) -> Vec<Row> {
    let (dot, color) = tool_dot("edit");
    let style = fg(color);
    let mut rows = Vec::new();

    // First line: "● Edit path(+N -N)"
    let first_line = text.lines().next().unwrap_or(text);
    rows.push(vec![
        (dot.to_string(), style),
        (first_line.to_string(), style),
    ]);

    // Parse and render diff content
    let body = text.lines().skip(1).collect::<Vec<_>>().join("\n");
    if !body.is_empty() {
        render_diff_view(&body, render_width, &mut rows);
    }

    rows
}

/// Render diff content with line numbers and colored additions/deletions
fn render_diff_view(diff_text: &str, render_width: usize, rows: &mut Vec<Row>) {
    let line_num_width = 4; // Width for line numbers
    let separator = " │ ";
    let separator_width = separator.len();

    // Calculate content width (subtract line number and separator)
    let content_width = render_width.saturating_sub(line_num_width * 2 + separator_width + 2).max(20);

    // Parse the diff text - look for markers from the formatted output
    // The diff is formatted as: <diff>+line\n-line\n line\n</diff>
    // But in the actual text we have lines starting with +, -, or space
    let mut line_num_old = 0u32;
    let mut line_num_new = 0u32;

    for line in diff_text.lines() {
        if line.starts_with('+') {
            // Addition
            line_num_new += 1;
            let content = &line[1..];
            let (wrapped, _) = wrap_line(content, content_width);
            for (i, wrap_line) in wrapped.iter().enumerate() {
                let old_str = if i == 0 { "   ".to_string() } else { "   ".to_string() };
                let new_str = format!("{:>width$}", line_num_new, width = line_num_width);
                rows.push(vec![
                    (old_str, fg(LINE_NUM_COLOR)),
                    (new_str, fg(DIFF_ADD_COLOR)),
                    (separator.to_string(), fg(DIFF_META_COLOR)),
                    ("+".to_string(), fg(DIFF_ADD_COLOR)),
                    (wrap_line.clone(), fg(DIFF_ADD_COLOR)),
                ]);
            }
        } else if line.starts_with('-') {
            // Deletion
            line_num_old += 1;
            let content = &line[1..];
            let (wrapped, _) = wrap_line(content, content_width);
            for (i, wrap_line) in wrapped.iter().enumerate() {
                let old_str = format!("{:>width$}", line_num_old, width = line_num_width);
                let new_str = if i == 0 { "   ".to_string() } else { "   ".to_string() };
                rows.push(vec![
                    (old_str, fg(DIFF_REMOVE_COLOR)),
                    (new_str, fg(LINE_NUM_COLOR)),
                    (separator.to_string(), fg(DIFF_META_COLOR)),
                    ("-".to_string(), fg(DIFF_REMOVE_COLOR)),
                    (wrap_line.clone(), fg(DIFF_REMOVE_COLOR)),
                ]);
            }
        } else if line.starts_with(' ') {
            // Context line
            line_num_old += 1;
            line_num_new += 1;
            let content = &line[1..];
            let (wrapped, _) = wrap_line(content, content_width);
            for wrap_line in wrapped {
                let old_str = format!("{:>width$}", line_num_old, width = line_num_width);
                let new_str = format!("{:>width$}", line_num_new, width = line_num_width);
                rows.push(vec![
                    (old_str, fg(LINE_NUM_COLOR)),
                    (new_str, fg(LINE_NUM_COLOR)),
                    (separator.to_string(), fg(DIFF_META_COLOR)),
                    (" ".to_string(), fg(Color::DarkGrey)),
                    (wrap_line, fg(Color::DarkGrey)),
                ]);
            }
        }
        // Skip other lines (like diff headers)
    }
}

/// Wrap a line to fit within width, returning wrapped lines
fn wrap_line(text: &str, width: usize) -> (Vec<String>, usize) {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut col = 0;

    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0).max(1);
        if col + ch_width > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            col = 0;
        }
        current.push(ch);
        col += ch_width;
    }
    lines.push(current);

    if lines.is_empty() {
        lines.push(String::new());
    }

    let line_count = lines.len();
    (lines, line_count)
}

/// Render a generic tool call (read, bash, etc.)
fn render_generic_tool(name: &str, text: &str) -> Vec<Row> {
    let (dot, color) = tool_dot(name);
    let style = fg(color);
    let mut rows = Vec::new();

    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            rows.push(vec![
                (dot.to_string(), style),
                (line.to_string(), style),
            ]);
        } else {
            rows.push(vec![(line.to_string(), style)]);
        }
    }
    if rows.is_empty() {
        rows.push(vec![(dot.to_string(), style)]);
    }
    rows
}

fn render_plain(prefix: &'static str, color: Color, body: &str) -> Vec<Row> {
    let style = fg(color);
    let mut rows = Vec::new();
    for (i, line) in body.lines().enumerate() {
        if i == 0 {
            rows.push(vec![
                (prefix.to_string(), style),
                (line.to_string(), style),
            ]);
        } else {
            rows.push(vec![(line.to_string(), style)]);
        }
    }
    if rows.is_empty() {
        rows.push(vec![(prefix.to_string(), style)]);
    }
    rows
}

fn render_thinking_block(body: &str, render_width: usize) -> Vec<Row> {
    if render_width < THINKING_BLOCK_MIN_WIDTH {
        return render_plain("thinking ", Color::DarkGrey, body);
    }

    let block_width = render_width;
    let inner_width = block_width.saturating_sub(4).max(1);
    let content_rows = THINKING_BLOCK_MAX_ROWS.saturating_sub(2).max(1);
    let wrapped = wrap_plain_text(body, inner_width);
    let start = wrapped.len().saturating_sub(content_rows);
    let visible = &wrapped[start..];
    let border = fg(Color::DarkGrey);
    let label = fg(Color::Yellow);
    let text = fg(Color::Grey);

    let mut rows = Vec::with_capacity(visible.len() + 2);
    rows.push(thinking_top_row(block_width, border, label));
    for line in visible {
        rows.push(vec![
            ("│ ".to_string(), border),
            (pad_to_width(line, inner_width), text),
            (" │".to_string(), border),
        ]);
    }
    rows.push(vec![(
        format!("╰{}╯", "─".repeat(block_width.saturating_sub(2))),
        border,
    )]);
    rows
}

fn thinking_top_row(width: usize, border: SpanStyle, label: SpanStyle) -> Row {
    let title = " thinking ";
    let prefix = "╭─";
    let suffix = "╮";
    let title_width = display_width(title);
    let prefix_width = display_width(prefix);
    let suffix_width = display_width(suffix);

    if width >= prefix_width + title_width + suffix_width {
        let fill = width - prefix_width - title_width - suffix_width;
        return vec![
            (prefix.to_string(), border),
            (title.to_string(), label),
            ("─".repeat(fill), border),
            (suffix.to_string(), border),
        ];
    }

    vec![(
        format!("╭{}╮", "─".repeat(width.saturating_sub(2))),
        border,
    )]
}

fn wrap_plain_text(body: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();

    for source_line in body.split('\n') {
        if source_line.is_empty() {
            rows.push(String::new());
            continue;
        }

        let mut current = String::new();
        let mut col = 0usize;
        for ch in source_line.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0).max(1);
            if col + ch_width > width && !current.is_empty() {
                rows.push(std::mem::take(&mut current));
                col = 0;
            }
            current.push(ch);
            col += ch_width;
        }
        rows.push(current);
    }

    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn pad_to_width(text: &str, width: usize) -> String {
    let used = display_width(text);
    if used >= width {
        return text.to_string();
    }
    format!("{text}{}", " ".repeat(width - used))
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn render_markdown(
    prefix: &'static str,
    color: Color,
    body: &str,
    render_width: usize,
    parser: Option<&mut StreamingSurface>,
) -> Vec<Row> {
    let theme = MarkdownTheme::default();
    let surface = match parser {
        Some(parser) => parser.render(body, render_width, &theme),
        None => markdown_surface(body, render_width, &theme),
    };
    let mut rows = surface.rows.clone();
    if rows.is_empty() {
        return vec![vec![(prefix.to_string(), fg(color))]];
    }
    rows[0].insert(0, (prefix.to_string(), fg(color)));
    rows
}

fn fg(color: Color) -> SpanStyle {
    SpanStyle {
        fg: Some(color),
        ..SpanStyle::default()
    }
}

#[allow(dead_code)]
fn bold(color: Color) -> SpanStyle {
    SpanStyle {
        fg: Some(color),
        add_modifier: Modifier::BOLD,
        ..SpanStyle::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iodilos::reactive::{Signal, create_root, create_signal};

    fn entry(kind: EntryKind) -> ConversationEntry {
        ConversationEntry { id: 0, kind }
    }

    /// Wrap a String body in a `Signal<String>` for the streaming `Assistant`
    /// / `Thinking` kinds. Must be called from inside a `create_root` scope.
    fn signal_of(text: &str) -> Signal<String> {
        create_signal(text.to_string())
    }

    fn row_text(row: &Row) -> String {
        row.iter().map(|(content, _)| content.as_str()).collect()
    }

    #[test]
    fn thinking_entry_renders_as_bounded_block() {
        let owner = create_root(|| {
            let rows = render_entry(
                &entry(EntryKind::Thinking(signal_of(
                    "line 1\nline 2\nline 3\nline 4\nline 5\nline 6",
                ))),
                24,
                None,
            );

            assert_eq!(rows.len(), 5);
            assert!(row_text(&rows[0]).starts_with("╭─ thinking "));
            assert!(row_text(&rows[1]).contains("line 4"));
            assert!(row_text(&rows[2]).contains("line 5"));
            assert!(row_text(&rows[3]).contains("line 6"));
            assert!(row_text(&rows[4]).starts_with("╰"));
        });
        owner.dispose();
    }

    #[test]
    fn thinking_entry_wraps_plain_text_without_markdown() {
        let owner = create_root(|| {
            let rows = render_entry(
                &entry(EntryKind::Thinking(signal_of("# heading is plain"))),
                14,
                None,
            );

            let body = rows.iter().map(row_text).collect::<Vec<_>>().join("\n");
            assert!(body.contains("# heading"));
            assert!(rows.len() <= THINKING_BLOCK_MAX_ROWS);
        });
        owner.dispose();
    }

    #[test]
    fn assistant_entry_uses_dot_prefix() {
        let owner = create_root(|| {
            let rows = render_entry(
                &entry(EntryKind::Assistant(signal_of("hello"))),
                40,
                None,
            );
            assert!(row_text(&rows[0]).starts_with("● "));
        });
        owner.dispose();
    }

    #[test]
    fn tool_entry_uses_colored_dot() {
        let rows = render_entry(
            &entry(EntryKind::Tool {
                name: "read".to_string(),
                text: "file.txt".to_string(),
            }),
            40,
            None,
        );
        assert!(row_text(&rows[0]).starts_with("● "));
        assert!(row_text(&rows[0]).contains("file.txt"));
    }

    #[test]
    fn edit_tool_renders_diff_with_line_numbers() {
        let edit_text = "Edit src/main.rs(+2 -1)\n<diff>\n fn main() {\n-    println!(\"old\");\n+    println!(\"new\");\n+    println!(\"added\");\n }\n</diff>";
        let rows = render_entry(
            &entry(EntryKind::Tool {
                name: "edit".to_string(),
                text: edit_text.to_string(),
            }),
            80,
            None,
        );

        // First row should have the edit header
        let first = row_text(&rows[0]);
        assert!(first.starts_with("● "));
        assert!(first.contains("Edit src/main.rs"));

        // Should have diff rows with line numbers
        let all_text: String = rows.iter().map(|r| row_text(r)).collect::<Vec<_>>().join("\n");
        assert!(all_text.contains("println!"));
    }

    #[test]
    fn write_tool_renders_content_as_markdown() {
        let write_text = "Write test.rs\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```";
        let rows = render_entry(
            &entry(EntryKind::Tool {
                name: "write".to_string(),
                text: write_text.to_string(),
            }),
            80,
            None,
        );

        // First row should have the write header
        let first = row_text(&rows[0]);
        assert!(first.starts_with("● "));
        assert!(first.contains("Write test.rs"));
    }

    #[test]
    fn bash_tool_renders_with_purple_dot() {
        let rows = render_entry(
            &entry(EntryKind::Tool {
                name: "bash".to_string(),
                text: "ls -la".to_string(),
            }),
            40,
            None,
        );
        assert!(row_text(&rows[0]).starts_with("● "));
        assert!(row_text(&rows[0]).contains("ls -la"));
    }

    #[test]
    fn bash_command_short_fits_unchanged() {
        let rows = render_entry(
            &entry(EntryKind::Tool {
                name: "bash".to_string(),
                text: "Bash(ls -la)".to_string(),
            }),
            40,
            None,
        );
        assert_eq!(rows.len(), 1, "short command stays a single line");
        let text = row_text(&rows[0]);
        assert!(text.starts_with("● "));
        assert!(text.contains("Bash(ls -la)"));
        assert!(!text.contains("..."), "no truncation when it fits");
    }

    #[test]
    fn bash_command_long_is_truncated_with_ellipsis_to_one_line() {
        let long = format!("Bash({})", "x".repeat(200));
        let width = 30usize;
        let rows = render_entry(
            &entry(EntryKind::Tool {
                name: "bash".to_string(),
                text: long,
            }),
            width,
            None,
        );
        assert_eq!(rows.len(), 1, "long command must collapse to one line");
        let text = row_text(&rows[0]);
        assert!(text.ends_with("..."), "truncated tail should be '...': {text:?}");
        // Whole row must not exceed the available width.
        assert!(
            display_width(&text) <= width,
            "row must fit in width {width}, got {}: {text:?}",
            display_width(&text)
        );
    }

    #[test]
    fn bash_result_indents_italic_light_and_caps_at_five_lines() {
        let rows = render_entry(
            &entry(EntryKind::ToolResult {
                tool: "bash".to_string(),
                output: "one\ntwo\nthree\nfour\nfive\nsix\nseven".to_string(),
            }),
            40,
            None,
        );
        // 5 body lines + 1 "..." continuation row.
        assert_eq!(rows.len(), 6, "output over 5 lines truncates with a '...' row");
        // Each row is indented two spaces.
        for (i, row) in rows.iter().enumerate() {
            let text = row_text(row);
            assert!(
                text.starts_with("  "),
                "row {i} must be indented two spaces: {text:?}"
            );
        }
        assert!(
            row_text(&rows[5]).ends_with("..."),
            "truncation marker row should end with '...'"
        );
        // The body rows carry the italic modifier on their styled segment.
        for row in &rows {
            assert!(
                row.iter().any(|(_, style)| {
                    style.add_modifier.contains(Modifier::ITALIC)
                }),
                "result row must have an italic segment: {:?}",
                row_text(row)
            );
        }
    }

    #[test]
    fn bash_result_short_output_not_truncated() {
        let rows = render_entry(
            &entry(EntryKind::ToolResult {
                tool: "bash".to_string(),
                output: "hello world".to_string(),
            }),
            40,
            None,
        );
        assert_eq!(rows.len(), 1);
        let text = row_text(&rows[0]);
        assert!(text.starts_with("  "));
        assert!(text.contains("hello world"));
        assert!(!text.contains("..."));
    }

    #[test]
    fn bash_result_each_line_truncates_with_ellipsis_when_too_wide() {
        // A single very long line should be clipped to the available width
        // (2-cell indent + body) with a trailing "...".
        let rows = render_entry(
            &entry(EntryKind::ToolResult {
                tool: "bash".to_string(),
                output: "y".repeat(200),
            }),
            20,
            None,
        );
        assert_eq!(rows.len(), 1);
        let text = row_text(&rows[0]);
        assert!(text.ends_with("..."));
        assert!(
            display_width(&text) <= 20,
            "line must fit in 20 cells, got {}: {text:?}",
            display_width(&text)
        );
    }
}

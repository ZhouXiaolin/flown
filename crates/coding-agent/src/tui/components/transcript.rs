//! Transcript — the scrollable conversation view.
//!
//! Renders the conversation into a bounded RichText viewport. The full
//! conversation stays in [`UiState::entries`], but this component only hands the
//! renderer the visible window so streaming output cannot grow the root layout
//! and push the editor/status bars off-screen.
//!
//! `scroll_offset == usize::MAX` means auto-follow the bottom. Otherwise it is
//! the number of rendered lines hidden below the viewport.

use std::rc::Rc;

use iodilos::prelude::*;

use crate::tui::components::message_block::render_entry;
use crate::tui::state::{ConversationEntry, EntryKind, UiState};

const RESERVED_ROWS: u16 = 7;
/// Columns consumed by the `ScrollableViewport` chrome: left/right border (1+1)
/// plus inner left/right padding (1+1). Must match the body width the markdown
/// renderer wraps to, otherwise `Paragraph::wrap` re-wraps at draw time and the
/// `RichText` node overflows its declared height, causing clipped output.
const TRANSCRIPT_CHROME_COLS: u16 = 4;

#[component]
pub fn Transcript() -> impl IntoView {
    let stack = use_context::<Rc<crate::tui::conversation::ConversationStack>>();
    let active_index = stack.active_index_signal();
    let terminal_size = use_terminal_size();

    let content = Node::new_richtext();
    let content_for_effect = content.clone();
    let stack_for_effect = Rc::clone(&stack);
    create_effect(move || {
        active_index.get();
        let state = Rc::clone(&stack_for_effect.active().state);
        let (terminal_width, terminal_height) = terminal_size.get();
        let viewport_lines = transcript_viewport_lines(terminal_height);
        let render_width = transcript_render_width(terminal_width);
        let scroll_offset = state.scroll_offset.get();
        state.entries.with(|entries| {
            content_for_effect.set_lines(visible_transcript_lines(
                entries,
                viewport_lines,
                scroll_offset,
                render_width,
            ));
        });
    });

    // Scroll acts on whatever layer is active at scroll time (not mount time).
    let scroll_stack = Rc::clone(&stack);
    let on_scroll = Callback::new(move |direction| {
        let state = Rc::clone(&scroll_stack.active().state);
        match direction {
            ScrollDirection::Up => state.scroll_up(3),
            ScrollDirection::Down => state.scroll_down(3),
            ScrollDirection::Left | ScrollDirection::Right => {}
        }
    });
    let mut props = ScrollableViewportProps::new(content, on_scroll);
    props.border_color = Color::Rgb(40, 40, 48);
    ScrollableViewport::new(props)
}

pub fn transcript_for_state(state: Rc<UiState>) -> Node {
    let terminal_size = use_terminal_size();

    let content = Node::new_richtext();
    let content_for_effect = content.clone();
    let state_for_effect = Rc::clone(&state);
    create_effect(move || {
        let (terminal_width, terminal_height) = terminal_size.get();
        let viewport_lines = transcript_viewport_lines(terminal_height);
        let render_width = transcript_render_width(terminal_width);
        let scroll_offset = state_for_effect.scroll_offset.get();
        state_for_effect.entries.with(|entries| {
            content_for_effect.set_lines(visible_transcript_lines(
                entries,
                viewport_lines,
                scroll_offset,
                render_width,
            ));
        });
    });

    let scroll_state = Rc::clone(&state);
    let on_scroll = Callback::new(move |direction| match direction {
        ScrollDirection::Up => scroll_state.scroll_up(3),
        ScrollDirection::Down => scroll_state.scroll_down(3),
        ScrollDirection::Left | ScrollDirection::Right => {}
    });
    let mut props = ScrollableViewportProps::new(content, on_scroll);
    props.border_color = Color::Rgb(40, 40, 48);
    ScrollableViewport::new(props)
}

fn transcript_viewport_lines(terminal_height: u16) -> usize {
    terminal_height.saturating_sub(RESERVED_ROWS).max(1) as usize
}

fn transcript_render_width(terminal_width: u16) -> usize {
    terminal_width.saturating_sub(TRANSCRIPT_CHROME_COLS).max(1) as usize
}

pub(crate) fn visible_transcript_lines(
    entries: &[ConversationEntry],
    viewport_lines: usize,
    scroll_offset: usize,
    render_width: usize,
) -> Vec<Line<'static>> {
    let viewport_lines = viewport_lines.max(1);
    let requested_offset = if scroll_offset == usize::MAX {
        0
    } else {
        scroll_offset
    };
    let needed_lines = viewport_lines.saturating_add(requested_offset);
    // Collect (entry, rendered chunk) pairs so the separator decision below can
    // see each entry's kind.
    let mut chunks: Vec<(&ConversationEntry, Vec<Line<'static>>)> = Vec::new();
    let mut rendered_lines = 0usize;

    for entry in entries.iter().rev() {
        let chunk = render_entry(entry, render_width);
        rendered_lines = rendered_lines.saturating_add(chunk.len());
        chunks.push((entry, chunk));
        if rendered_lines >= needed_lines {
            break;
        }
    }

    let mut lines = Vec::with_capacity(rendered_lines);
    let mut prev_entry: Option<&ConversationEntry> = None;
    for (entry, chunk) in chunks.into_iter().rev() {
        // One blank line on a User↔Assistant turn boundary, in either
        // direction: between a prompt and its reply (User→Assistant), and
        // between a reply and the next prompt (Assistant→User). Assistant-
        // internal rows (thinking/tool/text/error) stay tight — they belong to
        // one assistant turn. System rows never get a separator.
        if let Some(prev) = prev_entry {
            let boundary = matches!(
                (&prev.kind, &entry.kind),
                (EntryKind::User(_), EntryKind::Assistant(_))
                    | (EntryKind::Assistant(_), EntryKind::User(_))
            );
            if boundary {
                lines.push(Line::from(""));
            }
        }
        lines.extend(chunk);
        prev_entry = Some(entry);
    }

    if lines.is_empty() {
        return vec![Line::from("")];
    }

    let max_offset = lines.len().saturating_sub(viewport_lines);
    let offset = requested_offset.min(max_offset);
    let end = lines.len().saturating_sub(offset);
    let start = end.saturating_sub(viewport_lines);
    tracing::debug!(
        target: "flown::transcript",
        entries = entries.len(),
        rendered_total = lines.len(),
        viewport_lines,
        scroll_offset,
        start,
        end,
        "transcript window"
    );
    lines[start..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::EntryKind;

    fn entry(text: &str) -> ConversationEntry {
        ConversationEntry {
            kind: EntryKind::System(text.to_string()),
        }
    }

    fn entry_of(make_kind: impl FnOnce(String) -> EntryKind, text: &str) -> ConversationEntry {
        ConversationEntry {
            kind: make_kind(text.to_string()),
        }
    }

    fn plain(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn visible_lines_follow_the_bottom_by_default() {
        let entries = vec![entry("one"), entry("two"), entry("three"), entry("four")];

        let lines = visible_transcript_lines(&entries, 2, usize::MAX, 80);
        assert_eq!(plain(&lines[0]), "ℹ three");
        assert_eq!(plain(&lines[1]), "ℹ four");
    }

    #[test]
    fn visible_lines_can_scroll_above_the_bottom() {
        let entries = vec![entry("one"), entry("two"), entry("three"), entry("four")];

        let lines = visible_transcript_lines(&entries, 2, 1, 80);
        assert_eq!(plain(&lines[0]), "ℹ two");
        assert_eq!(plain(&lines[1]), "ℹ three");
    }

    /// A user prompt and the assistant reply that follows it are separated by a
    /// blank line — the visual turn boundary the user asked for.
    #[test]
    fn blank_line_separates_user_prompt_from_assistant_reply() {
        let entries = vec![
            entry_of(EntryKind::User, "list the files"),
            entry_of(EntryKind::Assistant, "here they are"),
        ];

        let lines = visible_transcript_lines(&entries, 4, usize::MAX, 80);
        assert_eq!(plain(&lines[0]), "> list the files");
        assert_eq!(plain(&lines[1]), ""); // the separator
        assert_eq!(plain(&lines[2]), "● here they are");
    }

    /// Assistant-side rows (thinking/tool/text) stay tight with no separator —
    /// only the User→Assistant boundary gets the blank line.
    #[test]
    fn assistant_internal_rows_stay_tight_without_separator() {
        let entries = vec![
            entry_of(EntryKind::User, "go"),
            entry_of(EntryKind::Assistant, "working"),
            entry_of(EntryKind::Thinking, "planning..."),
            entry_of(EntryKind::Tool, "bash: ls"),
            entry_of(EntryKind::Assistant, "done"),
        ];

        let lines = visible_transcript_lines(&entries, 10, usize::MAX, 80);
        // Index 0 = user prompt, 1 = separator (User→Assistant), 2 = assistant,
        // then thinking/tool/assistant follow with NO separators between them.
        assert_eq!(plain(&lines[0]), "> go");
        assert_eq!(plain(&lines[1]), "");
        assert_eq!(plain(&lines[2]), "● working");
        assert_eq!(plain(&lines[3]), "💭 planning...");
        assert_eq!(plain(&lines[4]), "🔧 bash: ls");
        assert_eq!(plain(&lines[5]), "● done");
    }

    /// The second turn's User prompt is separated from the previous turn's
    /// Assistant reply by a blank line too (Assistant→User boundary) — without
    /// this, a follow-up prompt crowds the reply above it.
    #[test]
    fn blank_line_separates_assistant_reply_from_next_user_prompt() {
        let entries = vec![
            entry_of(EntryKind::User, "first"),
            entry_of(EntryKind::Assistant, "reply one"),
            entry_of(EntryKind::User, "second"),
            entry_of(EntryKind::Assistant, "reply two"),
        ];

        let lines = visible_transcript_lines(&entries, 12, usize::MAX, 80);
        assert_eq!(plain(&lines[0]), "> first");
        assert_eq!(plain(&lines[1]), ""); // User→Assistant
        assert_eq!(plain(&lines[2]), "● reply one");
        assert_eq!(plain(&lines[3]), ""); // Assistant→User
        assert_eq!(plain(&lines[4]), "> second");
        assert_eq!(plain(&lines[5]), ""); // User→Assistant
        assert_eq!(plain(&lines[6]), "● reply two");
    }
}

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
use crate::tui::state::ConversationEntry;

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
    let mut chunks: Vec<Vec<Line<'static>>> = Vec::new();
    let mut rendered_lines = 0usize;

    for entry in entries.iter().rev() {
        let chunk = render_entry(entry, render_width);
        rendered_lines = rendered_lines.saturating_add(chunk.len());
        chunks.push(chunk);
        if rendered_lines >= needed_lines {
            break;
        }
    }

    let mut lines = Vec::with_capacity(rendered_lines);
    for chunk in chunks.into_iter().rev() {
        lines.extend(chunk);
    }

    if lines.is_empty() {
        return vec![Line::from("")];
    }

    let max_offset = lines.len().saturating_sub(viewport_lines);
    let offset = requested_offset.min(max_offset);
    let end = lines.len().saturating_sub(offset);
    let start = end.saturating_sub(viewport_lines);
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
}

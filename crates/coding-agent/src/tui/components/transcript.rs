//! Transcript — scrollable conversation view.

use std::cell::RefCell;
use std::rc::Rc;

use iodilos::node::TuiNode;
use iodilos::prelude::*;
use iodilos_md::StreamingParser;

use crate::tui::components::message_block::render_entry;
use crate::tui::state::{ConversationEntry, EntryKind, TerminalSize, UiState};

const TRANSCRIPT_CHROME_COLS: u16 = 4;

pub fn transcript() -> View {
    let stack = use_context::<Rc<crate::tui::conversation::ConversationStack>>();
    let term_size = use_context::<TerminalSize>();
    let active_index = stack.active_index_signal();
    let parsers = Rc::new(RefCell::new(Vec::<StreamingParser>::new()));
    // Persistent render cache: survives effect re-runs so a streaming delta
    // only re-renders the changed (last) entry, not the whole conversation.
    let cache = Rc::new(RefCell::new(RenderCache::default()));

    // The flex_grow div is built ONCE (not under a Dynamic), so its flex_grow
    // is honoured by the App column and the prompt is anchored to the bottom.
    // Only the inner content is dynamic: it re-renders on active-layer changes,
    // terminal resizes, scroll, and entry/streaming updates.
    let content = View::from_dynamic(move || {
        active_index.get(); // re-run when the active layer switches
        term_size.cols.get();
        term_size.rows.get();
        let state = Rc::clone(&stack.active().state);
        let viewport_rows = transcript_viewport_rows(term_size.rows.get());
        let render_width = transcript_render_width(term_size.cols.get());
        transcript_surface_view(
            &state,
            viewport_rows,
            render_width,
            Rc::clone(&parsers),
            Rc::clone(&cache),
        )
    });

    View::from(
        tags::div()
            .flex_grow(1.0)
            .flex_shrink(1.0)
            .min_height(0)
            .width(Size::Percent(100.0))
            .margin_bottom(1)
            .overflow(Overflow::Hidden)
            .children(content),
    )
}

pub fn transcript_for_state(state: Rc<UiState>) -> View {
    let term_size = use_context::<TerminalSize>();
    let parsers = Rc::new(RefCell::new(Vec::<StreamingParser>::new()));
    let cache = Rc::new(RefCell::new(RenderCache::default()));
    let content = View::from_dynamic(move || {
        term_size.cols.get();
        term_size.rows.get();
        let viewport_rows = transcript_viewport_rows(term_size.rows.get());
        let render_width = transcript_render_width(term_size.cols.get());
        transcript_surface_view(
            &state,
            viewport_rows,
            render_width,
            Rc::clone(&parsers),
            Rc::clone(&cache),
        )
    });

    View::from(
        tags::div()
            .flex_grow(1.0)
            .flex_shrink(1.0)
            .min_height(0)
            .width(Size::Percent(100.0))
            .margin_bottom(1)
            .overflow(Overflow::Hidden)
            .children(content),
    )
}

/// A render cache for the transcript surface. Holds the already-rendered
/// `TextRow`s for each entry plus the `(text, width)` they were rendered at,
/// so a streaming delta only re-renders the changed (last) entry instead of
/// the whole conversation every token.
#[derive(Default)]
struct RenderCache {
    /// Cached rendered rows for each entry (NOT including the inter-turn
    /// blank separator, which is cheap to re-insert).
    rows: Vec<Vec<TextRow>>,
    /// The `(entry_text, render_width)` each cached entry was produced from.
    keys: Vec<(String, usize)>,
}

impl RenderCache {
    fn sync_len(&mut self, len: usize) {
        self.rows.truncate(len);
        self.keys.truncate(len);
        while self.rows.len() < len {
            self.rows.push(Vec::new());
            self.keys.push((String::new(), 0));
        }
    }
}

/// Build the dynamic text-surface view for the active state. Reads
/// `scroll_offset` and `entries` so streaming updates and scroll re-render it.
/// The cache (held in the caller's closure) survives re-runs so only the
/// changed entry is re-rendered on a streaming delta.
fn transcript_surface_view(
    state: &Rc<UiState>,
    viewport_rows: usize,
    render_width: usize,
    parsers: Rc<RefCell<Vec<StreamingParser>>>,
    cache: Rc<RefCell<RenderCache>>,
) -> View {
    let scroll_offset = state.scroll_offset.get();
    let entries = state.entries.get_clone();
    let rendered = render_all_entries_cached(&entries, render_width, &parsers, &cache);

    if scroll_offset == usize::MAX {
        // Auto-follow (stick to bottom): hand the FULL rendered conversation to
        // the layout and let the paint path show the last `visible_height` rows,
        // where `visible_height` is taffy's real allocation for this node. This
        // is robust to the editor growing (slash menu, multiline input) — the
        // component no longer has to guess how many rows the transcript can
        // show, so the latest streamed row is never painted under the editor.
        let scroll = rendered
            .len()
            .try_into()
            .unwrap_or(i32::MAX)
            .saturating_add(1_000_000);
        View::from_node(TuiNode::create_text_surface_node(
            TextSurface::from_rows(rendered),
            scroll,
        ))
    } else {
        // Manual scroll: resolve the requested offset against the (estimated)
        // viewport height and hand the paint path a pre-sliced window at
        // scroll 0. `viewport_rows` is only an estimate here, but a manual
        // scroll means the user has already left the bottom, so a one-or-two
        // row imprecision at the top of the window is not load-bearing.
        let window = slice_viewport(rendered, viewport_rows, scroll_offset);
        View::from_node(TuiNode::create_text_surface_node(window, 0))
    }
}

fn transcript_viewport_rows(terminal_height: u16) -> usize {
    // Two rows for the prompt box (status border + input line) at minimum.
    // The transcript also keeps a one-row gap above the prompt. The prompt can
    // grow taller while typing; the transcript's flex_grow absorbs the
    // difference, so this only sizes the scroll window.
    terminal_height.saturating_sub(3).max(1) as usize
}

fn transcript_render_width(terminal_width: u16) -> usize {
    terminal_width.saturating_sub(TRANSCRIPT_CHROME_COLS).max(1) as usize
}

/// The text content an entry was last rendered from, for cache-keying. Two
/// entries with the same text render to the same rows at the same width.
///
/// Returns `Cow<str>` because a [`EntryKind::ToolResult`] is keyed by both its
/// source `tool` (which picks the renderer) and its `output`; embedding the
/// tool name into the key prevents a bash result and a read result with the
/// same body from sharing a cached render.
fn entry_text(entry: &ConversationEntry) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    match &entry.kind {
        EntryKind::User(s) | EntryKind::Assistant(s) | EntryKind::Thinking(s) => {
            Cow::Borrowed(s)
        }
        EntryKind::Tool { text, .. } => Cow::Borrowed(text),
        EntryKind::ToolResult { tool, output } => {
            // Prefix with the tool tag so different tools never collide.
            Cow::Owned(format!("[tool_result:{tool}]\n{output}"))
        }
        EntryKind::Error(text)
        | EntryKind::Warning(text)
        | EntryKind::System(text) => Cow::Borrowed(text),
    }
}

/// Test-facing convenience: render the full conversation and resolve the
/// viewport the way the live (manual-scroll) path does. The live stick-to-
/// bottom path no longer slices — it hands the full surface to the layout — so
/// this helper only exercises the slicing math used by manual scroll.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn visible_transcript_surface(
    entries: &[ConversationEntry],
    viewport_rows: usize,
    scroll_offset: usize,
    render_width: usize,
    parsers: &Rc<RefCell<Vec<StreamingParser>>>,
) -> TextSurface {
    let cache = RefCell::new(RenderCache::default());
    let rendered = render_all_entries_cached(entries, render_width, parsers, &cache);
    if scroll_offset == usize::MAX {
        // Mirror the live stick-to-bottom path: return the full rendered
        // surface (the caller / paint path clips to the visible height).
        return TextSurface::from_rows(rendered);
    }
    slice_viewport(rendered, viewport_rows, scroll_offset)
}

/// Render every entry (with turn-boundary blanks) into a flat `Vec<TextRow>`,
/// using the persistent cache so a streaming delta only re-renders the changed
/// (last) entry. The cache is held by the caller and survives re-runs.
fn render_all_entries_cached(
    entries: &[ConversationEntry],
    render_width: usize,
    parsers: &Rc<RefCell<Vec<StreamingParser>>>,
    cache: &RefCell<RenderCache>,
) -> Vec<TextRow> {
    let mut rendered = Vec::new();
    let mut parser_store = parsers.borrow_mut();
    if parser_store.len() < entries.len() {
        parser_store.resize_with(entries.len(), StreamingParser::new);
    } else if parser_store.len() > entries.len() {
        parser_store.truncate(entries.len());
    }
    let mut cache = cache.borrow_mut();
    cache.sync_len(entries.len());

    let mut prev: Option<&ConversationEntry> = None;
    for (idx, entry) in entries.iter().enumerate() {
        if let Some(prev) = prev
            && is_turn_boundary(prev, entry)
        {
            rendered.push(TextRow::raw(""));
        }
        let text = entry_text(entry);
        // Decide whether to reuse the cached rows for this entry BEFORE taking
        // a mutable borrow on the cache (it would otherwise conflict with the
        // key read/write below).
        let reuse = !cache.rows[idx].is_empty()
            && cache.keys[idx].0 == text
            && cache.keys[idx].1 == render_width;
        if reuse {
            rendered.extend_from_slice(&cache.rows[idx]);
        } else {
            let parser = match entry.kind {
                EntryKind::Assistant(_) => Some(&mut parser_store[idx]),
                _ => None,
            };
            let fresh = render_entry(entry, render_width, parser);
            cache.keys[idx] = (text.to_string(), render_width);
            rendered.extend_from_slice(&fresh);
            cache.rows[idx] = fresh;
        }
        prev = Some(entry);
    }

    if rendered.is_empty() {
        rendered.push(TextRow::raw(""));
    }
    rendered
}

/// Resolve a manual scroll offset against the (estimated) viewport height and
/// return the pre-sliced window. `scroll_offset` is lines hidden *below* the
/// viewport; `usize::MAX` is handled by the caller (stick-to-bottom) and never
/// reaches here.
fn slice_viewport(
    rendered: Vec<TextRow>,
    viewport_rows: usize,
    scroll_offset: usize,
) -> TextSurface {
    let viewport_rows = viewport_rows.max(1);
    let requested_offset = scroll_offset;
    let max_offset = rendered.len().saturating_sub(viewport_rows);
    let offset = requested_offset.min(max_offset);
    let end = rendered.len().saturating_sub(offset);
    let start = end.saturating_sub(viewport_rows);
    TextSurface::from_rows(rendered[start..end].to_vec())
}

fn is_turn_boundary(prev: &ConversationEntry, entry: &ConversationEntry) -> bool {
    matches!(
        (&prev.kind, &entry.kind),
        (EntryKind::User(_), EntryKind::Assistant(_))
            | (EntryKind::Assistant(_), EntryKind::User(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_of(make_kind: impl FnOnce(String) -> EntryKind, text: &str) -> ConversationEntry {
        ConversationEntry {
            kind: make_kind(text.to_string()),
        }
    }

    fn plain(surface: &TextSurface, row: usize) -> String {
        surface.rows()[row]
            .segments
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn visible_rows_follow_the_bottom_by_default() {
        // Stick-to-bottom (scroll_offset == MAX) now returns the FULL surface;
        // the layout's paint path clips it to the real node height. So the
        // surface carries every entry, and the LAST row is the newest one —
        // that is what the paint path shows when it clips to the bottom.
        let entries = vec![
            entry_of(EntryKind::System, "one"),
            entry_of(EntryKind::System, "two"),
            entry_of(EntryKind::System, "three"),
            entry_of(EntryKind::System, "four"),
        ];
        let parsers = Rc::new(RefCell::new(Vec::new()));
        let surface = visible_transcript_surface(&entries, 2, usize::MAX, 80, &parsers);
        assert_eq!(
            surface.row_count(),
            4,
            "full surface returned on stick-to-bottom"
        );
        assert_eq!(plain(&surface, 0), "info one");
        assert_eq!(
            plain(&surface, 3),
            "info four",
            "last row is the newest entry"
        );
    }

    #[test]
    fn viewport_rows_reserve_prompt_and_gap() {
        assert_eq!(transcript_viewport_rows(8), 5);
        assert_eq!(transcript_viewport_rows(2), 1);
    }

    #[test]
    fn blank_line_separates_user_prompt_from_assistant_reply() {
        let entries = vec![
            entry_of(EntryKind::User, "list"),
            entry_of(EntryKind::Assistant, "done"),
        ];
        let parsers = Rc::new(RefCell::new(Vec::new()));
        let surface = visible_transcript_surface(&entries, 10, usize::MAX, 80, &parsers);
        assert_eq!(plain(&surface, 1), "");
    }

    /// The render cache must produce the same surface across re-renders and
    /// pick up a streaming delta on the last entry. This mirrors the live
    /// streaming path, which re-runs the cached renderer on every token.
    #[test]
    fn streaming_growth_keeps_last_row_visible_when_stuck_to_bottom() {
        // The stick-to-bottom path now hands the FULL rendered conversation to
        // the layout (the paint path clips to the real node height). So this
        // test asserts the full surface always carries the latest streamed word
        // as its LAST row, regardless of how many rows precede it — that is what
        // makes the paint path's clip land on the newest content.
        let parsers = Rc::new(RefCell::new(Vec::new()));
        let cache = RefCell::new(RenderCache::default());
        let mut entries = vec![entry_of(EntryKind::Assistant, "alpha")];
        let width = 80;

        for word in ["beta", "gamma", "delta", "epsilon", "zeta", "eta"] {
            let EntryKind::Assistant(body) = &mut entries[0].kind else {
                unreachable!();
            };
            body.push_str(" ");
            body.push_str(word);
            let rows = render_all_entries_cached(&entries, width, &parsers, &cache);
            assert!(!rows.is_empty(), "surface should always have rows");
            let last_row: String = rows
                .last()
                .unwrap()
                .segments
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert!(
                last_row.contains(word),
                "streamed word {word:?} should be in the LAST row of the full surface, got: {last_row:?}"
            );
        }
    }

    #[test]
    fn streaming_markdown_growth_keeps_last_content_visible() {
        // A realistic streaming tick: paragraphs accumulate, blank-line
        // separated, at a narrow width so each wraps and the whole thing grows
        // well past any viewport. The full rendered surface's last row must
        // always carry the most recent word — that is what the paint path's
        // stick-to-bottom clip will show.
        let parsers = Rc::new(RefCell::new(Vec::new()));
        let cache = RefCell::new(RenderCache::default());
        let width = 20;

        let mut entries = vec![entry_of(EntryKind::Assistant, "")];
        let sentences = [
            "the quick brown fox jumps over the lazy dog",
            "pack my box with five dozen liquor jugs",
            "sphinx of black quartz judge my vow",
        ];
        let mut joined = String::new();
        for (n, sentence) in sentences.iter().enumerate() {
            if n > 0 {
                joined.push_str("\n\n");
            }
            for (i, word) in sentence.split_whitespace().enumerate() {
                if i > 0 {
                    joined.push(' ');
                }
                joined.push_str(word);
                let EntryKind::Assistant(body) = &mut entries[0].kind else {
                    unreachable!();
                };
                *body = joined.clone();
                let rows = render_all_entries_cached(&entries, width, &parsers, &cache);
                // The most recent word must appear somewhere in the surface
                // (the paint path's clip then shows the tail containing it).
                let contains_word = rows
                    .iter()
                    .any(|r| r.segments.iter().any(|s| s.content.as_ref().contains(word)));
                assert!(
                    contains_word,
                    "streamed word {word:?} (sentence {n}) should be present in the full surface"
                );
            }
        }
    }

    #[test]
    fn slice_viewport_manual_scroll_math() {
        // `scroll_offset` counts rows hidden BELOW the viewport, so offset 0
        // shows the BOTTOM of the content (stick-to-bottom), and a larger
        // offset scrolls UP toward the top. A huge offset clamps at the top.
        let rows: Vec<TextRow> = (0..10).map(|i| TextRow::raw(i.to_string())).collect();

        // offset 0 → bottom window (rows 7,8,9).
        let bottom = slice_viewport(rows.clone(), 3, 0);
        assert_eq!(bottom.row_count(), 3);
        assert_eq!(plain(&bottom, 0), "7");
        assert_eq!(plain(&bottom, 2), "9");

        // offset 1 → one row higher (rows 6,7,8).
        let one_up = slice_viewport(rows.clone(), 3, 1);
        assert_eq!(plain(&one_up, 0), "6");
        assert_eq!(plain(&one_up, 2), "8");

        // huge offset → clamps to the top (rows 0,1,2).
        let top = slice_viewport(rows, 3, usize::MAX / 2);
        assert_eq!(top.row_count(), 3);
        assert_eq!(plain(&top, 0), "0");
        assert_eq!(plain(&top, 2), "2");
    }

    #[test]
    fn cached_renderer_reuses_unchanged_entries_and_reflects_streaming_delta() {
        let parsers = Rc::new(RefCell::new(Vec::new()));
        let cache = RefCell::new(RenderCache::default());
        let entries = vec![
            entry_of(EntryKind::System, "hello"),
            entry_of(EntryKind::Assistant, "partial"),
        ];
        let surface_of = |rows: Vec<TextRow>| TextSurface::from_rows(rows);

        // First render populates the cache.
        let first = surface_of(render_all_entries_cached(&entries, 80, &parsers, &cache));
        assert_eq!(first.row_count(), 2);
        assert_eq!(plain(&first, 1), "● partial");

        // Second render with the SAME entries: cache reused, identical surface.
        let second = surface_of(render_all_entries_cached(&entries, 80, &parsers, &cache));
        assert_eq!(plain(&first, 0), plain(&second, 0));
        assert_eq!(plain(&first, 1), plain(&second, 1));

        // Streaming delta: only the assistant entry's text changes.
        let mut streamed = entries.clone();
        let EntryKind::Assistant(body) = &mut streamed[1].kind else {
            unreachable!();
        };
        body.push_str(" answer");
        let third = surface_of(render_all_entries_cached(&streamed, 80, &parsers, &cache));
        assert_eq!(plain(&third, 0), "info hello", "unchanged entry reused");
        assert_eq!(
            plain(&third, 1),
            "● partial answer",
            "streamed entry re-rendered"
        );
    }
}

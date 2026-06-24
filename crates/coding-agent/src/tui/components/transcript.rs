//! Transcript — the conversation view, built on `iodilos::StreamingList`.
//!
//! The transcript is a keyed scrollable list of `ConversationEntry`s:
//!
//! - Items are keyed by [`ConversationEntry::id`], so surviving entries keep
//!   their per-item scope and mapped view across `entries.set(..)`. Streaming
//!   updates targeting an entry's body `Signal<String>` re-render only that
//!   entry's reactive region — see [`entry_view`](super::entry_view::entry_view).
//! - Scrolling is the element-level `scroll` style on the StreamingList's
//!   container: `i32::MAX` = stick to bottom; negative = scrolled up by that
//!   many rows from stick-to-bottom (the natural mapping of the user-facing
//!   `state.scroll_offset` "lines hidden below the viewport" semantic).
//!
//! This replaces the legacy row-buffer pipeline (`RenderCache` +
//! `slice_viewport` + `transcript_surface_view` + manual stick-to-bottom
//! arithmetic). The keyed engine handles per-entry view reuse, and the paint
//! path handles the scroll math.

use std::rc::Rc;

use iodilos::prelude::*;

use crate::tui::components::entry_view::{entry_view, is_turn_boundary};
use crate::tui::state::{ConversationEntry, UiState};

/// Build the transcript view rooted at the active layer's `UiState`. Reads
/// `ConversationStack` via context to pick the live layer.
pub fn transcript() -> View {
    let stack = use_context::<Rc<crate::tui::conversation::ConversationStack>>();
    let active_index = stack.active_index_signal();

    // The flex_grow container is built ONCE so the App column's layout is
    // stable: the StreamingList lives inside a dynamic region that swaps
    // entire layer-state on `active_index` change.
    let content = View::from_dynamic(move || {
        let _ = active_index.get(); // re-run when the active layer switches
        let state = Rc::clone(&stack.active().state);
        transcript_for_state(state)
    });

    tags::div()
        .flex_grow(1.0)
        .flex_shrink(1.0)
        .min_height(0)
        .width(Size::Percent(100.0))
        .margin_bottom(1)
        .overflow(Overflow::Hidden)
        .children(content)
        .into()
}

/// Build a transcript view bound to a specific `UiState`. Used both by the
/// live transcript (driven by `ConversationStack::active`) and by the
/// conversation overlay (which inspects a non-active layer).
pub fn transcript_for_state(state: Rc<UiState>) -> View {
    // Memoised reactive view of the entries with the turn-boundary
    // top-margin flag flattened onto each one. The flag is part of the entry
    // tuple so that, even though the keyed engine uses `id` to reuse a
    // surviving entry's mapped view, the boundary status is captured at
    // first-render time. (Boundary changes are rare — only the very first
    // adjacent entry's flag could flip after a mutation that inserts at the
    // join.)
    let entries_signal = state.entries;
    let with_boundaries = create_memo(move || {
        let entries = entries_signal.get_clone();
        let mut out = Vec::with_capacity(entries.len());
        let mut prev: Option<&ConversationEntry> = None;
        for entry in &entries {
            let top_margin = prev.is_some_and(|p| is_turn_boundary(p, entry));
            out.push((entry.clone(), top_margin));
            prev = Some(entry);
        }
        out
    });

    // Translate `state.scroll_offset` (usize, "lines hidden below the
    // viewport"; usize::MAX = stick to bottom) into the element `scroll` value
    // expected by StreamingList:
    //   - usize::MAX             → i32::MAX  (stick to bottom)
    //   - n: usize (small)       → -(n as i32) (scrolled up by n rows)
    //   - n: usize (overflow)    → i32::MIN  (still treated as "very up" by
    //                                          the paint clamp to top)
    let scroll_signal = state.scroll_offset;
    let scroll = move || {
        let off = scroll_signal.get();
        if off == usize::MAX {
            i32::MAX
        } else if off > i32::MAX as usize {
            i32::MIN
        } else {
            -(off as i32)
        }
    };

    view! {
        StreamingList(
            items = with_boundaries,
            key = |item: &(ConversationEntry, bool)| item.0.id,
            view = |item: &(ConversationEntry, bool)| {
                entry_view(item.0.clone(), item.1)
            },
            scroll = scroll,
        )
    }
}

//! Per-entry view: turns a `ConversationEntry` into an iodilos `View`.
//!
//! This is the load-bearing per-key view of the transcript's
//! [`StreamingList`]. The keyed engine runs each entry's `entry_view` **once**
//! per id; surviving entries keep their mapped view across `entries.set(..)`,
//! and streaming updates are driven by per-item signals read inside the
//! reactive regions this function builds — see `EntryKind::Assistant` /
//! `Thinking` below.
//!
//! Rendering reuses the existing `message_block::render_entry` row-batching
//! logic verbatim: the entry shapes itself into a `Vec<Row>`, which becomes a
//! [`Lines`] producer wrapped in a leaf node. The `div` layer adds the entry's
//! top-margin (turn boundary) and any future per-kind chrome. The streaming
//! variants build the leaf inside a `View::from_dynamic` closure that reads
//! the per-item body `Signal<String>` plus the width signal — so a streamed
//! token triggers a single-entry re-render with no other side effects.
//!
//! The streaming `Assistant` path additionally owns a per-item
//! [`StreamingParser`] in an `Rc<RefCell<_>>`: the parser caches the committed
//! markdown prefix and only re-parses the open tail on each tick (see
//! `iodilos_md::stream`). The parser is **owned by the view closure's scope**,
//! so it survives across keyed map re-renders just like the body signal —
//! losing it would destroy the prefix cache and force a whole-message re-parse
//! every token.

use std::cell::RefCell;
use std::rc::Rc;

use iodilos::node::TuiNode;
use iodilos::prelude::*;
use iodilos::producer::Lines;
use iodilos_md::StreamingParser;

use crate::tui::components::message_block::{Row, render_entry};
use crate::tui::state::{ConversationEntry, EntryKind, TerminalSize};

/// Two cells of horizontal padding flank every entry's rendered surface, so the
/// content width passed to `render_entry` is `terminal_cols - TRANSCRIPT_CHROME`.
/// Matches the historical `TRANSCRIPT_CHROME_COLS` constant from the old
/// `transcript.rs` so per-entry wrapping looks identical to the pre-component
/// rendering.
pub const TRANSCRIPT_CHROME: u16 = 4;

/// Compute the render width (in cells) at which entries should shape, given
/// the current terminal column count.
pub fn entry_render_width(terminal_cols: u16) -> usize {
    terminal_cols.saturating_sub(TRANSCRIPT_CHROME).max(1) as usize
}

/// Build the View for a single conversation entry.
///
/// The returned View is a `div` carrying the entry's vertical separation
/// (`margin_top`) and a single leaf with the shaped rows. The width signal is
/// read inside any dynamic regions so a terminal resize re-shapes the entry's
/// rows in place; for the non-streaming kinds the whole leaf is wrapped in a
/// `from_dynamic` so resize alone triggers re-shape without depending on the
/// streaming body.
pub fn entry_view(entry: ConversationEntry, top_margin: bool) -> View {
    let term_size = use_context::<TerminalSize>();
    let cols = term_size.cols;

    // The leaf is wrapped in a `from_dynamic` so terminal resizes (and, for
    // streaming variants, body updates) re-shape the same entry's rows. The
    // surrounding `div` carries the static chrome (top margin) and is built
    // once — keyed reuse keeps it identical across list mutations.
    let leaf: View = match entry.kind.clone() {
        EntryKind::Assistant(body) => {
            // Per-entry StreamingParser owned by the keyed-map's per-item
            // scope. It caches the committed markdown prefix and is reused
            // across body.set(..) ticks; only the open tail is re-parsed.
            let parser = Rc::new(RefCell::new(StreamingParser::new()));
            View::from_dynamic(move || {
                // Read both signals so this region tracks both dependencies.
                let width = entry_render_width(cols.get());
                let _ = body.get_clone(); // dependency
                let mut p = parser.borrow_mut();
                let synthetic = ConversationEntry {
                    id: entry.id,
                    kind: EntryKind::Assistant(body),
                };
                let rows = render_entry(&synthetic, width, Some(&mut p));
                View::from_node(TuiNode::create_leaf_node(
                    Box::new(Lines::new(rows)),
                    0,
                ))
            })
        }
        EntryKind::Thinking(body) => View::from_dynamic(move || {
            let width = entry_render_width(cols.get());
            let _ = body.get_clone(); // dependency
            let synthetic = ConversationEntry {
                id: entry.id,
                kind: EntryKind::Thinking(body),
            };
            let rows = render_entry(&synthetic, width, None);
            View::from_node(TuiNode::create_leaf_node(
                Box::new(Lines::new(rows)),
                0,
            ))
        }),
        // Non-streaming kinds: the body is fixed at push time, but a terminal
        // resize must still re-shape the rows. Clone the entry once and read
        // it inside the dynamic region; the cost is bounded by terminal
        // resize events (rare) plus the keyed reuse (once per id at first
        // render).
        _ => {
            let entry = entry.clone();
            View::from_dynamic(move || {
                let width = entry_render_width(cols.get());
                let rows: Vec<Row> = render_entry(&entry, width, None);
                View::from_node(TuiNode::create_leaf_node(
                    Box::new(Lines::new(rows)),
                    0,
                ))
            })
        }
    };

    let mut wrapper = tags::div();
    if top_margin {
        wrapper = wrapper.margin_top(1);
    }
    wrapper.children(leaf).into()
}

/// Whether the boundary between `prev` and `entry` deserves a blank-line
/// separator. Mirrors the historical `is_turn_boundary` from `transcript.rs`:
/// only a User↔Assistant swap counts. Other adjacencies (tool/result, etc.)
/// hug each other.
pub fn is_turn_boundary(prev: &ConversationEntry, entry: &ConversationEntry) -> bool {
    matches!(
        (&prev.kind, &entry.kind),
        (EntryKind::User(_), EntryKind::Assistant(_))
            | (EntryKind::Assistant(_), EntryKind::User(_))
    )
}

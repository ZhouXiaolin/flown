//! HintBar — bottom bar showing keyboard shortcuts.
//!
//! Reads `state.busy` and renders one of two hint lines as a `RichText`. The
//! busy variant shows the abort hint; the idle variant shows the full shortcut
//! list. Mirrors the old `HintBar` component.
//!
//! The component body runs once at mount; to make the line swap reactively when
//! `busy` flips, a `create_effect` re-derives the line (reading `busy`) and
//! calls `set_lines` whenever it changes. This is the iodilos idiom for a
//! node whose content depends on a signal.

use std::rc::Rc;

use iodilos::prelude::*;


#[component]
pub fn HintBar() -> Node {
    let stack = use_context::<Rc<crate::tui::conversation::ConversationStack>>();
    let state = Rc::clone(&stack.active().state);
    let busy = state.busy;

    let node = Node::new_richtext();
    // Seed the initial line (idle), then keep it in sync with `busy`.
    let seed_node = node.clone();
    create_effect(move || {
        let line = if busy.get() {
            Line::from(vec![
                Span::styled("  ⟳ ", Style::default().fg(Color::Yellow)),
                Span::styled("thinking…", Style::default().fg(Color::Yellow)),
                Span::styled("  Esc ", Style::default().fg(Color::DarkGray)),
                Span::styled("abort", Style::default().fg(Color::Red)),
            ])
        } else {
            Line::from(vec![
                Span::styled("  ⏎ ", Style::default().fg(Color::DarkGray)),
                Span::styled("send", Style::default().fg(Color::Green)),
                Span::styled("  ⇧⏎ ", Style::default().fg(Color::DarkGray)),
                Span::styled("newline", Style::default().fg(Color::Green)),
                Span::styled("  / ", Style::default().fg(Color::DarkGray)),
                Span::styled("commands", Style::default().fg(Color::Green)),
                Span::styled("  Tab ", Style::default().fg(Color::DarkGray)),
                Span::styled("accept", Style::default().fg(Color::Green)),
                Span::styled("  Esc ", Style::default().fg(Color::DarkGray)),
                Span::styled("cancel", Style::default().fg(Color::Green)),
            ])
        };
        seed_node.set_lines(vec![line]);
    });
    node
}

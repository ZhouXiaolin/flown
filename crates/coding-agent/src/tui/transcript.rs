//! Transcript — a scrollable list of `MessageBlock` components.
//!
//! Each message is an independent `Component` with its own render cache.
//! Streaming updates only invalidate the last child's cache.

use std::sync::Arc;

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation};
use ratatui::Frame;

use super::component::Component;
use super::message_block::{MessageBlock, MessageKind, RenderAssets};

/// Scrollable transcript panel — the main conversation view.
///
/// Internally holds a `Vec<MessageBlock>` — each message is an independent
/// `Component` with its own render cache. Named after oh-my-pi's
/// `TranscriptContainer`.
pub struct Transcript {
    messages: Vec<MessageBlock>,
    scroll_offset: usize,
    total_lines: usize,
    assets: Arc<RenderAssets>,
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            scroll_offset: 0,
            total_lines: 0,
            assets: Arc::new(RenderAssets::new()),
        }
    }

    // ── Push convenience methods ──────────────────────────────────────

    pub fn push_user(&mut self, text: impl Into<String>) {
        self.push(MessageKind::User, text.into());
    }

    pub fn push_assistant(&mut self, text: impl Into<String>) {
        self.push(MessageKind::Assistant, text.into());
    }

    pub fn push_thinking(&mut self, text: impl Into<String>) {
        self.push(MessageKind::Thinking, text.into());
    }

    pub fn push_tool(&mut self, text: impl Into<String>) {
        self.push(MessageKind::Tool, text.into());
    }

    pub fn push_error(&mut self, text: impl Into<String>) {
        self.push(MessageKind::Error, text.into());
    }

    pub fn push_system(&mut self, text: impl Into<String>) {
        self.push(MessageKind::System, text.into());
    }

    fn push(&mut self, kind: MessageKind, body: String) {
        self.messages
            .push(MessageBlock::new(kind, body, self.assets.clone()));
        self.scroll_to_bottom();
    }

    // ── Streaming support ─────────────────────────────────────────────

    /// Append text to the last assistant message. Returns `true` if appended.
    pub fn append_to_assistant(&mut self, text: &str) -> bool {
        if let Some(last) = self.messages.last_mut() {
            if last.kind() == MessageKind::Assistant {
                last.push_body(text);
                self.scroll_to_bottom();
                return true;
            }
        }
        false
    }

    /// Check if the last message is an assistant message.
    pub fn last_is_assistant(&self) -> bool {
        self.messages
            .last()
            .is_some_and(|m| m.kind() == MessageKind::Assistant)
    }

    // ── General mutators ──────────────────────────────────────────────

    pub fn clear(&mut self) {
        self.messages.clear();
        self.scroll_offset = 0;
        self.total_lines = 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = usize::MAX;
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
    }

    // ── Render ────────────────────────────────────────────────────────

    /// Build all lines by rendering each `MessageBlock` and joining with
    /// blank-line separators.
    fn build_lines(&mut self, width: u16) -> Vec<Line<'static>> {
        let estimated: usize = self.messages.iter().map(|_| 8).sum();
        let mut all_lines = Vec::with_capacity(estimated);

        for (i, msg) in self.messages.iter_mut().enumerate() {
            if i > 0 {
                all_lines.push(Line::from(""));
            }
            all_lines.extend(msg.render(width));
        }
        all_lines
    }

    /// Render to a ratatui `Frame`, handling viewport clipping and scrollbar.
    pub fn render_frame(&mut self, f: &mut Frame, area: Rect) {
        let block = Block::default().borders(Borders::ALL);

        let inner = block.inner(area);
        let viewport_height = inner.height as usize;
        let render_width = inner.width as usize;

        let all_lines = self.build_lines(render_width as u16);
        self.total_lines = all_lines.len();

        if self.scroll_offset == usize::MAX
            || self.scroll_offset + viewport_height >= self.total_lines
        {
            self.scroll_offset = self.total_lines.saturating_sub(viewport_height);
        }

        let visible: Vec<Line> = all_lines
            .into_iter()
            .skip(self.scroll_offset)
            .take(viewport_height)
            .collect();

        let paragraph = Paragraph::new(visible).block(block);
        f.render_widget(paragraph, area);

        if self.total_lines > viewport_height {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_symbol("█")
                .track_symbol(Some("░"));
            let mut state =
                ratatui::widgets::ScrollbarState::new(self.total_lines).position(self.scroll_offset);
            f.render_stateful_widget(scrollbar, area, &mut state);
        }
    }
}

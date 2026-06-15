//! Reactive state model for the iodilos-based TUI.
//!
//! All UI state lives in iodilos `RwSignal`s held by [`UiState`], which is
//! provided via context at the mount root and read by the components. The
//! cross-runtime event pump in `runtime.rs` mutates these signals as
//! `AgentEvent`s arrive from the tokio side; signal writes flip iodilos's dirty
//! flag (via effects) and the renderer redraws.
//!
//! This mirrors the old `Transcript` / `StatusLine` / `Editor` structs, but
//! reactive: instead of a poll loop mutating struct fields and calling
//! `render_frame`, each component reads the signals it needs and iodilos
//! re-runs the dependent effects automatically.

use std::rc::Rc;

use iodilos::prelude::*;

use super::editor::SlashPopup;
use super::tool_format::format_tool_call;

// ── Conversation model ────────────────────────────────────────────────

/// One row in the transcript.
#[derive(Debug, Clone)]
pub struct ConversationEntry {
    pub kind: EntryKind,
}

/// The kind + body of a transcript entry. Mirrors the old `MessageKind` enum.
#[derive(Debug, Clone)]
pub enum EntryKind {
    User(String),
    Assistant(String),
    Thinking(String),
    Tool(String),
    Error(String),
    System(String),
}

impl EntryKind {
    /// A short label for the kind (used for debugging / accessibility).
    pub fn label(&self) -> &'static str {
        match self {
            EntryKind::User(_) => "user",
            EntryKind::Assistant(_) => "assistant",
            EntryKind::Thinking(_) => "thinking",
            EntryKind::Tool(_) => "tool",
            EntryKind::Error(_) => "error",
            EntryKind::System(_) => "system",
        }
    }
}

// ── Status-line model ─────────────────────────────────────────────────

/// Snapshot of everything the status line renders. Updated by the event pump
/// and by the busy-spinner `every` tick.
#[derive(Debug, Clone, Default)]
pub struct StatusInfo {
    pub model: String,
    pub provider: String,
    pub thinking_level: String,
    pub cwd: String,
    pub git_branch: Option<String>,
    pub git_dirty: bool,
    pub context_pct: f64,
    pub context_total: String,
    pub session_name: Option<String>,
    pub cache_read: u64,
    pub cache_write: u64,
    pub busy: bool,
    /// Current spinner frame index (advances on the `every` tick while busy).
    pub frame: usize,
}

/// Animated spinner glyphs for the busy state (mirrors the old StatusLine).
pub const BUSY_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];

// ── UiState ───────────────────────────────────────────────────────────

/// The shared reactive state, wrapped in `Rc` so it can be provided via iodilos
/// context and cheaply cloned into every component.
///
/// Every field is a `Copy` signal handle (`RwSignal`), so handlers and effects
/// capture them by copy without borrow-checker friction — the iodilos idiom.
pub struct UiState {
    /// The full conversation, oldest first. Drives the transcript `<For>`.
    pub entries: RwSignal<Vec<ConversationEntry>>,
    /// `true` while an agent prompt is streaming.
    pub busy: RwSignal<bool>,
    /// Status-line snapshot.
    pub status: RwSignal<StatusInfo>,
    /// The editor buffer (text, cursor, slash popup).
    pub input: RwSignal<TextAreaState>,
    /// Slash-completion popup state for the editor.
    pub slash_popup: RwSignal<Option<SlashPopup>>,
    /// Scroll offset in lines from the bottom; `usize::MAX` means "stuck to
    /// bottom" (auto-follow). The transcript component resolves this against
    /// the viewport height to pick the visible window.
    pub scroll_offset: RwSignal<usize>,
    /// Accumulator for `ThinkingDelta` until `ThinkingEnd` flushes it as a
    /// single entry (mirrors the old `accumulated_text` / `in_thinking` pair).
    pub thinking_acc: RwSignal<String>,
    /// `true` between `ThinkingStart` and `ThinkingEnd`.
    pub in_thinking: RwSignal<bool>,
}

impl UiState {
    /// Build a fresh state with empty signals. `editor` is the initial editor
    /// state (usually `TextAreaState::default()`).
    pub fn new(editor: TextAreaState) -> Self {
        Self {
            entries: create_rw_signal(Vec::new()),
            busy: create_rw_signal(false),
            status: create_rw_signal(StatusInfo::default()),
            input: create_rw_signal(editor),
            slash_popup: create_rw_signal(None),
            scroll_offset: create_rw_signal(usize::MAX),
            thinking_acc: create_rw_signal(String::new()),
            in_thinking: create_rw_signal(false),
        }
    }

    /// Append a new entry of `kind`, sticking to the bottom (auto-follow).
    pub fn push(&self, kind: EntryKind) {
        self.entries.update(|e| e.push(ConversationEntry { kind }));
        self.stick_to_bottom();
    }

    // ── Typed push helpers (mirror the old Transcript API) ────────────

    pub fn push_user(&self, text: impl Into<String>) {
        self.push(EntryKind::User(text.into()));
    }

    pub fn push_assistant(&self, text: impl Into<String>) {
        self.push(EntryKind::Assistant(text.into()));
    }

    pub fn push_thinking(&self, text: impl Into<String>) {
        self.push(EntryKind::Thinking(text.into()));
    }

    pub fn push_tool(&self, text: impl Into<String>) {
        self.push(EntryKind::Tool(text.into()));
    }

    pub fn push_tool_call(&self, name: &str, args: &serde_json::Value) {
        self.push(EntryKind::Tool(format_tool_call(name, args)));
    }

    pub fn push_error(&self, text: impl Into<String>) {
        self.push(EntryKind::Error(text.into()));
    }

    pub fn push_system(&self, text: impl Into<String>) {
        self.push(EntryKind::System(text.into()));
    }

    // ── Streaming support ─────────────────────────────────────────────

    /// Append `text` to the last assistant entry. Returns `true` if it was
    /// appended (the last entry was an assistant entry). Mirrors the old
    /// `Transcript::append_to_assistant`.
    ///
    /// `RwSignal::update` returns `()`, so we capture the result via a
    /// `Cell<Option<bool>>` shared into the closure.
    pub fn append_to_assistant(&self, text: &str) -> bool {
        use std::cell::Cell;
        let was_following_bottom = self.scroll_offset.get() == usize::MAX;
        let result = Cell::new(false);
        let result_ref = &result;
        self.entries.update(|e| {
            if let Some(last) = e.last_mut()
                && let EntryKind::Assistant(body) = &mut last.kind
            {
                body.push_str(text);
                result_ref.set(true);
            }
        });
        if result.get() && was_following_bottom {
            self.stick_to_bottom();
        }
        result.get()
    }

    /// Whether the last entry is an assistant entry.
    pub fn last_is_assistant(&self) -> bool {
        self.entries.with(|e| {
            e.last()
                .is_some_and(|m| matches!(m.kind, EntryKind::Assistant(_)))
        })
    }

    // ── Transcript-wide mutations ─────────────────────────────────────

    /// Clear the whole transcript.
    pub fn clear(&self) {
        self.entries.set(Vec::new());
        self.scroll_offset.set(usize::MAX);
    }

    // ── Scroll ────────────────────────────────────────────────────────

    /// Stick the transcript to the bottom (auto-follow new output).
    pub fn stick_to_bottom(&self) {
        self.scroll_offset.set(usize::MAX);
    }

    /// Scroll the viewport up by `lines` (towards older messages).
    pub fn scroll_up(&self, lines: usize) {
        self.scroll_offset.update(|o| {
            *o = if *o == usize::MAX {
                lines
            } else {
                o.saturating_add(lines)
            };
        });
    }

    /// Scroll the viewport down by `lines` (towards newer messages).
    pub fn scroll_down(&self, lines: usize) {
        self.scroll_offset.update(|o| {
            if *o == usize::MAX {
                return;
            }
            *o = o.saturating_sub(lines);
            if *o == 0 {
                *o = usize::MAX;
            }
        });
    }
}

/// A type-erased handle the slash-command module can push into, decoupled from
/// the reactive `UiState`. Implemented for `Rc<UiState>` so the command bodies
/// stay unchanged from the old `&mut Transcript` API.
///
/// Takes owned `String` (not `impl Into<String>`) so the trait stays
/// object-safe (`dyn TranscriptHandle`), since the slash-command dispatch is
/// generic over the handle.
pub trait TranscriptHandle {
    fn push_system(&mut self, text: String);
    fn push_error(&mut self, text: String);
    fn clear(&mut self);
}

impl TranscriptHandle for Rc<UiState> {
    fn push_system(&mut self, text: String) {
        UiState::push_system(self, text);
    }
    fn push_error(&mut self, text: String) {
        UiState::push_error(self, text);
    }
    fn clear(&mut self) {
        UiState::clear(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_stream_keeps_manual_scroll_position() {
        let (offset, owner) = create_root(|| {
            let state = UiState::new(TextAreaState::default());
            state.push_assistant("first");
            state.scroll_up(4);

            assert!(state.append_to_assistant(" second"));

            state.scroll_offset.get()
        });

        assert_eq!(offset, 4);
        owner.dispose();
    }

    #[test]
    fn assistant_stream_follows_when_stuck_to_bottom() {
        let (offset, owner) = create_root(|| {
            let state = UiState::new(TextAreaState::default());
            state.push_assistant("first");

            assert!(state.append_to_assistant(" second"));

            state.scroll_offset.get()
        });

        assert_eq!(offset, usize::MAX);
        owner.dispose();
    }
}

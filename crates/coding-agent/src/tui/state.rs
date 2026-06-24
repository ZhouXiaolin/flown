//! Reactive state model for the iodilos-based TUI.
//!
//! All UI state lives in iodilos `Signal`s held by [`UiState`], which is
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

use super::editor::{EditorState, SlashPopup};
use super::tool_format::format_tool_call;

// ── Conversation model ────────────────────────────────────────────────

/// One row in the transcript. The `id` is a per-session monotonically
/// increasing identifier used as the stable key for the iodilos
/// `StreamingList` keyed engine — it lets a surviving entry keep its mapped
/// view (and its per-item scope) across `entries.set(..)` mutations, so a
/// streaming assistant body updates in place via the body `Signal` instead of
/// re-running the entry's view closure.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationEntry {
    pub id: u64,
    pub kind: EntryKind,
}

/// The kind + body of a transcript entry. Mirrors the old `MessageKind` enum.
///
/// `Assistant` and `Thinking` carry their body as a `Signal<String>` (not a
/// plain `String`) — these are the two streaming variants, and storing them
/// as signals lets `append_to_assistant` / `append_to_thinking` mutate the
/// body **without** touching the parent `entries: Signal<Vec<_>>`. That keeps
/// `StreamingList`'s keyed diff a no-op (the surviving entry's view closure
/// is not re-invoked), and the markdown re-render is driven by the body
/// signal's own reactive region inside the entry view. Every other variant is
/// finalized at push time, so a plain `String` suffices.
#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    User(String),
    Assistant(Signal<String>),
    Thinking(Signal<String>),
    Tool { name: String, text: String },
    /// A finalized tool result (the output a tool produced after running).
    /// `tool` names the source tool so the renderer can apply per-tool styling
    /// (e.g. bash output is shown indented, italic, light, and capped to a few
    /// lines). `output` is the raw result text.
    ToolResult { tool: String, output: String },
    Error(String),
    /// A soft failure: the tool ran but reported a non-fatal problem (e.g. a
    /// bash command that exited non-zero yet still produced output). Rendered
    /// as a warning rather than a hard error so transient non-zero exits don't
    /// drown the transcript in red.
    Warning(String),
    System(String),
}

impl EntryKind {
    /// A short label for the kind (used for debugging / accessibility).
    pub fn label(&self) -> &'static str {
        match self {
            EntryKind::User(_) => "user",
            EntryKind::Assistant(_) => "assistant",
            EntryKind::Thinking(_) => "thinking",
            EntryKind::Tool { .. } => "tool",
            EntryKind::ToolResult { .. } => "tool_result",
            EntryKind::Error(_) => "error",
            EntryKind::Warning(_) => "warning",
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

// ── Terminal-size signal ─────────────────────────────────────────────

/// A reactive view of the terminal size in `(cols, rows)`. Provided once at
/// mount via iodilos context and updated by App's `terminal_resize` handler.
///
/// Components read `cols.get()` / `rows.get()` inside an effect so they
/// re-render when the terminal is resized. Without this, components that call
/// `crossterm::terminal::size()` once at build time render at a stale width
/// after a resize — the thinking block's border, for instance, would wrap as
/// if the terminal were still the old width.
#[derive(Clone, Copy)]
pub struct TerminalSize {
    pub cols: Signal<u16>,
    pub rows: Signal<u16>,
}

impl TerminalSize {
    /// Build a fresh signal pair seeded from the current terminal size (or
    /// 80×24 as a fallback when there is no tty, e.g. tests).
    pub fn new() -> Self {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        Self {
            cols: create_signal(cols),
            rows: create_signal(rows),
        }
    }

    /// Update both signals from a resize event.
    pub fn set(&self, cols: u16, rows: u16) {
        self.cols.set(cols);
        self.rows.set(rows);
    }
}

impl std::fmt::Debug for TerminalSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalSize")
            .field("cols", &self.cols.get())
            .field("rows", &self.rows.get())
            .finish()
    }
}

// ── UiState ───────────────────────────────────────────────────────────

/// The shared reactive state, wrapped in `Rc` so it can be provided via iodilos
/// context and cheaply cloned into every component.
///
/// Every field is a `Copy` signal handle (`Signal`), so handlers and effects
/// capture them by copy without borrow-checker friction — the iodilos idiom.
pub struct UiState {
    /// The full conversation, oldest first. Drives the transcript `<For>`.
    pub entries: Signal<Vec<ConversationEntry>>,
    /// Monotonic id allocator. Stamped onto each `ConversationEntry` at push
    /// time and used as the `StreamingList` key, so entry views are reused by
    /// id across `entries.set(..)` instead of being rebuilt by index.
    pub next_entry_id: Signal<u64>,
    /// `true` while an agent prompt is streaming.
    pub busy: Signal<bool>,
    /// Status-line snapshot.
    pub status: Signal<StatusInfo>,
    /// The editor buffer (text, cursor, slash popup).
    pub input: Signal<EditorState>,
    /// Slash-completion popup state for the editor.
    pub slash_popup: Signal<Option<SlashPopup>>,
    /// Scroll offset in lines from the bottom; `usize::MAX` means "stuck to
    /// bottom" (auto-follow). The transcript component resolves this against
    /// the viewport height to pick the visible window.
    pub scroll_offset: Signal<usize>,
    /// Accumulator for the active `ThinkingDelta` stream. The rendered
    /// transcript entry is updated as deltas arrive.
    pub thinking_acc: Signal<String>,
    /// `true` between `ThinkingStart` and `ThinkingEnd`.
    pub in_thinking: Signal<bool>,
}

impl UiState {
    /// Build a fresh state with empty signals. `editor` is the initial editor
    /// state (usually `EditorState::default()`).
    pub fn new(editor: EditorState) -> Self {
        Self {
            entries: create_signal(Vec::new()),
            next_entry_id: create_signal(0),
            busy: create_signal(false),
            status: create_signal(StatusInfo::default()),
            input: create_signal(editor),
            slash_popup: create_signal(None),
            scroll_offset: create_signal(usize::MAX),
            thinking_acc: create_signal(String::new()),
            in_thinking: create_signal(false),
        }
    }

    /// Allocate the next entry id and bump the counter.
    fn alloc_id(&self) -> u64 {
        let id = self.next_entry_id.get();
        self.next_entry_id.set(id + 1);
        id
    }

    /// Append a new entry of `kind`, sticking to the bottom (auto-follow).
    pub fn push(&self, kind: EntryKind) {
        let id = self.alloc_id();
        self.entries
            .update(|e| e.push(ConversationEntry { id, kind }));
        self.stick_to_bottom();
    }

    // ── Typed push helpers (mirror the old Transcript API) ────────────

    pub fn push_user(&self, text: impl Into<String>) {
        self.push(EntryKind::User(text.into()));
    }

    pub fn push_assistant(&self, text: impl Into<String>) {
        self.push(EntryKind::Assistant(create_signal(text.into())));
    }

    pub fn push_thinking(&self, text: impl Into<String>) {
        self.push(EntryKind::Thinking(create_signal(text.into())));
    }

    pub fn push_tool(&self, name: impl Into<String>, text: impl Into<String>) {
        self.push(EntryKind::Tool {
            name: name.into(),
            text: text.into(),
        });
    }

    pub fn push_tool_call(&self, name: &str, args: &serde_json::Value) {
        let text = format_tool_call(name, args);
        self.push(EntryKind::Tool {
            name: name.to_string(),
            text,
        });
    }

    /// Push a finalized tool result. `tool` selects per-tool rendering (bash
    /// output, for instance, is shown indented + italic + capped).
    pub fn push_tool_result(&self, tool: &str, output: impl Into<String>) {
        self.push(EntryKind::ToolResult {
            tool: tool.to_string(),
            output: output.into(),
        });
    }

    pub fn push_error(&self, text: impl Into<String>) {
        self.push(EntryKind::Error(text.into()));
    }

    pub fn push_warning(&self, text: impl Into<String>) {
        self.push(EntryKind::Warning(text.into()));
    }

    pub fn push_system(&self, text: impl Into<String>) {
        self.push(EntryKind::System(text.into()));
    }

    // ── Streaming support ─────────────────────────────────────────────

    /// Append `text` to the last assistant entry. Returns `true` if it was
    /// appended (the last entry was an assistant entry). Mirrors the old
    /// `Transcript::append_to_assistant`.
    ///
    /// Streaming append targets the entry's body **`Signal<String>`**, not the
    /// parent `entries: Signal<Vec<_>>`. That lets the `StreamingList` keyed
    /// engine skip the surviving entry's view closure entirely — the per-item
    /// reactive region reading `body.get_clone()` re-renders the markdown in
    /// place, while every other entry's view stays untouched.
    pub fn append_to_assistant(&self, text: &str) -> bool {
        let was_following_bottom = self.scroll_offset.get() == usize::MAX;
        let body_signal = self.entries.with(|e| match e.last() {
            Some(entry) => match &entry.kind {
                EntryKind::Assistant(sig) => Some(*sig),
                _ => None,
            },
            None => None,
        });
        let Some(body) = body_signal else {
            return false;
        };
        body.update(|s| s.push_str(text));
        if was_following_bottom {
            self.stick_to_bottom();
        }
        true
    }

    /// Append `text` to the last thinking entry. Returns `true` if it was
    /// appended (the last entry was a thinking entry).
    pub fn append_to_thinking(&self, text: &str) -> bool {
        let was_following_bottom = self.scroll_offset.get() == usize::MAX;
        let body_signal = self.entries.with(|e| match e.last() {
            Some(entry) => match &entry.kind {
                EntryKind::Thinking(sig) => Some(*sig),
                _ => None,
            },
            None => None,
        });
        let Some(body) = body_signal else {
            return false;
        };
        body.update(|s| s.push_str(text));
        if was_following_bottom {
            self.stick_to_bottom();
        }
        true
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
        let offset = std::cell::Cell::new(0usize);
        let owner = create_root(|| {
            let state = UiState::new(EditorState::default());
            state.push_assistant("first");
            state.scroll_up(4);

            assert!(state.append_to_assistant(" second"));

            offset.set(state.scroll_offset.get());
        });

        assert_eq!(offset.get(), 4);
        owner.dispose();
    }

    #[test]
    fn assistant_stream_follows_when_stuck_to_bottom() {
        let offset = std::cell::Cell::new(0usize);
        let owner = create_root(|| {
            let state = UiState::new(EditorState::default());
            state.push_assistant("first");

            assert!(state.append_to_assistant(" second"));

            offset.set(state.scroll_offset.get());
        });

        assert_eq!(offset.get(), usize::MAX);
        owner.dispose();
    }

    #[test]
    fn thinking_stream_appends_to_last_thinking_entry() {
        let text = std::cell::RefCell::new(String::new());
        let owner = create_root(|| {
            let state = UiState::new(EditorState::default());
            state.push_thinking("alpha");

            assert!(state.append_to_thinking(" beta"));

            state.entries.with(|entries| {
                let EntryKind::Thinking(body) = &entries[0].kind else {
                    panic!("expected thinking entry");
                };
                text.replace(body.get_clone());
            });
        });

        assert_eq!(text.into_inner(), "alpha beta");
        owner.dispose();
    }
}

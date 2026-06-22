//! Terminal UI, built on iodilos (a SolidJS-inspired reactive TUI framework).
//!
//! Architecture:
//! - [`state`] — reactive state model (`UiState`, signals, push helpers).
//! - [`runtime`] — the `run_tui` entry point + cross-runtime flume bridge
//!   (tokio agent driver ↔ iodilos event pump).
//! - [`editor`] — slash-completion glue over iodilos-prompt.
//! - [`tool_format`] — pure tool-call formatting helpers.
//! - [`components`] — the iodilos components (App, Transcript, MessageBlock,
//!   StatusLine, HintBar; editor rendering lands in Phase 3).
//! - Markdown rendering is provided by `iodilos-md`.

pub mod components;
pub mod conversation;
pub mod editor;
pub mod overlay_stack;
pub mod runtime;
mod slash_commands;
pub mod state;
pub mod tool_format;

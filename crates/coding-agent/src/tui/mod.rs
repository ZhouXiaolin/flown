//! Terminal UI, built on iodilos (a SolidJS-inspired reactive TUI framework).
//!
//! Architecture:
//! - [`state`] — reactive state model (`UiState`, signals, push helpers).
//! - [`runtime`] — the `run_tui` entry point + cross-runtime flume bridge
//!   (tokio agent driver ↔ iodilos event pump).
//! - [`editor`] — slash-completion glue over iodilos `TextAreaState`.
//! - [`tool_format`] — pure tool-call formatting helpers.
//! - [`components`] — the iodilos components (App, Transcript, MessageBlock,
//!   StatusLine, HintBar; editor rendering lands in Phase 3).
//! - [`markdown`] / [`theme`] — preserved rendering libraries
//!   (`parse_markdown_with_width`, color themes).

pub mod components;
pub mod editor;
pub mod markdown;
pub mod runtime;
pub mod state;
pub mod theme;
pub mod tool_format;
mod slash_commands;
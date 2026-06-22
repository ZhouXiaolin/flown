//! iodilos components for the coding-agent TUI.
//!
//! Components are plain Rust functions returning iodilos `View`s. They read
//! shared state from the [`UiState`](crate::tui::state::UiState) provided via
//! iodilos context.

pub mod app;
pub mod editor;
pub mod message_block;
pub mod model_overlay;
pub mod overlay_conversation;
pub mod transcript;

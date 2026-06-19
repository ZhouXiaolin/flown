//! iodilos components for the coding-agent TUI.
//!
//! Each component is a plain Rust function turned into a `#[component]` that
//! returns a `Node` (built with `view!` or by hand). They read shared state
//! from the [`UiState`](crate::tui::state::UiState) provided via iodilos context.

pub mod app;
pub mod editor;
pub mod hint_bar;
pub mod message_block;
pub mod model_overlay;
pub mod overlay_conversation;
pub mod status_line;
pub mod transcript;

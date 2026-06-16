//! [`BtwExtension`] — registers the `/btw` command.
//!
//! `/btw` is a **control** command: its handler needs the iodilos-side
//! [`ControlRuntime`](super::types::ControlRuntime) to drive the conversation
//! stack (fork the main session, push a layer, optionally submit a prompt).
//! That capability holds `Rc`-based state and cannot be `Send`, so — unlike
//! `/mcp`'s effect handler — the real `/btw` logic is bound at mount
//! (`runtime.rs`) into [`CommandSide`](super::runner::CommandSide), not during
//! `register` (which runs on tokio).
//!
//! What `register` does here is purely metadata: it tells the layer the
//! command exists (for autocomplete + `/help`) and marks it
//! `needs_control`. The placeholder handler surfaces a clear error if the
//! control capability is somehow absent at dispatch time (session-only mode).
//!
//! Argument parsing (`/btw` vs `/btw <message>`) is a pure, unit-tested
//! function so it stays testable without a TUI.

use super::types::{CommandEffect, CommandMeta, Extension, ExtensionApi};

/// The `/btw` extension. Stateless beyond the (unused here) registration — all
/// real work happens in the iodilos-side control handler bound at mount.
pub struct BtwExtension;

impl BtwExtension {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BtwExtension {
    fn default() -> Self {
        Self::new()
    }
}

impl Extension for BtwExtension {
    fn name(&self) -> &'static str {
        "btw"
    }

    fn register(&self, api: &mut ExtensionApi) {
        api.register_control_command(
            "/btw",
            CommandMeta::simple(
                "Open a temporary side conversation (forks current history; Ctrl+C to exit)",
            ),
            // Placeholder effect handler: only reached if dispatch falls
            // through (no control capability bound). Surfaces a clear message
            // rather than silently no-op'ing.
            std::sync::Arc::new(|_args: &str| {
                CommandEffect::NotifyError(
                    "/btw is unavailable in this mode (no conversation runtime).".to_string(),
                )
            }),
        );
    }
}

/// Parse the text after `/btw`. Returns the prompt to submit, or `None` when
/// the user invoked a bare `/btw` (enter an empty btw transcript, wait for
/// input). Whitespace-only args are treated as empty.
///
/// Examples:
/// - `parse_btw_args("")` → `None`
/// - `parse_btw_args("   ")` → `None`
/// - `parse_btw_args("how do I print in rust")` → `Some("how do I print in rust")`
pub fn parse_btw_args(args: &str) -> Option<String> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_btw_is_none() {
        assert_eq!(parse_btw_args(""), None);
    }

    #[test]
    fn whitespace_only_btw_is_none() {
        assert_eq!(parse_btw_args("   "), None);
        assert_eq!(parse_btw_args("\t\n"), None);
    }

    #[test]
    fn btw_with_message_is_some() {
        assert_eq!(
            parse_btw_args("how do I print in rust"),
            Some("how do I print in rust".to_string())
        );
    }

    #[test]
    fn btw_trims_surrounding_whitespace() {
        assert_eq!(parse_btw_args("  hello world  "), Some("hello world".to_string()));
    }
}

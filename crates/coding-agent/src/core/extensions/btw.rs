//! [`BtwExtension`] — registers the `/btw` command.
//!
//! `/btw` drives the conversation stack through the async `ExtensionContext`.
//! The extension owns the `/btw` policy and calls only the generic overlap API.
//!
//! Argument parsing (`/btw` vs `/btw <message>`) is a pure, unit-tested
//! function so it stays testable without a TUI.

use super::types::{CommandMeta, Extension, ExtensionApi, OverlapOptions, SlashCommandScope};

/// The `/btw` extension. Stateless beyond registration; all btw policy is in
/// [`open_btw_overlap`].
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
        api.register_command(
            "/btw",
            CommandMeta::simple(
                "Open a temporary side conversation (forks current history; Ctrl+C to exit)",
            ),
            std::sync::Arc::new(|invocation, ctx| {
                Box::pin(async move { open_btw_overlap(&invocation.args, ctx).await })
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

/// Runtime handler for `/btw`.
///
/// All btw-specific policy lives here. The TUI runtime only sees a generic
/// overlap request: open an agent-backed overlay, disable slash commands inside
/// it, display a badge, and keep it single-instance.
pub async fn open_btw_overlap(
    args: &str,
    ctx: super::types::ExtensionContext,
) -> anyhow::Result<()> {
    let mut options = OverlapOptions::new("btw");
    options.badge = Some("BTW".to_string());
    options.single_instance_key = Some("btw".to_string());
    options.dismissible = true;
    options.slash_commands = SlashCommandScope::Disabled;
    options.initial_prompt = parse_btw_args(args);
    ctx.conversation.open_overlap(options).await
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
        assert_eq!(
            parse_btw_args("  hello world  "),
            Some("hello world".to_string())
        );
    }
}

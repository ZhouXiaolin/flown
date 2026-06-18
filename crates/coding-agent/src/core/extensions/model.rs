//! [`ModelExtension`] — registers the `/model` command.
//!
//! `/model` opens the model + thinking-intensity picker overlay. The extension
//! is stateless beyond registration; all overlay behavior lives in the TUI
//! runtime (RuntimeControl::handle_open_model_overlay).

use super::types::{CommandMeta, Extension, ExtensionApi};

/// The `/model` extension. Stateless beyond registration.
pub struct ModelExtension;

impl ModelExtension {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ModelExtension {
    fn default() -> Self {
        Self::new()
    }
}

impl Extension for ModelExtension {
    fn name(&self) -> &'static str {
        "model"
    }

    fn register(&self, api: &mut ExtensionApi) {
        api.register_command(
            "/model",
            CommandMeta::simple("Select a model or thinking level (Esc to dismiss)"),
            std::sync::Arc::new(|_invocation, ctx| {
                Box::pin(async move { ctx.conversation.open_model_overlay().await })
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_extension_has_name() {
        assert_eq!(ModelExtension::new().name(), "model");
    }
}

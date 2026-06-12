pub mod api_registry;
pub mod models;
pub mod providers;
pub mod types;
pub mod validation;

// Re-export main types
pub use api_registry::{
    AiError, ApiProvider, AssistantMessageEventStream, Result, clear_api_providers, complete,
    complete_simple, get_api_provider, get_api_providers, stream, stream_simple, try_complete,
    try_complete_simple, try_stream, try_stream_simple,
};
pub use models::{
    calculate_cost, clamp_thinking_level, get_model, get_models, get_providers,
    get_supported_thinking_levels, models_are_equal, transform_messages,
};
pub use providers::{AnthropicProvider, OpenAiCompletionsProvider};
pub use types::*;
pub use validation::validate_tool_arguments;

/// Initialize the AI module with default providers and models
pub fn init() {
    providers::anthropic::register_anthropic_provider();
    providers::openai_completions::register_openai_completions_provider();
    models::register_deepseek_models();
}

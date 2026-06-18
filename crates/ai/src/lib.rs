mod api_registry;
mod diagnostics;
mod env_api_keys;
mod error;
mod image_models;
mod images;
mod images_api_registry;
mod images_types;
mod json_parse;
mod models;
mod overflow;
mod providers;
mod session_resources;
mod types;
mod validation;

// Re-export main types
pub use api_registry::{
    ApiProvider, AssistantMessageEventStream, RawEventStream, clear_api_providers, complete,
    complete_simple, create_assistant_message_event_stream, get_api_provider, get_api_providers,
    register_api_provider, register_built_in_api_providers, reset_api_providers, stream,
    stream_simple, unregister_api_providers,
};
pub use diagnostics::{
    AssistantMessageDiagnostic, DiagnosticErrorInfo, append_diagnostic, create_diagnostic,
    extract_diagnostic_error, format_thrown_value,
};
pub use env_api_keys::{find_env_keys, get_env_api_key};
pub use error::{AiError, Result};
pub use image_models::{get_image_model, get_image_models, get_image_providers};
pub use images::generate_images;
pub use images_api_registry::{
    ImagesApiProvider, clear_images_api_providers, get_images_api_provider,
    get_images_api_providers, register_built_in_images_api_providers, register_images_api_provider,
    reset_images_api_providers, unregister_images_api_providers,
};
pub use images_types::*;
pub use json_parse::{parse_json_with_repair, parse_streaming_json, repair_json};
pub use models::{
    calculate_cost, clamp_thinking_level, get_model, get_models, get_providers,
    get_supported_thinking_levels, models_are_equal,
};
pub use overflow::{get_overflow_patterns, is_context_overflow};
pub use providers::{
    AnthropicEffort, AnthropicOptions, AnthropicThinkingDisplay, AzureOpenAIResponsesOptions,
    BedrockOptions, BedrockThinkingDisplay, FauxAssistantMessageContent,
    FauxAssistantMessageOptions, FauxContentBlock, FauxModelDefinition, FauxProviderRegistration,
    FauxResponseStep, FauxTokenSize, GoogleOptions, GoogleThinkingLevel, GoogleVertexOptions,
    MistralOptions, OAuthAuthInfo, OAuthCredentials, OAuthDeviceCodeInfo, OAuthLoginCallbacks,
    OAuthPrompt, OAuthProvider, OAuthProviderId, OAuthProviderInfo, OAuthProviderInterface,
    OAuthSelectOption, OAuthSelectPrompt, OpenAICodexResponsesOptions,
    OpenAICodexWebSocketDebugStats, OpenAICompletionsOptions, OpenAIResponsesOptions,
    faux_assistant_message, faux_text, faux_thinking, faux_tool_call, register_faux_provider,
    stream_anthropic_public, stream_openai_completions_public, stream_openai_responses_public,
    stream_simple_anthropic, stream_simple_openai_completions, stream_simple_openai_responses,
};
pub use session_resources::{
    SessionResourceCleanup, cleanup_session_resources, register_session_resource_cleanup,
};
pub use types::*;
pub use validation::{validate_tool_arguments, validate_tool_call};

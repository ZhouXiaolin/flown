mod alignment_surface;
pub mod anthropic;
pub mod faux;
pub mod images;
mod common;
pub(crate) mod json;
pub mod openai_completions;
pub mod openai_responses;

pub use alignment_surface::{
    AzureOpenAIResponsesOptions, BedrockOptions, BedrockThinkingDisplay, GoogleOptions,
    GoogleThinkingLevel, GoogleVertexOptions, MistralOptions,
    OAuthAuthInfo, OAuthCredentials, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt,
    OAuthProvider, OAuthProviderId, OAuthProviderInfo, OAuthProviderInterface,
    OAuthSelectOption, OAuthSelectPrompt, OpenAICodexResponsesOptions,
    OpenAICodexWebSocketDebugStats, OpenAIResponsesOptions,
};
pub use anthropic::{
    AnthropicEffort, AnthropicOptions, AnthropicThinkingDisplay,
    stream_anthropic_public, stream_simple_anthropic,
};
pub use faux::*;
pub use openai_completions::{
    OpenAICompletionsOptions, stream_openai_completions_public, stream_simple_openai_completions,
};
pub use openai_responses::{
    stream_openai_responses_public, stream_simple_openai_responses,
};

pub fn register_built_in_api_providers() {
    anthropic::register_anthropic_provider();
    openai_completions::register_openai_completions_provider();
    openai_responses::register_openai_responses_provider();
}

pub fn register_built_in_images_api_providers() {
    images::register_built_in_images_api_providers();
}

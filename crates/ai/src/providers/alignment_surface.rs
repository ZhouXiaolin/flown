use crate::types::{StreamOptions, ThinkingBudgets};

/// Mirrors pi-ai's `BedrockThinkingDisplay` public union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BedrockThinkingDisplay {
    Summarized,
    Omitted,
}

/// Mirrors pi-ai's `GoogleThinkingLevel` public union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum GoogleThinkingLevel {
    #[serde(rename = "THINKING_LEVEL_UNSPECIFIED")]
    ThinkingLevelUnspecified,
    #[serde(rename = "MINIMAL")]
    Minimal,
    #[serde(rename = "LOW")]
    Low,
    #[serde(rename = "MEDIUM")]
    Medium,
    #[serde(rename = "HIGH")]
    High,
}

/// Public Bedrock provider options surface.
#[derive(Debug, Clone, Default)]
pub struct BedrockOptions {
    pub base: StreamOptions,
    pub region: Option<String>,
    pub profile: Option<String>,
    pub tool_choice: Option<serde_json::Value>,
    pub reasoning: Option<crate::types::ThinkingLevel>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub interleaved_thinking: Option<bool>,
    pub thinking_display: Option<BedrockThinkingDisplay>,
    pub request_metadata: Option<std::collections::HashMap<String, String>>,
    pub bearer_token: Option<String>,
}

/// Public Azure OpenAI Responses provider options surface.
#[derive(Debug, Clone, Default)]
pub struct AzureOpenAIResponsesOptions {
    pub base: StreamOptions,
    pub reasoning_effort: Option<String>,
    pub reasoning_summary: Option<String>,
    pub azure_api_version: Option<String>,
    pub azure_resource_name: Option<String>,
    pub azure_base_url: Option<String>,
    pub azure_deployment_name: Option<String>,
}

/// Public Google provider options surface.
#[derive(Debug, Clone, Default)]
pub struct GoogleOptions {
    pub base: StreamOptions,
    pub tool_choice: Option<String>,
    pub thinking: Option<GoogleThinkingConfig>,
}

/// Public Google Vertex provider options surface.
#[derive(Debug, Clone, Default)]
pub struct GoogleVertexOptions {
    pub base: StreamOptions,
    pub tool_choice: Option<String>,
    pub thinking: Option<GoogleThinkingConfig>,
    pub project: Option<String>,
    pub location: Option<String>,
}

/// Shared Google thinking configuration surface.
#[derive(Debug, Clone, Default)]
pub struct GoogleThinkingConfig {
    pub enabled: bool,
    pub budget_tokens: Option<i32>,
    pub level: Option<GoogleThinkingLevel>,
}

/// Public Mistral provider options surface.
#[derive(Debug, Clone, Default)]
pub struct MistralOptions {
    pub base: StreamOptions,
    pub tool_choice: Option<serde_json::Value>,
    pub prompt_mode: Option<String>,
    pub reasoning_effort: Option<String>,
}

/// Public OpenAI Responses provider options surface.
#[derive(Debug, Clone, Default)]
pub struct OpenAIResponsesOptions {
    pub base: StreamOptions,
    pub reasoning_effort: Option<String>,
    pub reasoning_summary: Option<String>,
    pub service_tier: Option<String>,
}

/// Public OpenAI Codex Responses provider options surface.
#[derive(Debug, Clone, Default)]
pub struct OpenAICodexResponsesOptions {
    pub base: StreamOptions,
    pub reasoning_effort: Option<String>,
    pub reasoning_summary: Option<String>,
    pub service_tier: Option<String>,
    pub text_verbosity: Option<String>,
}

/// Public OpenAI Codex WebSocket debug stats surface.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OpenAICodexWebSocketDebugStats {
    pub requests: u64,
    pub connections_created: u64,
    pub connections_reused: u64,
    pub cached_context_requests: u64,
    pub store_true_requests: u64,
    pub full_context_requests: u64,
    pub delta_requests: u64,
    pub last_input_items: u64,
    pub last_delta_input_items: Option<u64>,
    pub last_previous_response_id: Option<String>,
    pub websocket_failures: u64,
    pub sse_fallbacks: u64,
    pub websocket_fallback_active: Option<bool>,
    pub last_web_socket_error: Option<String>,
}

pub type OAuthProviderId = String;

/// @deprecated Use `OAuthProviderId` instead.
pub type OAuthProvider = OAuthProviderId;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OAuthCredentials {
    pub refresh: String,
    pub access: String,
    pub expires: i64,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OAuthPrompt {
    pub message: String,
    pub placeholder: Option<String>,
    pub allow_empty: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OAuthAuthInfo {
    pub url: String,
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OAuthDeviceCodeInfo {
    pub user_code: String,
    pub verification_uri: String,
    pub interval_seconds: Option<u64>,
    pub expires_in_seconds: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OAuthSelectOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OAuthSelectPrompt {
    pub message: String,
    pub options: Vec<OAuthSelectOption>,
}

/// Type-only public surface mirroring pi-ai's OAuth callbacks contract.
#[derive(Clone, Default)]
pub struct OAuthLoginCallbacks {
    pub on_auth:
        Option<std::sync::Arc<dyn Fn(OAuthAuthInfo) + Send + Sync>>,
    pub on_device_code:
        Option<std::sync::Arc<dyn Fn(OAuthDeviceCodeInfo) + Send + Sync>>,
    pub on_prompt: Option<
        std::sync::Arc<
            dyn Fn(OAuthPrompt) -> futures::future::BoxFuture<'static, String> + Send + Sync,
        >,
    >,
    pub on_progress: Option<std::sync::Arc<dyn Fn(String) + Send + Sync>>,
    pub on_manual_code_input: Option<
        std::sync::Arc<dyn Fn() -> futures::future::BoxFuture<'static, String> + Send + Sync>,
    >,
    pub on_select: Option<
        std::sync::Arc<
            dyn Fn(OAuthSelectPrompt) -> futures::future::BoxFuture<'static, Option<String>>
                + Send
                + Sync,
        >,
    >,
    pub signal: Option<crate::types::AbortSignal>,
}

impl std::fmt::Debug for OAuthLoginCallbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthLoginCallbacks")
            .field("on_auth", &self.on_auth.as_ref().map(|_| "<fn>"))
            .field("on_device_code", &self.on_device_code.as_ref().map(|_| "<fn>"))
            .field("on_prompt", &self.on_prompt.as_ref().map(|_| "<fn>"))
            .field("on_progress", &self.on_progress.as_ref().map(|_| "<fn>"))
            .field(
                "on_manual_code_input",
                &self.on_manual_code_input.as_ref().map(|_| "<fn>"),
            )
            .field("on_select", &self.on_select.as_ref().map(|_| "<fn>"))
            .field("signal", &self.signal)
            .finish()
    }
}

pub trait OAuthProviderInterface: Send + Sync {
    fn id(&self) -> &OAuthProviderId;
    fn name(&self) -> &str;
}

/// @deprecated Use `OAuthProviderInterface` instead.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OAuthProviderInfo {
    pub id: OAuthProviderId,
    pub name: String,
    pub available: bool,
}

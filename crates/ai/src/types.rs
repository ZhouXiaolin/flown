use chrono::{DateTime, Utc};
use futures::future::poll_fn;
use futures::task::AtomicWaker;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Runtime-agnostic abort signal for cancelling long-running operations.
#[derive(Debug, Clone, Default)]
pub struct AbortSignal {
    inner: Arc<AbortSignalInner>,
}

#[derive(Debug, Default)]
struct AbortSignalInner {
    cancelled: AtomicBool,
    waker: AtomicWaker,
}

impl AbortSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        self.inner.waker.wake();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    pub async fn cancelled(&self) {
        poll_fn(|cx| {
            if self.is_cancelled() {
                return std::task::Poll::Ready(());
            }

            self.inner.waker.register(cx.waker());

            if self.is_cancelled() {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        })
        .await
    }
}

/// Known API providers
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KnownApi {
    #[serde(rename = "openai-completions")]
    OpenAiCompletions,
    #[serde(rename = "mistral-conversations")]
    MistralConversations,
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    #[serde(rename = "azure-openai-responses")]
    AzureOpenAiResponses,
    #[serde(rename = "openai-codex-responses")]
    OpenAiCodexResponses,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "bedrock-converse-stream")]
    BedrockConverseStream,
    #[serde(rename = "google-generative-ai")]
    GoogleGenerativeAi,
    #[serde(rename = "google-vertex")]
    GoogleVertex,
}

/// API identifier - either known or custom
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Api {
    Known(KnownApi),
    Custom(String),
}

impl std::fmt::Display for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Api::Known(api) => write!(f, "{}", api),
            Api::Custom(api) => write!(f, "{}", api),
        }
    }
}

impl std::fmt::Display for KnownApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let id = match self {
            KnownApi::OpenAiCompletions => "openai-completions",
            KnownApi::MistralConversations => "mistral-conversations",
            KnownApi::OpenAiResponses => "openai-responses",
            KnownApi::AzureOpenAiResponses => "azure-openai-responses",
            KnownApi::OpenAiCodexResponses => "openai-codex-responses",
            KnownApi::AnthropicMessages => "anthropic-messages",
            KnownApi::BedrockConverseStream => "bedrock-converse-stream",
            KnownApi::GoogleGenerativeAi => "google-generative-ai",
            KnownApi::GoogleVertex => "google-vertex",
        };
        write!(f, "{}", id)
    }
}

/// Known providers
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KnownProvider {
    OpenAi,
    Anthropic,
    Deepseek,
    Google,
    #[serde(rename = "google-vertex")]
    GoogleVertex,
    Mistral,
    #[serde(rename = "azure-openai-responses")]
    AzureOpenAiResponses,
    #[serde(rename = "openai-codex")]
    OpenAiCodex,
    #[serde(rename = "github-copilot")]
    GithubCopilot,
    Xai,
    Groq,
    Cerebras,
    Openrouter,
    #[serde(rename = "vercel-ai-gateway")]
    VercelAiGateway,
    Zai,
    Minimax,
    #[serde(rename = "minimax-cn")]
    MinimaxCn,
    Moonshotai,
    #[serde(rename = "moonshotai-cn")]
    MoonshotaiCn,
    Huggingface,
    Fireworks,
    Together,
    Opencode,
    #[serde(rename = "opencode-go")]
    OpencodeGo,
    #[serde(rename = "kimi-coding")]
    KimiCoding,
    #[serde(rename = "cloudflare-workers-ai")]
    CloudflareWorkersAi,
    #[serde(rename = "cloudflare-ai-gateway")]
    CloudflareAiGateway,
    Xiaomi,
    #[serde(rename = "xiaomi-token-plan-cn")]
    XiaomiTokenPlanCn,
    #[serde(rename = "xiaomi-token-plan-ams")]
    XiaomiTokenPlanAms,
    #[serde(rename = "xiaomi-token-plan-sgp")]
    XiaomiTokenPlanSgp,
    #[serde(rename = "amazon-bedrock")]
    AmazonBedrock,
    Nvidia,
    #[serde(rename = "zai-coding-cn")]
    ZaiCodingCn,
    #[serde(rename = "ant-ling")]
    AntLing,
}

/// Provider identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Provider {
    Known(KnownProvider),
    Custom(String),
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Provider::Known(p) => write!(f, "{}", p),
            Provider::Custom(s) => write!(f, "{}", s),
        }
    }
}

impl std::fmt::Display for KnownProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let id = match self {
            KnownProvider::OpenAi => "openai",
            KnownProvider::Anthropic => "anthropic",
            KnownProvider::Deepseek => "deepseek",
            KnownProvider::Google => "google",
            KnownProvider::GoogleVertex => "google-vertex",
            KnownProvider::Mistral => "mistral",
            KnownProvider::AzureOpenAiResponses => "azure-openai-responses",
            KnownProvider::OpenAiCodex => "openai-codex",
            KnownProvider::GithubCopilot => "github-copilot",
            KnownProvider::Xai => "xai",
            KnownProvider::Groq => "groq",
            KnownProvider::Cerebras => "cerebras",
            KnownProvider::Openrouter => "openrouter",
            KnownProvider::VercelAiGateway => "vercel-ai-gateway",
            KnownProvider::Zai => "zai",
            KnownProvider::Minimax => "minimax",
            KnownProvider::MinimaxCn => "minimax-cn",
            KnownProvider::Moonshotai => "moonshotai",
            KnownProvider::MoonshotaiCn => "moonshotai-cn",
            KnownProvider::Huggingface => "huggingface",
            KnownProvider::Fireworks => "fireworks",
            KnownProvider::Together => "together",
            KnownProvider::Opencode => "opencode",
            KnownProvider::OpencodeGo => "opencode-go",
            KnownProvider::KimiCoding => "kimi-coding",
            KnownProvider::CloudflareWorkersAi => "cloudflare-workers-ai",
            KnownProvider::CloudflareAiGateway => "cloudflare-ai-gateway",
            KnownProvider::Xiaomi => "xiaomi",
            KnownProvider::XiaomiTokenPlanCn => "xiaomi-token-plan-cn",
            KnownProvider::XiaomiTokenPlanAms => "xiaomi-token-plan-ams",
            KnownProvider::XiaomiTokenPlanSgp => "xiaomi-token-plan-sgp",
            KnownProvider::AmazonBedrock => "amazon-bedrock",
            KnownProvider::Nvidia => "nvidia",
            KnownProvider::ZaiCodingCn => "zai-coding-cn",
            KnownProvider::AntLing => "ant-ling",
        };
        write!(f, "{}", id)
    }
}

/// Thinking/reasoning level
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
}

/// Token budgets for thinking levels
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThinkingBudgets {
    pub minimal: Option<u32>,
    pub low: Option<u32>,
    pub medium: Option<u32>,
    pub high: Option<u32>,
}

/// Cache retention preference
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheRetention {
    None,
    Short,
    Long,
}

/// Transport type
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Sse,
    WebSocket,
    #[serde(rename = "websocket-cached")]
    WebSocketCached,
    Auto,
}

/// Provider response info passed to onResponse callback
#[derive(Debug, Clone)]
pub struct ProviderResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
}

/// Callback for inspecting/replacing provider payloads before sending
pub type OnPayloadFn = std::sync::Arc<
    dyn Fn(
            serde_json::Value,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<serde_json::Value>> + Send>>
        + Send
        + Sync,
>;

/// Callback invoked after HTTP response is received
pub type OnResponseFn = std::sync::Arc<
    dyn Fn(ProviderResponse) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Stream options for API calls
pub struct StreamOptions {
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub signal: Option<AbortSignal>,
    pub api_key: Option<String>,
    pub transport: Option<Transport>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub timeout_ms: Option<u64>,
    /// WebSocket connect timeout in milliseconds for providers that support
    /// WebSocket transports. Covers the connection/open handshake only;
    /// stream idleness after connection uses `timeout_ms`.
    pub websocket_connect_timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    pub metadata: Option<HashMap<String, serde_json::Value>>,
    /// Provider-specific tool choice payload.
    ///
    /// pi-ai models this through provider option intersections. Rust does not
    /// have an ergonomic equivalent to `StreamOptions & Record<string, unknown>`,
    /// so this intentionally stays as open JSON while the rest of the common
    /// option surface remains typed.
    pub tool_choice: Option<serde_json::Value>,
    pub thinking_enabled: Option<bool>,
    pub thinking_budget_tokens: Option<u32>,
    pub thinking_display: Option<String>,
    pub effort: Option<String>,
    pub interleaved_thinking: Option<bool>,
    pub on_payload: Option<OnPayloadFn>,
    pub on_response: Option<OnResponseFn>,
}

impl std::fmt::Debug for StreamOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamOptions")
            .field("temperature", &self.temperature)
            .field("max_tokens", &self.max_tokens)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("headers", &self.headers)
            .field("timeout_ms", &self.timeout_ms)
            .field(
                "websocket_connect_timeout_ms",
                &self.websocket_connect_timeout_ms,
            )
            .field("max_retries", &self.max_retries)
            .field("tool_choice", &self.tool_choice)
            .field("thinking_enabled", &self.thinking_enabled)
            .field("thinking_budget_tokens", &self.thinking_budget_tokens)
            .field("thinking_display", &self.thinking_display)
            .field("effort", &self.effort)
            .field("interleaved_thinking", &self.interleaved_thinking)
            .field("on_payload", &self.on_payload.as_ref().map(|_| "<fn>"))
            .field("on_response", &self.on_response.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl Clone for StreamOptions {
    fn clone(&self) -> Self {
        Self {
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            signal: self.signal.clone(),
            api_key: self.api_key.clone(),
            transport: self.transport.clone(),
            cache_retention: self.cache_retention.clone(),
            session_id: self.session_id.clone(),
            headers: self.headers.clone(),
            timeout_ms: self.timeout_ms,
            websocket_connect_timeout_ms: self.websocket_connect_timeout_ms,
            max_retries: self.max_retries,
            max_retry_delay_ms: self.max_retry_delay_ms,
            metadata: self.metadata.clone(),
            tool_choice: self.tool_choice.clone(),
            thinking_enabled: self.thinking_enabled,
            thinking_budget_tokens: self.thinking_budget_tokens,
            thinking_display: self.thinking_display.clone(),
            effort: self.effort.clone(),
            interleaved_thinking: self.interleaved_thinking,
            on_payload: self.on_payload.clone(),
            on_response: self.on_response.clone(),
        }
    }
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            temperature: None,
            max_tokens: None,
            signal: None,
            api_key: None,
            transport: None,
            cache_retention: None,
            session_id: None,
            headers: None,
            timeout_ms: None,
            websocket_connect_timeout_ms: None,
            max_retries: None,
            max_retry_delay_ms: None,
            metadata: None,
            tool_choice: None,
            thinking_enabled: None,
            thinking_budget_tokens: None,
            thinking_display: None,
            effort: None,
            interleaved_thinking: None,
            on_payload: None,
            on_response: None,
        }
    }
}

/// Simple stream options with reasoning level
#[derive(Debug, Clone, Default)]
pub struct SimpleStreamOptions {
    pub base: StreamOptions,
    pub reasoning: Option<ThinkingLevel>,
    pub thinking_budgets: Option<ThinkingBudgets>,
}

/// Text content block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
    #[serde(
        rename = "textSignature",
        alias = "text_signature",
        skip_serializing_if = "Option::is_none"
    )]
    pub text_signature: Option<String>,
}

/// Thinking content block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub thinking: String,
    #[serde(
        rename = "thinkingSignature",
        alias = "thinking_signature",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
}

/// Image content block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub data: String,
    #[serde(rename = "mimeType", alias = "mime_type")]
    pub mime_type: String,
}

/// Phase discriminator carried inside a [`TextSignatureV1`] envelope.
///
/// Some providers split response generation into a "commentary" phase
/// (intermediate reasoning) and a "final_answer" phase (the user-facing
/// reply); the phase lets downstream tooling disambiguate the two when
/// both share a single signature field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextSignaturePhase {
    Commentary,
    FinalAnswer,
}

/// Structured envelope stored inside [`TextContent::text_signature`] when
/// the provider returns more than a bare id. Mirrors pi-ai's
/// `TextSignatureV1` (types.ts:229-233).
///
/// The legacy form — a plain id string — is still supported: callers
/// should try JSON-parsing `text_signature` as `TextSignatureV1` and
/// fall back to treating the whole string as the id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSignatureV1 {
    pub v: u8,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<TextSignaturePhase>,
}

impl TextSignatureV1 {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            v: 1,
            id: id.into(),
            phase: None,
        }
    }

    pub fn with_phase(mut self, phase: TextSignaturePhase) -> Self {
        self.phase = Some(phase);
        self
    }
}

/// Try to parse `raw` as a `TextSignatureV1` JSON envelope. If that fails
/// (or `raw` is empty), returns a `TextSignatureV1` whose `id` is the
/// bare string — matching pi-ai's "legacy id string" fallback.
pub fn parse_text_signature(raw: &str) -> TextSignatureV1 {
    if raw.is_empty() {
        return TextSignatureV1::new("");
    }
    serde_json::from_str::<TextSignatureV1>(raw).unwrap_or_else(|_| TextSignatureV1::new(raw))
}

/// Tool call from assistant
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(rename = "type")]
    pub content_type: String,
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    #[serde(
        rename = "thoughtSignature",
        alias = "thought_signature",
        skip_serializing_if = "Option::is_none"
    )]
    pub thought_signature: Option<String>,
}

/// Token usage statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input: u32,
    pub output: u32,
    #[serde(rename = "cacheRead", alias = "cache_read")]
    pub cache_read: u32,
    #[serde(rename = "cacheWrite", alias = "cache_write")]
    pub cache_write: u32,
    /// Subset of `cache_write` written with 1h retention. Only Anthropic
    /// reports this split; other providers leave it `None`.
    #[serde(
        rename = "cacheWrite1h",
        alias = "cache_write_1h",
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_write_1h: Option<u32>,
    #[serde(rename = "totalTokens", alias = "total_tokens")]
    pub total_tokens: u32,
    pub cost: Cost,
}

/// Cost breakdown
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    #[serde(rename = "cacheRead", alias = "cache_read")]
    pub cache_read: f64,
    #[serde(rename = "cacheWrite", alias = "cache_write")]
    pub cache_write: f64,
    pub total: f64,
}

/// Stop reason for assistant response
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StopReason {
    Stop,
    Length,
    #[serde(rename = "toolUse", alias = "tooluse")]
    ToolUse,
    Error,
    Aborted,
}

/// User message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    pub role: String,
    pub content: MessageContent,
    pub timestamp: DateTime<Utc>,
}

/// Assistant message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub role: String,
    pub content: Vec<AssistantContent>,
    pub api: Api,
    pub provider: Provider,
    pub model: String,
    #[serde(
        rename = "responseModel",
        alias = "response_model",
        skip_serializing_if = "Option::is_none"
    )]
    pub response_model: Option<String>,
    #[serde(
        rename = "responseId",
        alias = "response_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub response_id: Option<String>,
    pub usage: Usage,
    #[serde(rename = "stopReason", alias = "stop_reason")]
    pub stop_reason: StopReason,
    #[serde(
        rename = "errorMessage",
        alias = "error_message",
        skip_serializing_if = "Option::is_none"
    )]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<crate::diagnostics::AssistantMessageDiagnostic>>,
    pub timestamp: DateTime<Utc>,
}

/// Tool result message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultMessage {
    pub role: String,
    #[serde(rename = "toolCallId", alias = "tool_call_id")]
    pub tool_call_id: String,
    #[serde(rename = "toolName", alias = "tool_name")]
    pub tool_name: String,
    pub content: Vec<ToolResultContent>,
    pub details: serde_json::Value,
    #[serde(rename = "isError", alias = "is_error")]
    pub is_error: bool,
    pub timestamp: DateTime<Utc>,
}

/// Message content - either string or array of content blocks
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<UserContentBlock>),
}

/// User content block
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum UserContentBlock {
    #[serde(rename = "text")]
    Text(TextContent),
    #[serde(rename = "image")]
    Image(ImageContent),
}

/// Assistant content block
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AssistantContent {
    #[serde(rename = "text")]
    Text(TextContent),
    #[serde(rename = "thinking")]
    Thinking(ThinkingContent),
    #[serde(rename = "toolCall")]
    ToolCall(ToolCall),
}

/// Tool result content block
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolResultContent {
    #[serde(rename = "text")]
    Text(TextContent),
    #[serde(rename = "image")]
    Image(ImageContent),
}

/// Message enum
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum Message {
    #[serde(rename = "user")]
    User(UserMessage),
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),
    #[serde(rename = "toolResult")]
    ToolResult(ToolResultMessage),
}

/// Tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Context for API calls
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    #[serde(
        rename = "systemPrompt",
        alias = "system_prompt",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

/// Model definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub api: Api,
    pub provider: Provider,
    #[serde(rename = "baseUrl", alias = "base_url")]
    pub base_url: String,
    pub reasoning: bool,
    #[serde(
        rename = "thinkingLevelMap",
        alias = "thinking_level_map",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking_level_map: Option<HashMap<ThinkingLevel, Option<String>>>,
    pub input: Vec<String>,
    pub cost: ModelCost,
    #[serde(rename = "contextWindow", alias = "context_window")]
    pub context_window: u32,
    #[serde(rename = "maxTokens", alias = "max_tokens")]
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Compat>,
}

/// Model cost per million tokens
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    #[serde(rename = "cacheRead", alias = "cache_read")]
    pub cache_read: f64,
    #[serde(rename = "cacheWrite", alias = "cache_write")]
    pub cache_write: f64,
}

/// Compatibility settings for OpenAI-compatible APIs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Compat {
    #[serde(
        rename = "supportsStore",
        alias = "supports_store",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_store: Option<bool>,
    #[serde(
        rename = "supportsDeveloperRole",
        alias = "supports_developer_role",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_developer_role: Option<bool>,
    #[serde(
        rename = "supportsReasoningEffort",
        alias = "supports_reasoning_effort",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_reasoning_effort: Option<bool>,
    #[serde(
        rename = "supportsUsageInStreaming",
        alias = "supports_usage_in_streaming",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_usage_in_streaming: Option<bool>,
    #[serde(
        rename = "maxTokensField",
        alias = "max_tokens_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_tokens_field: Option<String>,
    #[serde(
        rename = "requiresToolResultName",
        alias = "requires_tool_result_name",
        skip_serializing_if = "Option::is_none"
    )]
    pub requires_tool_result_name: Option<bool>,
    #[serde(
        rename = "requiresAssistantAfterToolResult",
        alias = "requires_assistant_after_tool_result",
        skip_serializing_if = "Option::is_none"
    )]
    pub requires_assistant_after_tool_result: Option<bool>,
    #[serde(
        rename = "requiresThinkingAsText",
        alias = "requires_thinking_as_text",
        skip_serializing_if = "Option::is_none"
    )]
    pub requires_thinking_as_text: Option<bool>,
    #[serde(
        rename = "requiresReasoningContentOnAssistantMessages",
        alias = "requires_reasoning_content_on_assistant_messages",
        skip_serializing_if = "Option::is_none"
    )]
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    #[serde(
        rename = "thinkingFormat",
        alias = "thinking_format",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking_format: Option<String>,
    #[serde(
        rename = "supportsStrictMode",
        alias = "supports_strict_mode",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_strict_mode: Option<bool>,
    #[serde(
        rename = "cacheControlFormat",
        alias = "cache_control_format",
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_control_format: Option<String>,
    #[serde(
        rename = "sendSessionAffinityHeaders",
        alias = "send_session_affinity_headers",
        skip_serializing_if = "Option::is_none"
    )]
    pub send_session_affinity_headers: Option<bool>,
    #[serde(
        rename = "supportsLongCacheRetention",
        alias = "supports_long_cache_retention",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(
        rename = "openRouterRouting",
        alias = "open_router_routing",
        skip_serializing_if = "Option::is_none"
    )]
    pub open_router_routing: Option<serde_json::Value>,
    #[serde(
        rename = "vercelGatewayRouting",
        alias = "vercel_gateway_routing",
        skip_serializing_if = "Option::is_none"
    )]
    pub vercel_gateway_routing: Option<serde_json::Value>,
    #[serde(
        rename = "zaiToolStream",
        alias = "zai_tool_stream",
        skip_serializing_if = "Option::is_none"
    )]
    pub zai_tool_stream: Option<bool>,
    #[serde(
        rename = "supportsEagerToolInputStreaming",
        alias = "supports_eager_tool_input_streaming",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_eager_tool_input_streaming: Option<bool>,
    #[serde(
        rename = "supportsCacheControlOnTools",
        alias = "supports_cache_control_on_tools",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_cache_control_on_tools: Option<bool>,
    #[serde(
        rename = "forceAdaptiveThinking",
        alias = "force_adaptive_thinking",
        skip_serializing_if = "Option::is_none"
    )]
    pub force_adaptive_thinking: Option<bool>,
    /// Whether the model accepts the Anthropic `temperature` request field.
    /// Claude Opus 4.7+ rejects non-default temperature values. Default: true.
    #[serde(
        rename = "supportsTemperature",
        alias = "supports_temperature",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_temperature: Option<bool>,
    /// Whether to replay empty thinking signatures as `signature: ""` instead
    /// of converting thinking to text. Default: false.
    #[serde(
        rename = "allowEmptySignature",
        alias = "allow_empty_signature",
        skip_serializing_if = "Option::is_none"
    )]
    pub allow_empty_signature: Option<bool>,
}

/// Event from assistant message stream
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AssistantMessageEvent {
    #[serde(rename = "start")]
    Start { partial: AssistantMessage },
    #[serde(rename = "text_start")]
    TextStart {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "text_delta")]
    TextDelta {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "text_end")]
    TextEnd {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        content: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "thinking_start")]
    ThinkingStart {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "thinking_end")]
    ThinkingEnd {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        content: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        #[serde(rename = "toolCall", alias = "tool_call")]
        tool_call: ToolCall,
        partial: AssistantMessage,
    },
    #[serde(rename = "done")]
    Done {
        reason: StopReason,
        message: AssistantMessage,
    },
    #[serde(rename = "error")]
    Error {
        reason: StopReason,
        error: AssistantMessage,
    },
}

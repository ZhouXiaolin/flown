use crate::api_registry::{
    ApiProvider, AssistantMessageEventStream, RawEventStream, register_api_provider_with_source,
    unregister_api_providers,
};
use crate::types::*;
use futures_timer::Delay;
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DEFAULT_API: &str = "faux";
const DEFAULT_PROVIDER: &str = "faux";
const DEFAULT_MODEL_ID: &str = "faux-1";
const DEFAULT_MODEL_NAME: &str = "Faux Model";
const DEFAULT_BASE_URL: &str = "http://localhost:0";
const DEFAULT_MIN_TOKEN_SIZE: usize = 3;
const DEFAULT_MAX_TOKEN_SIZE: usize = 5;

type FauxResponseFuture = Pin<Box<dyn Future<Output = AssistantMessage> + Send>>;
type FauxResponseFactory = Arc<
    dyn Fn(Context, Option<StreamOptions>, usize, Model) -> FauxResponseFuture + Send + Sync,
>;

#[derive(Clone)]
pub struct FauxModelDefinition {
    pub id: String,
    pub name: Option<String>,
    pub reasoning: Option<bool>,
    pub input: Option<Vec<String>>,
    pub cost: Option<ModelCost>,
    pub context_window: Option<u32>,
    pub max_tokens: Option<u32>,
}

#[derive(Clone)]
pub enum FauxResponseStep {
    Message(AssistantMessage),
    Factory(FauxResponseFactory),
}

#[derive(Clone)]
pub enum FauxContentBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(ToolCall),
}

pub struct RegisterFauxProviderOptions {
    pub api: Option<String>,
    pub provider: Option<String>,
    pub models: Option<Vec<FauxModelDefinition>>,
    pub tokens_per_second: Option<u32>,
    pub token_size: Option<FauxTokenSize>,
}

#[derive(Clone, Copy)]
pub struct FauxTokenSize {
    pub min: Option<usize>,
    pub max: Option<usize>,
}

impl Default for RegisterFauxProviderOptions {
    fn default() -> Self {
        Self {
            api: None,
            provider: None,
            models: None,
            tokens_per_second: None,
            token_size: None,
        }
    }
}

#[derive(Clone)]
pub struct FauxProviderRegistration {
    api: Api,
    models: Vec<Model>,
    state: Arc<FauxProviderState>,
    source_id: String,
}

struct FauxProviderState {
    call_count: AtomicUsize,
    pending_responses: Mutex<VecDeque<FauxResponseStep>>,
    prompt_cache: Mutex<HashMap<String, String>>,
}

struct FauxProvider {
    api: Api,
    provider: Provider,
    min_token_size: usize,
    max_token_size: usize,
    tokens_per_second: Option<u32>,
    state: Arc<FauxProviderState>,
}

pub fn faux_text(text: impl Into<String>) -> TextContent {
    TextContent {
        content_type: "text".to_string(),
        text: text.into(),
        text_signature: None,
    }
}

pub fn faux_thinking(thinking: impl Into<String>) -> ThinkingContent {
    ThinkingContent {
        content_type: "thinking".to_string(),
        thinking: thinking.into(),
        thinking_signature: None,
        redacted: None,
    }
}

pub fn faux_tool_call(
    name: impl Into<String>,
    arguments: serde_json::Value,
    id: Option<String>,
) -> ToolCall {
    ToolCall {
        content_type: "toolCall".to_string(),
        id: id.unwrap_or_else(|| random_id("tool")),
        name: name.into(),
        arguments,
        thought_signature: None,
    }
}

pub fn faux_assistant_message(
    content: impl Into<FauxAssistantMessageContent>,
    options: FauxAssistantMessageOptions,
) -> AssistantMessage {
    let content = match content.into() {
        FauxAssistantMessageContent::Text(text) => vec![AssistantContent::Text(faux_text(text))],
        FauxAssistantMessageContent::Block(block) => vec![block.into()],
        FauxAssistantMessageContent::Blocks(blocks) => blocks.into_iter().map(Into::into).collect(),
    };

    AssistantMessage {
        role: "assistant".to_string(),
        content,
        api: Api::Custom(DEFAULT_API.to_string()),
        provider: Provider::Custom(DEFAULT_PROVIDER.to_string()),
        model: DEFAULT_MODEL_ID.to_string(),
        response_model: None,
        response_id: options.response_id,
        usage: Usage::default(),
        stop_reason: options.stop_reason.unwrap_or(StopReason::Stop),
        error_message: options.error_message,
        diagnostics: None,
        timestamp: options.timestamp.unwrap_or_else(chrono::Utc::now),
    }
}

pub fn register_faux_provider(options: RegisterFauxProviderOptions) -> FauxProviderRegistration {
    let api_id = options.api.unwrap_or_else(|| random_id(DEFAULT_API));
    let provider_id = options
        .provider
        .unwrap_or_else(|| DEFAULT_PROVIDER.to_string());
    let source_id = random_id("faux-provider");
    let api = Api::Custom(api_id);
    let provider = Provider::Custom(provider_id);

    let token_size = options.token_size.unwrap_or(FauxTokenSize {
        min: None,
        max: None,
    });
    let min_token_size = token_size.min.unwrap_or(DEFAULT_MIN_TOKEN_SIZE).max(1);
    let max_token_size = token_size
        .max
        .unwrap_or(DEFAULT_MAX_TOKEN_SIZE)
        .max(min_token_size);

    let models = build_models(api.clone(), provider.clone(), options.models);
    let state = Arc::new(FauxProviderState {
        call_count: AtomicUsize::new(0),
        pending_responses: Mutex::new(VecDeque::new()),
        prompt_cache: Mutex::new(HashMap::new()),
    });

    register_api_provider_with_source(
        Arc::new(FauxProvider {
            api: api.clone(),
            provider,
            min_token_size,
            max_token_size,
            tokens_per_second: options.tokens_per_second,
            state: state.clone(),
        }),
        Some(source_id.clone()),
    );

    FauxProviderRegistration {
        api,
        models,
        state,
        source_id,
    }
}

impl FauxProviderRegistration {
    pub fn api(&self) -> &Api {
        &self.api
    }

    pub fn models(&self) -> &[Model] {
        &self.models
    }

    pub fn get_model(&self, model_id: Option<&str>) -> Option<Model> {
        match model_id {
            Some(model_id) => self.models.iter().find(|model| model.id == model_id).cloned(),
            None => self.models.first().cloned(),
        }
    }

    pub fn call_count(&self) -> usize {
        self.state.call_count.load(Ordering::SeqCst)
    }

    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        let mut pending = self.state.pending_responses.lock().unwrap();
        *pending = responses.into_iter().collect();
    }

    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        self.state
            .pending_responses
            .lock()
            .unwrap()
            .extend(responses);
    }

    pub fn pending_response_count(&self) -> usize {
        self.state.pending_responses.lock().unwrap().len()
    }

    pub fn unregister(&self) {
        unregister_api_providers(&self.source_id);
    }
}

impl ApiProvider for FauxProvider {
    fn api(&self) -> Api {
        self.api.clone()
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
    ) -> AssistantMessageEventStream {
        self.stream_impl(model, context, options.cloned())
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.stream_impl(model, context, options.map(|options| options.base.clone()))
    }
}

impl FauxProvider {
    fn stream_impl(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        let model = model.clone();
        let context = context.clone();
        let api = self.api.clone();
        let provider = self.provider.clone();
        let state = self.state.clone();
        let min_token_size = self.min_token_size;
        let max_token_size = self.max_token_size;
        let tokens_per_second = self.tokens_per_second;

        let raw: RawEventStream = Box::pin(async_stream::stream! {
            if let Some(hook) = options.as_ref().and_then(|options| options.on_response.clone()) {
                hook(ProviderResponse {
                    status: 200,
                    headers: HashMap::new(),
                }).await;
            }

            let call_count = state.call_count.fetch_add(1, Ordering::SeqCst) + 1;
            let step = state.pending_responses.lock().unwrap().pop_front();

            let message = match step {
                Some(FauxResponseStep::Message(message)) => {
                    clone_message(message, &api, &provider, &model.id)
                }
                Some(FauxResponseStep::Factory(factory)) => {
                    clone_message(
                        factory(context.clone(), options.clone(), call_count, model.clone()).await,
                        &api,
                        &provider,
                        &model.id,
                    )
                }
                None => create_error_message(
                    "No more faux responses queued",
                    &api,
                    &provider,
                    &model.id,
                ),
            };

            let message = with_usage_estimate(message, &context, options.as_ref(), &state.prompt_cache);
            for event in stream_with_deltas(
                message,
                min_token_size,
                max_token_size,
                tokens_per_second,
                options.as_ref().and_then(|options| options.signal.clone()),
            ).await {
                yield event;
            }
        });

        AssistantMessageEventStream::from_stream(raw)
    }
}

#[derive(Clone)]
pub struct FauxAssistantMessageOptions {
    pub stop_reason: Option<StopReason>,
    pub error_message: Option<String>,
    pub response_id: Option<String>,
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

impl Default for FauxAssistantMessageOptions {
    fn default() -> Self {
        Self {
            stop_reason: None,
            error_message: None,
            response_id: None,
            timestamp: None,
        }
    }
}

pub enum FauxAssistantMessageContent {
    Text(String),
    Block(FauxContentBlock),
    Blocks(Vec<FauxContentBlock>),
}

impl From<String> for FauxAssistantMessageContent {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for FauxAssistantMessageContent {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<FauxContentBlock> for FauxAssistantMessageContent {
    fn from(value: FauxContentBlock) -> Self {
        Self::Block(value)
    }
}

impl From<Vec<FauxContentBlock>> for FauxAssistantMessageContent {
    fn from(value: Vec<FauxContentBlock>) -> Self {
        Self::Blocks(value)
    }
}

impl From<FauxContentBlock> for AssistantContent {
    fn from(value: FauxContentBlock) -> Self {
        match value {
            FauxContentBlock::Text(text) => AssistantContent::Text(text),
            FauxContentBlock::Thinking(thinking) => AssistantContent::Thinking(thinking),
            FauxContentBlock::ToolCall(tool_call) => AssistantContent::ToolCall(tool_call),
        }
    }
}

fn build_models(
    api: Api,
    provider: Provider,
    definitions: Option<Vec<FauxModelDefinition>>,
) -> Vec<Model> {
    let definitions = definitions.unwrap_or_else(|| {
        vec![FauxModelDefinition {
            id: DEFAULT_MODEL_ID.to_string(),
            name: Some(DEFAULT_MODEL_NAME.to_string()),
            reasoning: Some(false),
            input: Some(vec!["text".to_string(), "image".to_string()]),
            cost: Some(ModelCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            }),
            context_window: Some(128_000),
            max_tokens: Some(16_384),
        }]
    });

    definitions
        .into_iter()
        .map(|definition| Model {
            id: definition.id.clone(),
            name: definition.name.unwrap_or_else(|| definition.id.clone()),
            api: api.clone(),
            provider: provider.clone(),
            base_url: DEFAULT_BASE_URL.to_string(),
            reasoning: definition.reasoning.unwrap_or(false),
            thinking_level_map: None,
            input: definition
                .input
                .unwrap_or_else(|| vec!["text".to_string(), "image".to_string()]),
            cost: definition.cost.unwrap_or(ModelCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            }),
            context_window: definition.context_window.unwrap_or(128_000),
            max_tokens: definition.max_tokens.unwrap_or(16_384),
            headers: None,
            compat: None,
        })
        .collect()
}

fn clone_message(message: AssistantMessage, api: &Api, provider: &Provider, model_id: &str) -> AssistantMessage {
    AssistantMessage {
        role: message.role,
        content: message.content,
        api: api.clone(),
        provider: provider.clone(),
        model: model_id.to_string(),
        response_model: message.response_model,
        response_id: message.response_id,
        usage: message.usage,
        stop_reason: message.stop_reason,
        error_message: message.error_message,
        diagnostics: message.diagnostics,
        timestamp: message.timestamp,
    }
}

fn create_error_message(
    error: impl Into<String>,
    api: &Api,
    provider: &Provider,
    model_id: &str,
) -> AssistantMessage {
    AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: api.clone(),
        provider: provider.clone(),
        model: model_id.to_string(),
        response_model: None,
        response_id: None,
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        error_message: Some(error.into()),
        diagnostics: None,
        timestamp: chrono::Utc::now(),
    }
}

fn create_aborted_message(partial: &AssistantMessage) -> AssistantMessage {
    let mut aborted = partial.clone();
    aborted.stop_reason = StopReason::Aborted;
    aborted.error_message = Some("Request was aborted".to_string());
    aborted.timestamp = chrono::Utc::now();
    aborted
}

fn estimate_tokens(text: &str) -> u32 {
    text.chars().count().div_ceil(4) as u32
}

fn serialize_context(context: &Context) -> String {
    let mut parts = Vec::new();
    if let Some(system_prompt) = &context.system_prompt {
        parts.push(format!("system:{system_prompt}"));
    }
    for message in &context.messages {
        parts.push(match message {
            Message::User(message) => format!("user:{}", message_to_text_content(&message.content)),
            Message::Assistant(message) => format!("assistant:{}", assistant_content_to_text(&message.content)),
            Message::ToolResult(message) => format!(
                "toolResult:{}\n{}",
                message.tool_name,
                tool_result_content_to_text(&message.content)
            ),
        });
    }
    if let Some(tools) = &context.tools {
        parts.push(format!("tools:{}", json!(tools)));
    }
    parts.join("\n\n")
}

fn message_to_text_content(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block {
                UserContentBlock::Text(text) => text.text.clone(),
                UserContentBlock::Image(image) => {
                    format!("[image:{}:{}]", image.mime_type, image.data.len())
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn assistant_content_to_text(content: &[AssistantContent]) -> String {
    content
        .iter()
        .map(|block| match block {
            AssistantContent::Text(text) => text.text.clone(),
            AssistantContent::Thinking(thinking) => thinking.thinking.clone(),
            AssistantContent::ToolCall(tool_call) => {
                format!("{}:{}", tool_call.name, tool_call.arguments)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_result_content_to_text(content: &[ToolResultContent]) -> String {
    content
        .iter()
        .map(|block| match block {
            ToolResultContent::Text(text) => text.text.clone(),
            ToolResultContent::Image(image) => {
                format!("[image:{}:{}]", image.mime_type, image.data.len())
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn with_usage_estimate(
    mut message: AssistantMessage,
    context: &Context,
    options: Option<&StreamOptions>,
    prompt_cache: &Mutex<HashMap<String, String>>,
) -> AssistantMessage {
    let prompt_text = serialize_context(context);
    let prompt_tokens = estimate_tokens(&prompt_text);
    let output_tokens = estimate_tokens(&assistant_content_to_text(&message.content));
    let mut input = prompt_tokens;
    let mut cache_read = 0;
    let mut cache_write = 0;

    if let Some(session_id) = options
        .and_then(|options| options.session_id.as_ref())
        .filter(|_| options.and_then(|options| options.cache_retention.as_ref()) != Some(&CacheRetention::None))
    {
        let mut cache = prompt_cache.lock().unwrap();
        if let Some(previous_prompt) = cache.get(session_id) {
            let cached_chars = common_prefix_len(previous_prompt, &prompt_text);
            cache_read = estimate_tokens(&previous_prompt[..cached_chars]);
            cache_write = estimate_tokens(&prompt_text[cached_chars..]);
            input = prompt_tokens.saturating_sub(cache_read);
        } else {
            cache_write = prompt_tokens;
        }
        cache.insert(session_id.clone(), prompt_text);
    }

    message.usage = Usage {
        input,
        output: output_tokens,
        cache_read,
        cache_write,
        cache_write_1h: None,
        total_tokens: input + output_tokens + cache_read + cache_write,
        cost: Cost::default(),
    };
    message
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(lhs, rhs)| lhs == rhs)
        .map(|(ch, _)| ch.len_utf8())
        .sum()
}

async fn stream_with_deltas(
    message: AssistantMessage,
    min_token_size: usize,
    max_token_size: usize,
    tokens_per_second: Option<u32>,
    signal: Option<AbortSignal>,
) -> Vec<AssistantMessageEvent> {
    let mut events = Vec::new();
    let mut partial = AssistantMessage {
        role: message.role.clone(),
        content: Vec::new(),
        api: message.api.clone(),
        provider: message.provider.clone(),
        model: message.model.clone(),
        response_model: message.response_model.clone(),
        response_id: message.response_id.clone(),
        usage: message.usage.clone(),
        stop_reason: message.stop_reason.clone(),
        error_message: message.error_message.clone(),
        diagnostics: message.diagnostics.clone(),
        timestamp: message.timestamp,
    };

    if signal.as_ref().is_some_and(AbortSignal::is_cancelled) {
        let aborted = create_aborted_message(&partial);
        events.push(AssistantMessageEvent::Error {
            reason: StopReason::Aborted,
            error: aborted,
        });
        return events;
    }

    events.push(AssistantMessageEvent::Start {
        partial: partial.clone(),
    });

    for (index, block) in message.content.iter().enumerate() {
        match block {
            AssistantContent::Thinking(thinking) => {
                partial.content.push(AssistantContent::Thinking(ThinkingContent {
                    content_type: "thinking".to_string(),
                    thinking: String::new(),
                    thinking_signature: None,
                    redacted: thinking.redacted,
                }));
                events.push(AssistantMessageEvent::ThinkingStart {
                    content_index: index,
                    partial: partial.clone(),
                });
                for chunk in split_string_by_token_size(&thinking.thinking, min_token_size, max_token_size) {
                    schedule_chunk(&chunk, tokens_per_second).await;
                    if signal.as_ref().is_some_and(AbortSignal::is_cancelled) {
                        let aborted = create_aborted_message(&partial);
                        events.push(AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: aborted,
                        });
                        return events;
                    }
                    if let Some(AssistantContent::Thinking(partial_thinking)) = partial.content.get_mut(index) {
                        partial_thinking.thinking.push_str(&chunk);
                    }
                    events.push(AssistantMessageEvent::ThinkingDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                events.push(AssistantMessageEvent::ThinkingEnd {
                    content_index: index,
                    content: thinking.thinking.clone(),
                    partial: partial.clone(),
                });
            }
            AssistantContent::Text(text) => {
                partial.content.push(AssistantContent::Text(TextContent {
                    content_type: "text".to_string(),
                    text: String::new(),
                    text_signature: None,
                }));
                events.push(AssistantMessageEvent::TextStart {
                    content_index: index,
                    partial: partial.clone(),
                });
                for chunk in split_string_by_token_size(&text.text, min_token_size, max_token_size) {
                    schedule_chunk(&chunk, tokens_per_second).await;
                    if signal.as_ref().is_some_and(AbortSignal::is_cancelled) {
                        let aborted = create_aborted_message(&partial);
                        events.push(AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: aborted,
                        });
                        return events;
                    }
                    if let Some(AssistantContent::Text(partial_text)) = partial.content.get_mut(index) {
                        partial_text.text.push_str(&chunk);
                    }
                    events.push(AssistantMessageEvent::TextDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                events.push(AssistantMessageEvent::TextEnd {
                    content_index: index,
                    content: text.text.clone(),
                    partial: partial.clone(),
                });
            }
            AssistantContent::ToolCall(tool_call) => {
                partial.content.push(AssistantContent::ToolCall(ToolCall {
                    content_type: "toolCall".to_string(),
                    id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    arguments: json!({}),
                    thought_signature: None,
                }));
                events.push(AssistantMessageEvent::ToolCallStart {
                    content_index: index,
                    partial: partial.clone(),
                });
                for chunk in split_string_by_token_size(
                    &serde_json::to_string(&tool_call.arguments).unwrap_or_else(|_| "{}".to_string()),
                    min_token_size,
                    max_token_size,
                ) {
                    schedule_chunk(&chunk, tokens_per_second).await;
                    if signal.as_ref().is_some_and(AbortSignal::is_cancelled) {
                        let aborted = create_aborted_message(&partial);
                        events.push(AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: aborted,
                        });
                        return events;
                    }
                    events.push(AssistantMessageEvent::ToolCallDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                if let Some(AssistantContent::ToolCall(partial_tool_call)) = partial.content.get_mut(index) {
                    partial_tool_call.arguments = tool_call.arguments.clone();
                }
                events.push(AssistantMessageEvent::ToolCallEnd {
                    content_index: index,
                    tool_call: tool_call.clone(),
                    partial: partial.clone(),
                });
            }
        }
    }

    if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
        events.push(AssistantMessageEvent::Error {
            reason: message.stop_reason.clone(),
            error: message,
        });
    } else {
        events.push(AssistantMessageEvent::Done {
            reason: message.stop_reason.clone(),
            message,
        });
    }

    events
}

fn split_string_by_token_size(text: &str, min_token_size: usize, max_token_size: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }

    let step = (min_token_size.max(1) + max_token_size.max(min_token_size)) / 2;
    let char_size = (step.max(1)) * 4;
    let chars: Vec<char> = text.chars().collect();
    chars.chunks(char_size)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

async fn schedule_chunk(chunk: &str, tokens_per_second: Option<u32>) {
    let Some(tokens_per_second) = tokens_per_second.filter(|rate| *rate > 0) else {
        return;
    };
    let delay_ms = ((estimate_tokens(chunk) as f64 / tokens_per_second as f64) * 1000.0).max(0.0);
    Delay::new(Duration::from_millis(delay_ms as u64)).await;
}

fn random_id(prefix: &str) -> String {
    format!("{prefix}:{}", uuid::Uuid::new_v4())
}

use crate::api_registry::{ApiProvider, AssistantMessageEventStream, RawEventStream};
use crate::models::{calculate_cost, clamp_thinking_level, transform_messages};
use crate::providers::common::{
    build_copilot_dynamic_headers, is_cloudflare_ai_gateway,
    resolve_cache_retention as resolve_common_cache_retention, resolve_cloudflare_base_url,
};
use crate::providers::json::parse_streaming_json;
use crate::types::*;
use async_stream::stream;
use eventsource_stream::Eventsource;
use futures::{FutureExt, stream::StreamExt};
use reqwest::Client;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;
const FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";

#[derive(Debug, Clone)]
struct AnthropicCompat {
    supports_eager_tool_input_streaming: bool,
    supports_long_cache_retention: bool,
    send_session_affinity_headers: bool,
    supports_cache_control_on_tools: bool,
    force_adaptive_thinking: bool,
    supports_temperature: bool,
    allow_empty_signature: bool,
}

impl AnthropicCompat {
    fn resolve(model: &Model) -> Self {
        let provider = model.provider.to_string();
        let is_fireworks = provider == "fireworks";
        let is_cloudflare_ai_gateway_anthropic =
            provider == "cloudflare-ai-gateway" && model.base_url.contains("anthropic");
        let compat = model.compat.as_ref();

        Self {
            supports_eager_tool_input_streaming: compat
                .and_then(|compat| compat.supports_eager_tool_input_streaming)
                .unwrap_or(!is_fireworks),
            supports_long_cache_retention: compat
                .and_then(|compat| compat.supports_long_cache_retention)
                .unwrap_or(!is_fireworks),
            send_session_affinity_headers: compat
                .and_then(|compat| compat.send_session_affinity_headers)
                .unwrap_or(is_fireworks || is_cloudflare_ai_gateway_anthropic),
            supports_cache_control_on_tools: compat
                .and_then(|compat| compat.supports_cache_control_on_tools)
                .unwrap_or(!is_fireworks),
            force_adaptive_thinking: compat
                .and_then(|compat| compat.force_adaptive_thinking)
                .unwrap_or(false),
            supports_temperature: compat
                .and_then(|compat| compat.supports_temperature)
                .unwrap_or(true),
            allow_empty_signature: compat
                .and_then(|compat| compat.allow_empty_signature)
                .unwrap_or(false),
        }
    }
}

/// Effort level for adaptive-thinking Anthropic models, mirroring pi-ai's
/// `AnthropicEffort` union. `"max"` forces unconstrained thinking (Opus 4.6
/// only); the others map to Claude 4.7+ adaptive output_config.effort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicEffort {
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

/// How thinking content is returned in Anthropic API responses, mirroring
/// pi-ai's `AnthropicThinkingDisplay`. `"summarized"` yields readable
/// thinking text; `"omitted"` returns empty thinking blocks with the
/// signature only (faster time-to-first-text-token when the UI does not
/// surface thinking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicThinkingDisplay {
    Summarized,
    Omitted,
}

/// Public Anthropic provider options, mirroring pi-mono's `AnthropicOptions`.
#[derive(Debug, Clone, Default)]
pub struct AnthropicOptions {
    pub base: StreamOptions,
    pub thinking_enabled: Option<bool>,
    pub thinking_budget_tokens: Option<u32>,
    pub effort: Option<AnthropicEffort>,
    pub thinking_display: Option<AnthropicThinkingDisplay>,
    pub interleaved_thinking: Option<bool>,
    pub tool_choice: Option<AnthropicToolChoice>,
}

/// Anthropic tool choice policy, mirroring pi-mono's public union type.
#[derive(Debug, Clone)]
pub enum AnthropicToolChoice {
    Auto,
    Any,
    None,
    Tool { name: String },
}

/// Function-style Anthropic provider. Mirrors pi-ai's `streamAnthropic` /
/// `streamSimpleAnthropic`: a unit struct implementing [`ApiProvider`] so the
/// registry can dispatch to it. Not exported — embedders register the
/// built-in via [`crate::register_built_in_api_providers`].
struct AnthropicApiProvider;

impl AnthropicApiProvider {
    fn client() -> Client {
        Client::new()
    }

    fn build_request_body(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        thinking: Option<serde_json::Value>,
    ) -> serde_json::Value {
        let compat = AnthropicCompat::resolve(model);
        let is_oauth = options_is_oauth(options);
        let cache_control = anthropic_cache_control(model, options, &compat);
        let mut body = serde_json::json!({
            "model": model.id,
            "messages": convert_messages(
                &transform_messages(
                &context.messages,
                model,
                Some(&|id, _model, _source| normalize_tool_call_id(id)),
                ),
                is_oauth,
                context.tools.as_deref(),
            ),
            "max_tokens": options.and_then(|options| options.max_tokens).unwrap_or(model.max_tokens),
            "stream": true,
        });

        if let Some(system_prompt) = &context.system_prompt {
            let mut system = Vec::new();
            if is_oauth {
                system.push(serde_json::json!({
                    "type": "text",
                    "text": "You are Claude Code, Anthropic's official CLI for Claude.",
                }));
            }
            system.push(serde_json::json!({
                "type": "text",
                "text": system_prompt,
            }));
            body["system"] = serde_json::Value::Array(system);
            if let Some(cache_control) = &cache_control {
                if let Some(system) = body["system"].as_array_mut() {
                    for block in system {
                        block["cache_control"] = cache_control.clone();
                    }
                }
            }
        } else if is_oauth {
            body["system"] = serde_json::json!([{
                "type": "text",
                "text": "You are Claude Code, Anthropic's official CLI for Claude.",
            }]);
            if let Some(cache_control) = &cache_control {
                body["system"][0]["cache_control"] = cache_control.clone();
            }
        }

        if let Some(temperature) = options.and_then(|options| options.temperature) {
            let thinking_is_disabled = thinking
                .as_ref()
                .and_then(|thinking| thinking.get("type"))
                .and_then(|kind| kind.as_str())
                == Some("disabled");
            if (thinking.is_none() || thinking_is_disabled)
                && !options
                    .and_then(|options| options.thinking_enabled)
                    .unwrap_or(false)
            {
                body["temperature"] = serde_json::json!(temperature);
            }
        }

        if let Some(tools) = &context.tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(convert_tools(
                    tools,
                    is_oauth,
                    compat.supports_eager_tool_input_streaming,
                    if compat.supports_cache_control_on_tools {
                        cache_control.clone()
                    } else {
                        None
                    },
                ));
            }
        }

        if let Some(metadata) = options.and_then(|options| options.metadata.as_ref()) {
            if let Some(user_id) = metadata.get("user_id").and_then(|user_id| user_id.as_str()) {
                body["metadata"] = serde_json::json!({ "user_id": user_id });
            }
        }

        if let Some(mut thinking) =
            thinking.or_else(|| build_anthropic_thinking(model, options, &compat))
        {
            if let Some(output_config) = thinking.get("__output_config").cloned() {
                body["output_config"] = output_config;
                if let Some(object) = thinking.as_object_mut() {
                    object.remove("__output_config");
                }
            }
            body["thinking"] = thinking;
        }

        if let Some(options) = options {
            if let Some(tool_choice) = &options.tool_choice {
                body["tool_choice"] = normalize_anthropic_tool_choice(tool_choice);
            }
        }

        if cache_control.is_some() {
            apply_anthropic_message_cache_control(&mut body, cache_control.as_ref().unwrap());
        }

        body
    }

    fn build_simple_request_body(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
    ) -> serde_json::Value {
        let compat = AnthropicCompat::resolve(model);
        let adjusted = options
            .and_then(|options| options.reasoning.as_ref())
            .filter(|reasoning| **reasoning != ThinkingLevel::Off)
            .filter(|_| !compat.force_adaptive_thinking)
            .map(|reasoning| {
                adjust_max_tokens_for_thinking(
                    options.and_then(|options| options.base.max_tokens),
                    model.max_tokens,
                    reasoning,
                    options.and_then(|options| options.thinking_budgets.as_ref()),
                )
            });
        let thinking = if model.reasoning {
            match options.and_then(|options| options.reasoning.as_ref()) {
                Some(ThinkingLevel::Off) | None => Some(serde_json::json!({ "type": "disabled" })),
                Some(level) if compat.force_adaptive_thinking => {
                    let mut thinking = serde_json::json!({
                        "type": "adaptive",
                        "display": options
                            .and_then(|options| options.base.thinking_display.as_deref())
                            .unwrap_or("summarized"),
                    });
                    let effort = options
                        .and_then(|options| options.base.effort.clone())
                        .unwrap_or_else(|| map_thinking_level_to_effort(model, level));
                    let mut output_config = serde_json::Map::new();
                    output_config.insert("effort".to_string(), serde_json::json!(effort));
                    thinking["__output_config"] = serde_json::Value::Object(output_config);
                    Some(thinking)
                }
                Some(_) => Some(serde_json::json!({
                    "type": "enabled",
                    "budget_tokens": adjusted
                        .as_ref()
                        .map(|adjusted| adjusted.thinking_budget)
                        .or_else(|| options.and_then(|options| options.base.thinking_budget_tokens))
                        .unwrap_or(1024),
                    "display": options
                        .and_then(|options| options.base.thinking_display.as_deref())
                        .unwrap_or("summarized"),
                })),
            }
        } else {
            None
        };
        let mut base = options.map(|options| options.base.clone());
        if let (Some(base), Some(adjusted)) = (base.as_mut(), adjusted) {
            base.max_tokens = Some(adjusted.max_tokens);
            base.thinking_enabled = Some(true);
            base.thinking_budget_tokens = Some(adjusted.thinking_budget);
        }

        self.build_request_body(
            model,
            context,
            base.as_ref()
                .or_else(|| options.map(|options| &options.base)),
            thinking,
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct AdjustedThinkingTokens {
    max_tokens: u32,
    thinking_budget: u32,
}

fn adjust_max_tokens_for_thinking(
    base_max_tokens: Option<u32>,
    model_max_tokens: u32,
    reasoning_level: &ThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> AdjustedThinkingTokens {
    let mut thinking_budget = thinking_budget(Some(reasoning_level), custom_budgets)
        .unwrap_or_else(|| match clamp_reasoning(reasoning_level) {
            ThinkingLevel::Minimal => 1024,
            ThinkingLevel::Low => 2048,
            ThinkingLevel::Medium => 8192,
            ThinkingLevel::High => 16_384,
            ThinkingLevel::Off | ThinkingLevel::XHigh => 16_384,
        });
    let max_tokens = base_max_tokens
        .map(|base| base.saturating_add(thinking_budget).min(model_max_tokens))
        .unwrap_or(model_max_tokens);

    if max_tokens <= thinking_budget {
        thinking_budget = max_tokens.saturating_sub(1024);
    }

    AdjustedThinkingTokens {
        max_tokens,
        thinking_budget,
    }
}

fn clamp_reasoning(level: &ThinkingLevel) -> ThinkingLevel {
    match level {
        ThinkingLevel::XHigh => ThinkingLevel::High,
        level => level.clone(),
    }
}

fn map_thinking_level_to_effort(model: &Model, level: &ThinkingLevel) -> String {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(level))
        .and_then(|mapped| mapped.clone())
        .unwrap_or_else(|| match level {
            ThinkingLevel::Minimal | ThinkingLevel::Low => "low".to_string(),
            ThinkingLevel::Medium => "medium".to_string(),
            ThinkingLevel::High | ThinkingLevel::XHigh => "high".to_string(),
            ThinkingLevel::Off => "high".to_string(),
        })
}

fn resolve_cache_retention(options: Option<&StreamOptions>) -> CacheRetention {
    resolve_common_cache_retention(options.and_then(|options| options.cache_retention.as_ref()))
}

fn anthropic_cache_control(
    model: &Model,
    options: Option<&StreamOptions>,
    compat: &AnthropicCompat,
) -> Option<serde_json::Value> {
    let retention = resolve_cache_retention(options);
    if retention == CacheRetention::None {
        return None;
    }

    let mut cache_control = serde_json::json!({ "type": "ephemeral" });
    if retention == CacheRetention::Long && compat.supports_long_cache_retention {
        cache_control["ttl"] = serde_json::json!("1h");
    }

    // Keep the model argument in the signature to match the compat-oriented call
    // shape used elsewhere, even though current cache behavior only needs compat.
    let _ = model;
    Some(cache_control)
}

fn build_anthropic_thinking(
    model: &Model,
    options: Option<&StreamOptions>,
    compat: &AnthropicCompat,
) -> Option<serde_json::Value> {
    if !model.reasoning {
        return None;
    }

    match options.and_then(|options| options.thinking_enabled) {
        Some(true) if compat.force_adaptive_thinking => {
            let mut thinking = serde_json::json!({
                "type": "adaptive",
                "display": options
                    .and_then(|options| options.thinking_display.as_deref())
                    .unwrap_or("summarized"),
            });
            if let Some(effort) = options.and_then(|options| options.effort.as_ref()) {
                thinking["__output_config"] = serde_json::json!({ "effort": effort });
            }
            Some(thinking)
        }
        Some(true) => Some(serde_json::json!({
            "type": "enabled",
            "budget_tokens": options
                .and_then(|options| options.thinking_budget_tokens)
                .unwrap_or(1024),
            "display": options
                .and_then(|options| options.thinking_display.as_deref())
                .unwrap_or("summarized"),
        })),
        Some(false) => Some(serde_json::json!({ "type": "disabled" })),
        None => None,
    }
}

fn normalize_anthropic_tool_choice(tool_choice: &serde_json::Value) -> serde_json::Value {
    if let Some(choice) = tool_choice.as_str() {
        serde_json::json!({ "type": choice })
    } else {
        tool_choice.clone()
    }
}

fn apply_anthropic_message_cache_control(
    body: &mut serde_json::Value,
    cache_control: &serde_json::Value,
) {
    let Some(messages) = body
        .get_mut("messages")
        .and_then(|messages| messages.as_array_mut())
    else {
        return;
    };

    let Some(last_message) = messages.last_mut() else {
        return;
    };
    if last_message.get("role").and_then(|role| role.as_str()) != Some("user") {
        return;
    }

    let content = &mut last_message["content"];
    if let Some(text) = content.as_str() {
        *content = serde_json::json!([{
            "type": "text",
            "text": text,
            "cache_control": cache_control,
        }]);
        return;
    }

    if let Some(blocks) = content.as_array_mut() {
        if let Some(last_block) = blocks.last_mut() {
            last_block["cache_control"] = cache_control.clone();
        }
    }
}

fn thinking_budget(
    level: Option<&ThinkingLevel>,
    budgets: Option<&ThinkingBudgets>,
) -> Option<u32> {
    let budgets = budgets?;
    match level {
        Some(ThinkingLevel::Minimal) => budgets.minimal,
        Some(ThinkingLevel::Low) => budgets.low,
        Some(ThinkingLevel::Medium) => budgets.medium,
        Some(ThinkingLevel::High) | Some(ThinkingLevel::XHigh) => budgets.high,
        Some(ThinkingLevel::Off) | None => None,
    }
}

fn convert_messages(
    messages: &[Message],
    is_oauth: bool,
    _tools: Option<&[Tool]>,
) -> Vec<serde_json::Value> {
    let mut converted = Vec::new();
    let mut index = 0;

    while index < messages.len() {
        match &messages[index] {
            Message::User(message) => {
                let content = match &message.content {
                    MessageContent::Text(text) => {
                        if text.trim().is_empty() {
                            index += 1;
                            continue;
                        }
                        serde_json::json!(text)
                    }
                    MessageContent::Blocks(blocks) => {
                        let blocks: Vec<_> = blocks
                            .iter()
                            .filter_map(|block| match block {
                                UserContentBlock::Text(text) if text.text.trim().is_empty() => None,
                                UserContentBlock::Text(text) => Some(serde_json::json!({
                                    "type": "text",
                                    "text": text.text,
                                })),
                                UserContentBlock::Image(image) => Some(serde_json::json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": image.mime_type,
                                        "data": image.data,
                                    },
                                })),
                            })
                            .collect();
                        if blocks.is_empty() {
                            index += 1;
                            continue;
                        }
                        serde_json::json!(blocks)
                    }
                };

                converted.push(serde_json::json!({
                    "role": "user",
                    "content": content,
                }));
            }
            Message::Assistant(message) => {
                let blocks: Vec<_> = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContent::Text(text) if text.text.trim().is_empty() => None,
                        AssistantContent::Text(text) => Some(serde_json::json!({
                            "type": "text",
                            "text": text.text,
                        })),
                        AssistantContent::Thinking(thinking)
                            if thinking.redacted.unwrap_or(false) =>
                        {
                            thinking.thinking_signature.as_ref().map(|signature| {
                                serde_json::json!({
                                    "type": "redacted_thinking",
                                    "data": signature,
                                })
                            })
                        }
                        AssistantContent::Thinking(thinking)
                            if thinking.thinking.trim().is_empty() =>
                        {
                            None
                        }
                        AssistantContent::Thinking(thinking)
                            if thinking
                                .thinking_signature
                                .as_deref()
                                .unwrap_or("")
                                .trim()
                                .is_empty() =>
                        {
                            Some(serde_json::json!({
                                "type": "text",
                                "text": thinking.thinking,
                            }))
                        }
                        AssistantContent::Thinking(thinking) => Some(serde_json::json!({
                            "type": "thinking",
                            "thinking": thinking.thinking,
                            "signature": thinking.thinking_signature,
                        })),
                        AssistantContent::ToolCall(tool_call) => Some(serde_json::json!({
                            "type": "tool_use",
                            "id": normalize_tool_call_id(&tool_call.id),
                            "name": if is_oauth { to_claude_code_name(&tool_call.name) } else { tool_call.name.clone() },
                            "input": tool_call.arguments,
                        })),
                    })
                    .collect();
                if !blocks.is_empty() {
                    converted.push(serde_json::json!({
                        "role": "assistant",
                        "content": blocks,
                    }));
                }
            }
            Message::ToolResult(_) => {
                let mut tool_results = Vec::new();
                while index < messages.len() {
                    let Message::ToolResult(result) = &messages[index] else {
                        break;
                    };
                    tool_results.push(serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": normalize_tool_call_id(&result.tool_call_id),
                        "content": convert_tool_result_content(&result.content),
                        "is_error": result.is_error,
                    }));
                    index += 1;
                }
                converted.push(serde_json::json!({
                    "role": "user",
                    "content": tool_results,
                }));
                continue;
            }
        }

        index += 1;
    }

    converted
}

fn convert_tool_result_content(content: &[ToolResultContent]) -> serde_json::Value {
    let has_images = content
        .iter()
        .any(|content| matches!(content, ToolResultContent::Image(_)));
    if !has_images {
        return serde_json::json!(
            content
                .iter()
                .filter_map(|content| match content {
                    ToolResultContent::Text(text) => Some(text.text.as_str()),
                    ToolResultContent::Image(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    let mut blocks = Vec::new();
    let mut has_text = false;
    for content in content {
        match content {
            ToolResultContent::Text(text) => {
                has_text = true;
                blocks.push(serde_json::json!({
                    "type": "text",
                    "text": text.text,
                }));
            }
            ToolResultContent::Image(image) => {
                blocks.push(serde_json::json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": image.mime_type,
                        "data": image.data,
                    },
                }));
            }
        }
    }
    if !has_text {
        blocks.insert(
            0,
            serde_json::json!({
                "type": "text",
                "text": "(see attached image)",
            }),
        );
    }

    serde_json::json!(blocks)
}

fn convert_tools(
    tools: &[Tool],
    is_oauth: bool,
    supports_eager_tool_input_streaming: bool,
    cache_control: Option<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let last_index = tools.len().saturating_sub(1);
    tools
        .iter()
        .enumerate()
        .map(|(index, tool)| {
            let mut converted = serde_json::json!({
                "name": if is_oauth { to_claude_code_name(&tool.name) } else { tool.name.clone() },
                "description": tool.description,
                "input_schema": {
                    "type": "object",
                    "properties": tool.parameters.get("properties").cloned().unwrap_or_else(|| serde_json::json!({})),
                    "required": tool.parameters.get("required").cloned().unwrap_or_else(|| serde_json::json!([])),
                },
            });
            if supports_eager_tool_input_streaming {
                converted["eager_input_streaming"] = serde_json::json!(true);
            }
            if index == last_index {
                if let Some(cache_control) = &cache_control {
                    converted["cache_control"] = cache_control.clone();
                }
            }
            converted
        })
        .collect()
}

fn options_is_oauth(options: Option<&StreamOptions>) -> bool {
    options
        .and_then(|options| options.api_key.as_deref())
        .is_some_and(|api_key| api_key.contains("sk-ant-oat"))
}

const CLAUDE_CODE_TOOLS: [&str; 15] = [
    "Bash",
    "Edit",
    "MultiEdit",
    "Read",
    "Write",
    "Grep",
    "Glob",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "KillShell",
    "NotebookEdit",
    "Skill",
    "Task",
    "TodoWrite",
];

fn to_claude_code_name(name: &str) -> String {
    CLAUDE_CODE_TOOLS
        .iter()
        .find(|tool| tool.eq_ignore_ascii_case(name))
        .copied()
        .unwrap_or(name)
        .to_string()
}

fn from_claude_code_name(name: &str, tools: Option<&[Tool]>) -> String {
    if let Some(tools) = tools {
        if let Some(tool) = tools
            .iter()
            .find(|tool| tool.name.eq_ignore_ascii_case(name))
        {
            return tool.name.clone();
        }
    }
    name.to_string()
}

fn normalize_tool_call_id(id: &str) -> String {
    id.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

#[derive(Debug)]
struct AnthropicStreamParser {
    model: Model,
    partial: AssistantMessage,
    is_oauth: bool,
    tools: Vec<Tool>,
    saw_message_start: bool,
    saw_message_stop: bool,
    blocks: HashMap<usize, usize>,
    tool_partials: HashMap<usize, String>,
}

impl AnthropicStreamParser {
    fn new(model: &Model, is_oauth: bool, tools: Vec<Tool>) -> Self {
        Self {
            model: model.clone(),
            partial: AssistantMessage {
                role: "assistant".to_string(),
                content: Vec::new(),
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                response_model: None,
                response_id: None,
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                error_message: None,
                diagnostics: None,
                timestamp: chrono::Utc::now(),
            },
            is_oauth,
            tools,
            saw_message_start: false,
            saw_message_stop: false,
            blocks: HashMap::new(),
            tool_partials: HashMap::new(),
        }
    }

    fn partial(&self) -> AssistantMessage {
        self.partial.clone()
    }

    fn process_data(&mut self, data: &str) -> Vec<AssistantMessageEvent> {
        let Ok(data) = serde_json::from_str::<serde_json::Value>(data) else {
            return Vec::new();
        };

        let Some(event_type) = data.get("type").and_then(|kind| kind.as_str()) else {
            return Vec::new();
        };

        let mut events = Vec::new();
        match event_type {
            "message_start" => {
                self.saw_message_start = true;
                if let Some(message) = data.get("message") {
                    if let Some(id) = message.get("id").and_then(|id| id.as_str()) {
                        self.partial.response_id = Some(id.to_string());
                    }
                    if let Some(usage) = message.get("usage") {
                        self.update_usage(usage);
                    }
                }
            }
            "content_block_start" => {
                let index = data
                    .get("index")
                    .and_then(|index| index.as_u64())
                    .unwrap_or(0) as usize;
                let Some(block) = data.get("content_block") else {
                    return events;
                };
                match block.get("type").and_then(|kind| kind.as_str()) {
                    Some("text") => {
                        let content_index = self.partial.content.len();
                        self.partial
                            .content
                            .push(AssistantContent::Text(TextContent {
                                content_type: "text".to_string(),
                                text: block
                                    .get("text")
                                    .and_then(|text| text.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                text_signature: None,
                            }));
                        self.blocks.insert(index, content_index);
                        events.push(AssistantMessageEvent::TextStart {
                            content_index,
                            partial: self.partial.clone(),
                        });
                    }
                    Some("thinking") => {
                        let content_index = self.partial.content.len();
                        self.partial
                            .content
                            .push(AssistantContent::Thinking(ThinkingContent {
                                content_type: "thinking".to_string(),
                                thinking: String::new(),
                                thinking_signature: Some(String::new()),
                                redacted: None,
                            }));
                        self.blocks.insert(index, content_index);
                        events.push(AssistantMessageEvent::ThinkingStart {
                            content_index,
                            partial: self.partial.clone(),
                        });
                    }
                    Some("redacted_thinking") => {
                        let content_index = self.partial.content.len();
                        self.partial
                            .content
                            .push(AssistantContent::Thinking(ThinkingContent {
                                content_type: "thinking".to_string(),
                                thinking: "[Reasoning redacted]".to_string(),
                                thinking_signature: block
                                    .get("data")
                                    .and_then(|data| data.as_str())
                                    .map(ToString::to_string),
                                redacted: Some(true),
                            }));
                        self.blocks.insert(index, content_index);
                        events.push(AssistantMessageEvent::ThinkingStart {
                            content_index,
                            partial: self.partial.clone(),
                        });
                    }
                    Some("tool_use") => {
                        let content_index = self.partial.content.len();
                        self.partial
                            .content
                            .push(AssistantContent::ToolCall(ToolCall {
                                content_type: "toolCall".to_string(),
                                id: block
                                    .get("id")
                                    .and_then(|id| id.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                name: block
                                    .get("name")
                                    .and_then(|name| name.as_str())
                                    .map(|name| {
                                        if self.is_oauth {
                                            from_claude_code_name(name, Some(&self.tools))
                                        } else {
                                            name.to_string()
                                        }
                                    })
                                    .unwrap_or_default(),
                                arguments: block
                                    .get("input")
                                    .cloned()
                                    .unwrap_or_else(|| serde_json::json!({})),
                                thought_signature: None,
                            }));
                        self.blocks.insert(index, content_index);
                        self.tool_partials.insert(content_index, String::new());
                        events.push(AssistantMessageEvent::ToolCallStart {
                            content_index,
                            partial: self.partial.clone(),
                        });
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let stream_index = data
                    .get("index")
                    .and_then(|index| index.as_u64())
                    .unwrap_or(0) as usize;
                let Some(content_index) = self.blocks.get(&stream_index).copied() else {
                    return events;
                };
                let Some(delta) = data.get("delta") else {
                    return events;
                };
                match delta.get("type").and_then(|kind| kind.as_str()) {
                    Some("text_delta") => {
                        let text = delta
                            .get("text")
                            .and_then(|text| text.as_str())
                            .unwrap_or("");
                        if let Some(AssistantContent::Text(block)) =
                            self.partial.content.get_mut(content_index)
                        {
                            block.text.push_str(text);
                        }
                        events.push(AssistantMessageEvent::TextDelta {
                            content_index,
                            delta: text.to_string(),
                            partial: self.partial.clone(),
                        });
                    }
                    Some("thinking_delta") => {
                        let thinking = delta
                            .get("thinking")
                            .and_then(|thinking| thinking.as_str())
                            .unwrap_or("");
                        if let Some(AssistantContent::Thinking(block)) =
                            self.partial.content.get_mut(content_index)
                        {
                            block.thinking.push_str(thinking);
                        }
                        events.push(AssistantMessageEvent::ThinkingDelta {
                            content_index,
                            delta: thinking.to_string(),
                            partial: self.partial.clone(),
                        });
                    }
                    Some("signature_delta") => {
                        let signature = delta
                            .get("signature")
                            .and_then(|signature| signature.as_str())
                            .unwrap_or("");
                        if let Some(AssistantContent::Thinking(block)) =
                            self.partial.content.get_mut(content_index)
                        {
                            block
                                .thinking_signature
                                .get_or_insert_with(String::new)
                                .push_str(signature);
                        }
                    }
                    Some("input_json_delta") => {
                        let partial_json = delta
                            .get("partial_json")
                            .and_then(|json| json.as_str())
                            .unwrap_or("");
                        let raw = self.tool_partials.entry(content_index).or_default();
                        raw.push_str(partial_json);
                        if let Some(AssistantContent::ToolCall(block)) =
                            self.partial.content.get_mut(content_index)
                        {
                            block.arguments = parse_streaming_json(raw);
                        }
                        events.push(AssistantMessageEvent::ToolCallDelta {
                            content_index,
                            delta: partial_json.to_string(),
                            partial: self.partial.clone(),
                        });
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let stream_index = data
                    .get("index")
                    .and_then(|index| index.as_u64())
                    .unwrap_or(0) as usize;
                let Some(content_index) = self.blocks.remove(&stream_index) else {
                    return events;
                };
                if let Some(raw) = self.tool_partials.remove(&content_index) {
                    if let Some(AssistantContent::ToolCall(tool_call)) =
                        self.partial.content.get_mut(content_index)
                    {
                        tool_call.arguments = parse_streaming_json(&raw);
                        let tool_call = tool_call.clone();
                        events.push(AssistantMessageEvent::ToolCallEnd {
                            content_index,
                            tool_call,
                            partial: self.partial.clone(),
                        });
                    }
                } else if let Some(block) = self.partial.content.get(content_index) {
                    match block {
                        AssistantContent::Text(text) => {
                            events.push(AssistantMessageEvent::TextEnd {
                                content_index,
                                content: text.text.clone(),
                                partial: self.partial.clone(),
                            })
                        }
                        AssistantContent::Thinking(thinking) => {
                            events.push(AssistantMessageEvent::ThinkingEnd {
                                content_index,
                                content: thinking.thinking.clone(),
                                partial: self.partial.clone(),
                            });
                        }
                        AssistantContent::ToolCall(_) => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(reason) = data
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(|reason| reason.as_str())
                {
                    self.partial.stop_reason = map_stop_reason(reason);
                }
                if let Some(usage) = data.get("usage") {
                    self.update_usage(usage);
                }
            }
            "message_stop" => {
                self.saw_message_stop = true;
                events.push(AssistantMessageEvent::Done {
                    reason: self.partial.stop_reason.clone(),
                    message: self.partial.clone(),
                });
            }
            _ => {}
        }

        events
    }

    fn update_usage(&mut self, usage: &serde_json::Value) {
        if let Some(input) = usage.get("input_tokens").and_then(|input| input.as_u64()) {
            self.partial.usage.input = input as u32;
        }
        if let Some(output) = usage
            .get("output_tokens")
            .and_then(|output| output.as_u64())
        {
            self.partial.usage.output = output as u32;
        }
        if let Some(cache_read) = usage
            .get("cache_read_input_tokens")
            .and_then(|cache_read| cache_read.as_u64())
        {
            self.partial.usage.cache_read = cache_read as u32;
        }
        if let Some(cache_write) = usage
            .get("cache_creation_input_tokens")
            .and_then(|cache_write| cache_write.as_u64())
        {
            self.partial.usage.cache_write = cache_write as u32;
        }
        self.partial.usage.total_tokens = self.partial.usage.input
            + self.partial.usage.output
            + self.partial.usage.cache_read
            + self.partial.usage.cache_write;
        calculate_cost(&self.model, &mut self.partial.usage);
    }

    fn finish_stream(&mut self) -> Option<AssistantMessageEvent> {
        if self.saw_message_start && !self.saw_message_stop {
            self.partial.stop_reason = StopReason::Error;
            self.partial.error_message =
                Some("Anthropic stream ended before message_stop".to_string());
            return Some(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: self.partial.clone(),
            });
        }

        Some(AssistantMessageEvent::Done {
            reason: self.partial.stop_reason.clone(),
            message: self.partial.clone(),
        })
    }
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" | "pause_turn" | "stop_sequence" => StopReason::Stop,
        "max_tokens" => StopReason::Length,
        "tool_use" => StopReason::ToolUse,
        _ => StopReason::Error,
    }
}

fn headers_to_map(headers: &reqwest::header::HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

fn should_retry_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::CONFLICT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn parse_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    if let Some(value) = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        return Some(value);
    }

    let value = headers.get("retry-after")?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds.saturating_mul(1000));
    }

    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .and_then(|date| {
            let now = chrono::Utc::now();
            let target = date.with_timezone(&chrono::Utc);
            if target <= now {
                Some(0)
            } else {
                Some((target - now).num_milliseconds().max(0) as u64)
            }
        })
}

fn retry_delay_ms(
    attempt: u32,
    headers: Option<&reqwest::header::HeaderMap>,
    options: Option<&StreamOptions>,
) -> u64 {
    let fallback = 500_u64.saturating_mul(2_u64.saturating_pow(attempt.saturating_sub(1)));
    let delay = headers.and_then(parse_retry_after_ms).unwrap_or(fallback);
    let cap = options
        .and_then(|options| options.max_retry_delay_ms)
        .unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);
    if cap > 0 { delay.min(cap) } else { delay }
}

fn aborted_message(parser: &AnthropicStreamParser) -> AssistantMessage {
    AssistantMessage {
        error_message: Some("Operation aborted".to_string()),
        stop_reason: StopReason::Aborted,
        ..parser.partial()
    }
}

fn stream_anthropic(
    client: Client,
    model: Model,
    options: Option<StreamOptions>,
    mut body: serde_json::Value,
    tools: Vec<Tool>,
    messages: Vec<Message>,
) -> RawEventStream {
    let on_payload = options
        .as_ref()
        .and_then(|options| options.on_payload.clone());
    let on_response = options
        .as_ref()
        .and_then(|options| options.on_response.clone());
    let signal = options.as_ref().and_then(|options| options.signal.clone());
    let api_key = options
        .as_ref()
        .and_then(|options| options.api_key.clone())
        .unwrap_or_default();
    let is_oauth = api_key.contains("sk-ant-oat");
    let url = if is_cloudflare_ai_gateway(&model) {
        resolve_cloudflare_base_url(&model)
    } else {
        Ok(model.base_url.clone())
    }
    .map(|base_url| format!("{}/v1/messages", base_url.trim_end_matches('/')));
    let compat = AnthropicCompat::resolve(&model);

    let mut headers = reqwest::header::HeaderMap::new();
    if is_cloudflare_ai_gateway(&model) {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}")) {
            headers.insert("cf-aig-authorization", value);
        }
    } else if model.provider == Provider::Known(KnownProvider::GithubCopilot) {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}")) {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
    } else if is_oauth {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}")) {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        headers.insert(
            "user-agent",
            reqwest::header::HeaderValue::from_static("claude-cli/2.1.75"),
        );
        headers.insert("x-app", reqwest::header::HeaderValue::from_static("cli"));
    } else {
        headers.insert(
            "x-api-key",
            reqwest::header::HeaderValue::from_str(&api_key)
                .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("")),
        );
    }

    if model.provider == Provider::Known(KnownProvider::GithubCopilot) {
        for (key, value) in build_copilot_dynamic_headers(&messages) {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                reqwest::header::HeaderValue::from_str(&value),
            ) {
                headers.insert(name, value);
            }
        }
    }
    headers.insert(
        "anthropic-version",
        reqwest::header::HeaderValue::from_static(ANTHROPIC_VERSION),
    );
    headers.insert(
        "Content-Type",
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    let mut beta_features = Vec::new();
    if is_oauth {
        beta_features.push("claude-code-20250219");
        beta_features.push("oauth-2025-04-20");
    }
    let has_tools = body
        .get("tools")
        .and_then(|tools| tools.as_array())
        .is_some_and(|tools| !tools.is_empty());
    if has_tools && !compat.supports_eager_tool_input_streaming {
        beta_features.push(FINE_GRAINED_TOOL_STREAMING_BETA);
    }
    if options
        .as_ref()
        .and_then(|options| options.interleaved_thinking)
        .unwrap_or(true)
        && !compat.force_adaptive_thinking
    {
        beta_features.push(INTERLEAVED_THINKING_BETA);
    }
    if !beta_features.is_empty() {
        headers.insert(
            "anthropic-beta",
            reqwest::header::HeaderValue::from_str(&beta_features.join(","))
                .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("")),
        );
    }

    if let Some(session_id) = options
        .as_ref()
        .and_then(|options| options.session_id.as_ref())
    {
        if compat.send_session_affinity_headers {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(session_id) {
                headers.insert("x-session-affinity", value);
            }
        }
    }

    if let Some(model_headers) = &model.headers {
        for (key, value) in model_headers {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                reqwest::header::HeaderValue::from_str(value),
            ) {
                headers.insert(name, value);
            }
        }
    }
    if let Some(custom_headers) = options
        .as_ref()
        .and_then(|options| options.headers.as_ref())
    {
        for (key, value) in custom_headers {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                reqwest::header::HeaderValue::from_str(value),
            ) {
                headers.insert(name, value);
            }
        }
    }

    let mut parser = AnthropicStreamParser::new(&model, is_oauth, tools);
    Box::pin(stream! {
        let url = match &url {
            Ok(url) => url.clone(),
            Err(error) => {
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: AssistantMessage {
                        error_message: Some(error.clone()),
                        stop_reason: StopReason::Error,
                        ..parser.partial()
                    },
                };
                return;
            }
        };

        if signal.as_ref().is_some_and(|signal| signal.is_cancelled()) {
            yield AssistantMessageEvent::Error {
                reason: StopReason::Aborted,
                error: aborted_message(&parser),
            };
            return;
        }

        if let Some(hook) = on_payload {
            if let Some(modified) = hook(body.clone()).await {
                body = modified;
            }
        }

        if signal.as_ref().is_some_and(|signal| signal.is_cancelled()) {
            yield AssistantMessageEvent::Error {
                reason: StopReason::Aborted,
                error: aborted_message(&parser),
            };
            return;
        }

        let max_retries = options.as_ref().and_then(|options| options.max_retries).unwrap_or(0);
        let mut attempt = 0_u32;
        let response = loop {
            let mut request = client.post(&url).headers(headers.clone()).json(&body);
            if let Some(timeout_ms) = options.as_ref().and_then(|options| options.timeout_ms) {
                request = request.timeout(Duration::from_millis(timeout_ms));
            }

            let response = if let Some(signal) = signal.clone() {
                let send = request.send().fuse();
                futures::pin_mut!(send);
                futures::select! {
                    _ = signal.cancelled().fuse() => {
                        yield AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: aborted_message(&parser),
                        };
                        return;
                    }
                    response = send => response,
                }
            } else {
                request.send().await
            };

            match response {
                Ok(response) if should_retry_status(response.status()) && attempt < max_retries => {
                    attempt += 1;
                    let delay_ms = retry_delay_ms(attempt, Some(response.headers()), options.as_ref());
                    if let Some(signal) = signal.clone() {
                        let delay = futures_timer::Delay::new(Duration::from_millis(delay_ms)).fuse();
                        futures::pin_mut!(delay);
                        futures::select! {
                            _ = signal.cancelled().fuse() => {
                                yield AssistantMessageEvent::Error {
                                    reason: StopReason::Aborted,
                                    error: aborted_message(&parser),
                                };
                                return;
                            }
                            _ = delay => {}
                        }
                    } else {
                        futures_timer::Delay::new(Duration::from_millis(delay_ms)).await;
                    }
                }
                Ok(response) => break response,
                Err(error) if (error.is_timeout() || error.is_connect() || error.is_request()) && attempt < max_retries => {
                    attempt += 1;
                    let delay_ms = retry_delay_ms(attempt, None, options.as_ref());
                    if let Some(signal) = signal.clone() {
                        let delay = futures_timer::Delay::new(Duration::from_millis(delay_ms)).fuse();
                        futures::pin_mut!(delay);
                        futures::select! {
                            _ = signal.cancelled().fuse() => {
                                yield AssistantMessageEvent::Error {
                                    reason: StopReason::Aborted,
                                    error: aborted_message(&parser),
                                };
                                return;
                            }
                            _ = delay => {}
                        }
                    } else {
                        futures_timer::Delay::new(Duration::from_millis(delay_ms)).await;
                    }
                }
                Err(error) => {
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        error: AssistantMessage {
                            error_message: Some(error.to_string()),
                            stop_reason: StopReason::Error,
                            ..parser.partial()
                        },
                    };
                    return;
                }
            }
        };

        if let Some(hook) = on_response {
            hook(ProviderResponse {
                status: response.status().as_u16(),
                headers: headers_to_map(response.headers()),
            }).await;
        }

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            let error_message = if error_body.trim().is_empty() {
                format!("HTTP {}", status.as_u16())
            } else {
                format!("HTTP {}: {}", status.as_u16(), error_body)
            };
            yield AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: AssistantMessage {
                    error_message: Some(error_message),
                    stop_reason: StopReason::Error,
                    ..parser.partial()
                },
            };
            return;
        }

        yield AssistantMessageEvent::Start {
            partial: parser.partial(),
        };

        let event_stream = response.bytes_stream().eventsource();
        futures::pin_mut!(event_stream);
        while let Some(event) = if let Some(signal) = signal.clone() {
            futures::pin_mut!(signal);
            futures::select! {
                _ = signal.cancelled().fuse() => {
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Aborted,
                        error: aborted_message(&parser),
                    };
                    return;
                }
                event = event_stream.next().fuse() => event,
            }
        } else {
            event_stream.next().await
        } {
            match event {
                Ok(event) => {
                    for output in parser.process_data(&event.data) {
                        let done = matches!(output, AssistantMessageEvent::Done { .. });
                        yield output;
                        if done {
                            return;
                        }
                    }
                }
                Err(error) => {
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        error: AssistantMessage {
                            error_message: Some(error.to_string()),
                            stop_reason: StopReason::Error,
                            ..parser.partial()
                        },
                    };
                    return;
                }
            }
        }

        if let Some(output) = parser.finish_stream() {
            yield output;
        }
    })
}

impl ApiProvider for AnthropicApiProvider {
    fn api(&self) -> Api {
        Api::Known(KnownApi::AnthropicMessages)
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
    ) -> AssistantMessageEventStream {
        let model = model.clone();
        let tools = context.tools.clone().unwrap_or_default();
        let messages = context.messages.clone();
        let options = options.cloned();
        let body = self.build_request_body(&model, context, options.as_ref(), None);
        let raw = stream_anthropic(Self::client(), model, options, body, tools, messages);
        AssistantMessageEventStream::from_raw(raw)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        let model = model.clone();
        let mut options = options.cloned();
        if let Some(options) = options.as_mut() {
            if let Some(reasoning) = options.reasoning.clone() {
                options.reasoning = Some(clamp_thinking_level(&model, reasoning));
            }
        }
        let body = self.build_simple_request_body(&model, context, options.as_ref());
        let tools = context.tools.clone().unwrap_or_default();
        let messages = context.messages.clone();
        let raw = stream_anthropic(
            Self::client(),
            model,
            options.map(|options| options.base),
            body,
            tools,
            messages,
        );
        AssistantMessageEventStream::from_raw(raw)
    }
}

pub fn stream_anthropic_public(
    model: &Model,
    context: &Context,
    options: Option<&StreamOptions>,
) -> AssistantMessageEventStream {
    AnthropicApiProvider.stream(model, context, options)
}

pub fn stream_simple_anthropic(
    model: &Model,
    context: &Context,
    options: Option<&SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    AnthropicApiProvider.stream_simple(model, context, options)
}

pub(crate) fn register_anthropic_provider() {
    crate::api_registry::register_api_provider(Arc::new(AnthropicApiProvider));
}

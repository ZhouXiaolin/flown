use crate::api_registry::{ApiProvider, RawEventStream};
use crate::models::{calculate_cost, clamp_thinking_level, transform_messages};
use crate::providers::common::{
    build_copilot_dynamic_headers, is_cloudflare_ai_gateway, is_cloudflare_provider,
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

const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;

#[derive(Debug, Clone)]
struct OpenAiCompletionsCompat {
    supports_store: bool,
    supports_developer_role: bool,
    supports_reasoning_effort: bool,
    supports_usage_in_streaming: bool,
    max_tokens_field: &'static str,
    requires_tool_result_name: bool,
    requires_assistant_after_tool_result: bool,
    requires_thinking_as_text: bool,
    requires_reasoning_content_on_assistant_messages: bool,
    thinking_format: String,
    supports_strict_mode: bool,
    send_session_affinity_headers: bool,
    supports_long_cache_retention: bool,
    cache_control_format: Option<String>,
    open_router_routing: Option<serde_json::Value>,
    vercel_gateway_routing: Option<serde_json::Value>,
    zai_tool_stream: bool,
}

impl OpenAiCompletionsCompat {
    fn resolve(model: &Model) -> Self {
        let detected = Self::detect(model);
        let Some(compat) = model.compat.as_ref() else {
            return detected;
        };

        Self {
            supports_store: compat.supports_store.unwrap_or(detected.supports_store),
            supports_developer_role: compat
                .supports_developer_role
                .unwrap_or(detected.supports_developer_role),
            supports_reasoning_effort: compat
                .supports_reasoning_effort
                .unwrap_or(detected.supports_reasoning_effort),
            supports_usage_in_streaming: compat
                .supports_usage_in_streaming
                .unwrap_or(detected.supports_usage_in_streaming),
            max_tokens_field: match compat.max_tokens_field.as_deref() {
                Some("max_tokens") => "max_tokens",
                Some("max_completion_tokens") => "max_completion_tokens",
                None => detected.max_tokens_field,
                Some(_) => detected.max_tokens_field,
            },
            requires_tool_result_name: compat
                .requires_tool_result_name
                .unwrap_or(detected.requires_tool_result_name),
            requires_assistant_after_tool_result: compat
                .requires_assistant_after_tool_result
                .unwrap_or(detected.requires_assistant_after_tool_result),
            requires_thinking_as_text: compat
                .requires_thinking_as_text
                .unwrap_or(detected.requires_thinking_as_text),
            requires_reasoning_content_on_assistant_messages: compat
                .requires_reasoning_content_on_assistant_messages
                .unwrap_or(detected.requires_reasoning_content_on_assistant_messages),
            thinking_format: compat
                .thinking_format
                .clone()
                .unwrap_or(detected.thinking_format),
            supports_strict_mode: compat
                .supports_strict_mode
                .unwrap_or(detected.supports_strict_mode),
            send_session_affinity_headers: compat
                .send_session_affinity_headers
                .unwrap_or(detected.send_session_affinity_headers),
            supports_long_cache_retention: compat
                .supports_long_cache_retention
                .unwrap_or(detected.supports_long_cache_retention),
            cache_control_format: compat
                .cache_control_format
                .clone()
                .or(detected.cache_control_format),
            open_router_routing: compat
                .open_router_routing
                .clone()
                .or(detected.open_router_routing),
            vercel_gateway_routing: compat
                .vercel_gateway_routing
                .clone()
                .or(detected.vercel_gateway_routing),
            zai_tool_stream: compat.zai_tool_stream.unwrap_or(detected.zai_tool_stream),
        }
    }

    fn detect(model: &Model) -> Self {
        let provider = match &model.provider {
            Provider::Known(provider) => provider.to_string(),
            Provider::Custom(provider) => provider.clone(),
        };
        let base_url = &model.base_url;

        let is_together = provider == "together"
            || base_url.contains("api.together.ai")
            || base_url.contains("api.together.xyz");
        let is_moonshot = provider == "moonshotai"
            || provider == "moonshotai-cn"
            || base_url.contains("api.moonshot.");
        let is_cloudflare_workers_ai =
            provider == "cloudflare-workers-ai" || base_url.contains("api.cloudflare.com");
        let is_cloudflare_ai_gateway =
            provider == "cloudflare-ai-gateway" || base_url.contains("gateway.ai.cloudflare.com");
        let is_zai = provider == "zai" || base_url.contains("api.z.ai");
        let is_grok = provider == "xai" || base_url.contains("api.x.ai");
        let is_deepseek = provider == "deepseek" || base_url.contains("deepseek.com");
        let is_openrouter = provider == "openrouter" || base_url.contains("openrouter.ai");

        let is_non_standard = provider == "cerebras"
            || base_url.contains("cerebras.ai")
            || is_grok
            || is_together
            || base_url.contains("chutes.ai")
            || is_deepseek
            || is_zai
            || is_moonshot
            || provider == "opencode"
            || base_url.contains("opencode.ai")
            || is_cloudflare_workers_ai
            || is_cloudflare_ai_gateway;

        let use_max_tokens = base_url.contains("chutes.ai")
            || is_moonshot
            || is_cloudflare_ai_gateway
            || is_together;

        Self {
            supports_store: !is_non_standard,
            supports_developer_role: !is_non_standard && !is_openrouter,
            supports_reasoning_effort: !is_grok
                && !is_zai
                && !is_moonshot
                && !is_together
                && !is_cloudflare_ai_gateway,
            supports_usage_in_streaming: true,
            max_tokens_field: if use_max_tokens {
                "max_tokens"
            } else {
                "max_completion_tokens"
            },
            requires_tool_result_name: false,
            requires_assistant_after_tool_result: false,
            requires_thinking_as_text: false,
            requires_reasoning_content_on_assistant_messages: is_deepseek,
            thinking_format: if is_deepseek {
                "deepseek"
            } else if is_zai {
                "zai"
            } else if is_together {
                "together"
            } else if is_openrouter {
                "openrouter"
            } else {
                "openai"
            }
            .to_string(),
            supports_strict_mode: !is_moonshot && !is_together && !is_cloudflare_ai_gateway,
            send_session_affinity_headers: false,
            supports_long_cache_retention: !(is_together
                || is_cloudflare_workers_ai
                || is_cloudflare_ai_gateway),
            cache_control_format: if is_openrouter && model.id.starts_with("anthropic/") {
                Some("anthropic".to_string())
            } else {
                None
            },
            open_router_routing: None,
            vercel_gateway_routing: None,
            zai_tool_stream: false,
        }
    }
}

/// OpenAI-compatible chat completions provider
pub struct OpenAiCompletionsProvider {
    client: Client,
}

impl OpenAiCompletionsProvider {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    fn build_request_body(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
    ) -> serde_json::Value {
        self.build_request_body_with_reasoning(model, context, options, None)
    }

    fn build_simple_request_body(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
    ) -> serde_json::Value {
        self.build_request_body_with_reasoning(
            model,
            context,
            options.map(|options| &options.base),
            options.and_then(|options| options.reasoning.as_ref()),
        )
    }

    fn build_request_body_with_reasoning(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        reasoning: Option<&ThinkingLevel>,
    ) -> serde_json::Value {
        let compat = OpenAiCompletionsCompat::resolve(model);
        let mut body = serde_json::json!({
            "model": model.id,
            "stream": true,
        });

        // Add messages
        let messages = self.convert_messages(model, context, &compat);
        body["messages"] = serde_json::json!(messages);

        // Add tools if present
        if let Some(tools) = &context.tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(convert_tools(tools, &compat));
                if compat.zai_tool_stream {
                    body["tool_stream"] = serde_json::json!(true);
                }
            } else if has_tool_history(&context.messages) {
                body["tools"] = serde_json::json!([]);
            }
        } else if has_tool_history(&context.messages) {
            body["tools"] = serde_json::json!([]);
        }

        // Apply options
        if let Some(opts) = options {
            if let Some(temp) = opts.temperature {
                body["temperature"] = serde_json::json!(temp);
            }
            if let Some(max_tokens) = opts.max_tokens {
                body[compat.max_tokens_field] = serde_json::json!(max_tokens);
            }
        }

        if compat.supports_usage_in_streaming {
            body["stream_options"] = serde_json::json!({ "include_usage": true });
        }

        if compat.supports_store {
            body["store"] = serde_json::json!(false);
        }

        let cache_retention = resolve_cache_retention(options);
        if ((model.base_url.contains("api.openai.com") && cache_retention != CacheRetention::None)
            || (cache_retention == CacheRetention::Long && compat.supports_long_cache_retention))
            && let Some(session_id) = options.and_then(|opts| opts.session_id.as_ref())
        {
            body["prompt_cache_key"] = serde_json::json!(clamp_prompt_cache_key(session_id));
        }
        if cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
            body["prompt_cache_retention"] = serde_json::json!("24h");
        }

        if let Some(tool_choice) = options.and_then(|opts| opts.tool_choice.as_ref()) {
            body["tool_choice"] = tool_choice.clone();
        }

        apply_reasoning_params(&mut body, model, &compat, reasoning);
        apply_openai_routing(&mut body, model, &compat);
        if compat.cache_control_format.as_deref() == Some("anthropic") {
            if cache_retention != CacheRetention::None {
                apply_anthropic_cache_control(&mut body, &cache_retention, &compat);
            }
        }

        body
    }

    fn convert_messages(
        &self,
        model: &Model,
        context: &Context,
        compat: &OpenAiCompletionsCompat,
    ) -> Vec<serde_json::Value> {
        let mut messages = Vec::new();
        let transformed_messages = transform_messages(
            &context.messages,
            model,
            Some(&|id, model, _source| normalize_openai_tool_call_id(id, model)),
        );

        // Add system prompt if present
        if let Some(system) = &context.system_prompt {
            let role = if model.reasoning && compat.supports_developer_role {
                "developer"
            } else {
                "system"
            };
            messages.push(serde_json::json!({
                "role": role,
                "content": system,
            }));
        }

        // Convert messages
        let mut last_role: Option<&str> = None;
        let mut index = 0;
        while index < transformed_messages.len() {
            let msg = &transformed_messages[index];
            if compat.requires_assistant_after_tool_result
                && last_role == Some("toolResult")
                && matches!(msg, Message::User(_))
            {
                messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": "I have processed the tool results.",
                }));
            }

            match msg {
                Message::User(user) => {
                    let content = match &user.content {
                        MessageContent::Text(text) => serde_json::json!(text),
                        MessageContent::Blocks(blocks) => {
                            let content_blocks: Vec<serde_json::Value> = blocks
                                .iter()
                                .map(|block| match block {
                                    UserContentBlock::Text(text) => {
                                        serde_json::json!({
                                            "type": "text",
                                            "text": text.text,
                                        })
                                    }
                                    UserContentBlock::Image(img) => {
                                        serde_json::json!({
                                            "type": "image_url",
                                            "image_url": {
                                                "url": format!("data:{};base64,{}", img.mime_type, img.data),
                                            }
                                        })
                                    }
                                })
                                .collect();
                            serde_json::json!(content_blocks)
                        }
                    };
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": content,
                    }));
                    last_role = Some("user");
                }
                Message::Assistant(assistant) => {
                    let mut content_parts = Vec::new();
                    let mut tool_calls = Vec::new();
                    let mut reasoning_content = String::new();
                    let mut reasoning_details = Vec::new();

                    for block in &assistant.content {
                        match block {
                            AssistantContent::Text(text) if !text.text.trim().is_empty() => {
                                content_parts.push(serde_json::json!({
                                    "type": "text",
                                    "text": text.text,
                                }));
                            }
                            AssistantContent::Text(_) => {}
                            AssistantContent::Thinking(thinking)
                                if !thinking.thinking.trim().is_empty() =>
                            {
                                // DeepSeek uses reasoning_content for thinking
                                if compat.requires_thinking_as_text {
                                    content_parts.push(serde_json::json!({
                                        "type": "text",
                                        "text": thinking.thinking,
                                    }));
                                } else {
                                    reasoning_content.push_str(&thinking.thinking);
                                }
                            }
                            AssistantContent::Thinking(_) => {}
                            AssistantContent::ToolCall(tc) => {
                                tool_calls.push(serde_json::json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": {
                                        "name": tc.name,
                                        "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default(),
                                    }
                                }));
                                if let Some(signature) = &tc.thought_signature {
                                    if let Ok(detail) =
                                        serde_json::from_str::<serde_json::Value>(signature)
                                    {
                                        reasoning_details.push(detail);
                                    }
                                }
                            }
                        }
                    }

                    let mut msg = serde_json::json!({
                        "role": "assistant",
                    });

                    if !reasoning_content.is_empty() {
                        msg["reasoning_content"] = serde_json::json!(reasoning_content);
                    } else if compat.requires_reasoning_content_on_assistant_messages
                        && model.reasoning
                    {
                        msg["reasoning_content"] = serde_json::json!("");
                    }

                    if !tool_calls.is_empty() {
                        msg["tool_calls"] = serde_json::json!(tool_calls);
                        if !reasoning_details.is_empty() {
                            msg["reasoning_details"] = serde_json::json!(reasoning_details);
                        }
                        // When there are tool calls, content should be a simple string
                        if !content_parts.is_empty() {
                            // Concatenate text parts into a single string
                            let text: String = content_parts
                                .iter()
                                .filter_map(|p| p["text"].as_str())
                                .collect();
                            msg["content"] = serde_json::json!(text);
                        } else {
                            msg["content"] = serde_json::json!("");
                        }
                    } else if !content_parts.is_empty() {
                        if content_parts.len() == 1 {
                            msg["content"] = content_parts[0]["text"].clone();
                        } else {
                            msg["content"] = serde_json::json!(content_parts);
                        }
                    }

                    let has_content = msg.get("content").is_some_and(|content| match content {
                        serde_json::Value::String(text) => !text.is_empty(),
                        serde_json::Value::Array(parts) => !parts.is_empty(),
                        serde_json::Value::Null => false,
                        _ => true,
                    });
                    if has_content || !tool_calls.is_empty() {
                        messages.push(msg);
                        last_role = Some("assistant");
                    }
                }
                Message::ToolResult(_) => {
                    let mut image_blocks = Vec::new();
                    let mut cursor = index;
                    while cursor < transformed_messages.len() {
                        let Message::ToolResult(result) = &transformed_messages[cursor] else {
                            break;
                        };

                        let text_result = result
                            .content
                            .iter()
                            .filter_map(|content| match content {
                                ToolResultContent::Text(text) => Some(text.text.as_str()),
                                ToolResultContent::Image(_) => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        let has_images = result
                            .content
                            .iter()
                            .any(|content| matches!(content, ToolResultContent::Image(_)));
                        let mut tool_message = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": result.tool_call_id,
                            "content": if text_result.is_empty() && has_images {
                                "(see attached image)"
                            } else {
                                text_result.as_str()
                            },
                        });
                        if compat.requires_tool_result_name && !result.tool_name.is_empty() {
                            tool_message["name"] = serde_json::json!(result.tool_name);
                        }
                        messages.push(tool_message);

                        if has_images && model.input.iter().any(|input| input == "image") {
                            for image in result.content.iter().filter_map(|content| match content {
                                ToolResultContent::Image(image) => Some(image),
                                ToolResultContent::Text(_) => None,
                            }) {
                                image_blocks.push(serde_json::json!({
                                    "type": "image_url",
                                    "image_url": {
                                        "url": format!("data:{};base64,{}", image.mime_type, image.data),
                                    },
                                }));
                            }
                        }

                        cursor += 1;
                    }

                    index = cursor.saturating_sub(1);

                    if !image_blocks.is_empty() {
                        if compat.requires_assistant_after_tool_result {
                            messages.push(serde_json::json!({
                                "role": "assistant",
                                "content": "I have processed the tool results.",
                            }));
                        }

                        let mut content = vec![serde_json::json!({
                            "type": "text",
                            "text": "Attached image(s) from tool result:",
                        })];
                        content.extend(image_blocks);
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": content,
                        }));
                        last_role = Some("user");
                    } else {
                        last_role = Some("toolResult");
                    }
                }
            }
            index += 1;
        }

        messages
    }
}

fn clamp_prompt_cache_key(session_id: &str) -> String {
    session_id.chars().take(64).collect()
}

fn resolve_cache_retention(options: Option<&StreamOptions>) -> CacheRetention {
    resolve_common_cache_retention(options.and_then(|options| options.cache_retention.as_ref()))
}

fn normalize_openai_tool_call_id(id: &str, model: &Model) -> String {
    if let Some((call_id, _)) = id.split_once('|') {
        return call_id
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                    ch
                } else {
                    '_'
                }
            })
            .take(40)
            .collect();
    }

    if model.provider == Provider::Known(KnownProvider::OpenAi) {
        return id.chars().take(40).collect();
    }

    id.to_string()
}

fn has_tool_history(messages: &[Message]) -> bool {
    messages.iter().any(|message| match message {
        Message::Assistant(assistant) => assistant
            .content
            .iter()
            .any(|content| matches!(content, AssistantContent::ToolCall(_))),
        Message::ToolResult(_) => true,
        Message::User(_) => false,
    })
}

fn convert_tools(tools: &[Tool], compat: &OpenAiCompletionsCompat) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|tool| {
            let mut function = serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            });
            if compat.supports_strict_mode {
                function["strict"] = serde_json::json!(false);
            }

            serde_json::json!({
                "type": "function",
                "function": function,
            })
        })
        .collect()
}

fn thinking_level_name(level: &ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::XHigh => "xhigh",
    }
}

fn mapped_thinking_level(model: &Model, level: &ThinkingLevel) -> String {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(level))
        .and_then(|mapped| mapped.clone())
        .unwrap_or_else(|| thinking_level_name(level).to_string())
}

fn apply_reasoning_params(
    body: &mut serde_json::Value,
    model: &Model,
    compat: &OpenAiCompletionsCompat,
    reasoning: Option<&ThinkingLevel>,
) {
    if !model.reasoning {
        return;
    }

    let reasoning = reasoning.filter(|level| **level != ThinkingLevel::Off);
    if compat.thinking_format == "deepseek" {
        body["thinking"] = serde_json::json!({
            "type": if reasoning.is_some() { "enabled" } else { "disabled" },
        });
        if let Some(level) = reasoning {
            body["reasoning_effort"] = serde_json::json!(mapped_thinking_level(model, level));
        }
    } else if compat.thinking_format == "zai" || compat.thinking_format == "qwen" {
        body["enable_thinking"] = serde_json::json!(reasoning.is_some());
    } else if compat.thinking_format == "qwen-chat-template" {
        body["chat_template_kwargs"] = serde_json::json!({
            "enable_thinking": reasoning.is_some(),
            "preserve_thinking": true,
        });
    } else if compat.thinking_format == "openrouter" {
        if let Some(level) = reasoning {
            body["reasoning"] = serde_json::json!({
                "effort": mapped_thinking_level(model, level),
            });
        } else if model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&ThinkingLevel::Off))
            .is_none_or(|mapped| mapped.is_some())
        {
            body["reasoning"] = serde_json::json!({
                "effort": model
                    .thinking_level_map
                    .as_ref()
                    .and_then(|map| map.get(&ThinkingLevel::Off))
                    .and_then(|mapped| mapped.clone())
                    .unwrap_or_else(|| "none".to_string()),
            });
        }
    } else if compat.thinking_format == "together" {
        body["reasoning"] = serde_json::json!({ "enabled": reasoning.is_some() });
        if let Some(level) = reasoning {
            if compat.supports_reasoning_effort {
                body["reasoning_effort"] = serde_json::json!(mapped_thinking_level(model, level));
            }
        }
    } else if compat.thinking_format == "string-thinking" {
        if let Some(level) = reasoning {
            body["thinking"] = serde_json::json!(mapped_thinking_level(model, level));
        } else if model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&ThinkingLevel::Off))
            .is_none_or(|mapped| mapped.is_some())
        {
            body["thinking"] = serde_json::json!(
                model
                    .thinking_level_map
                    .as_ref()
                    .and_then(|map| map.get(&ThinkingLevel::Off))
                    .and_then(|mapped| mapped.clone())
                    .unwrap_or_else(|| "none".to_string())
            );
        }
    } else if let Some(level) = reasoning {
        if compat.supports_reasoning_effort {
            body["reasoning_effort"] = serde_json::json!(mapped_thinking_level(model, level));
        }
    } else if compat.supports_reasoning_effort {
        if let Some(off_value) = model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&ThinkingLevel::Off))
            .and_then(|mapped| mapped.clone())
        {
            body["reasoning_effort"] = serde_json::json!(off_value);
        }
    }
}

fn apply_openai_routing(
    body: &mut serde_json::Value,
    model: &Model,
    compat: &OpenAiCompletionsCompat,
) {
    if model.base_url.contains("openrouter.ai") {
        if let Some(routing) = &compat.open_router_routing {
            body["provider"] = routing.clone();
        }
    }

    if model.base_url.contains("ai-gateway.vercel.sh") {
        let Some(routing) = &compat.vercel_gateway_routing else {
            return;
        };
        let mut gateway = serde_json::Map::new();
        if let Some(only) = routing.get("only") {
            gateway.insert("only".to_string(), only.clone());
        }
        if let Some(order) = routing.get("order") {
            gateway.insert("order".to_string(), order.clone());
        }
        if !gateway.is_empty() {
            body["providerOptions"] = serde_json::json!({ "gateway": gateway });
        }
    }
}

fn anthropic_cache_control(
    retention: &CacheRetention,
    compat: &OpenAiCompletionsCompat,
) -> serde_json::Value {
    let mut cache = serde_json::json!({ "type": "ephemeral" });
    if *retention == CacheRetention::Long && compat.supports_long_cache_retention {
        cache["ttl"] = serde_json::json!("1h");
    }
    cache
}

fn add_cache_control_to_text_content(
    value: &mut serde_json::Value,
    cache_control: &serde_json::Value,
) -> bool {
    if let Some(text) = value.as_str() {
        if text.is_empty() {
            return false;
        }
        *value = serde_json::json!([{
            "type": "text",
            "text": text,
            "cache_control": cache_control,
        }]);
        return true;
    }

    let Some(parts) = value.as_array_mut() else {
        return false;
    };
    for part in parts.iter_mut().rev() {
        if part.get("type").and_then(|kind| kind.as_str()) == Some("text") {
            part["cache_control"] = cache_control.clone();
            return true;
        }
    }
    false
}

fn apply_anthropic_cache_control(
    body: &mut serde_json::Value,
    retention: &CacheRetention,
    compat: &OpenAiCompletionsCompat,
) {
    let cache_control = anthropic_cache_control(retention, compat);
    if let Some(messages) = body
        .get_mut("messages")
        .and_then(|messages| messages.as_array_mut())
    {
        for message in messages.iter_mut() {
            if matches!(
                message.get("role").and_then(|role| role.as_str()),
                Some("system" | "developer")
            ) {
                if add_cache_control_to_text_content(&mut message["content"], &cache_control) {
                    break;
                }
            }
        }

        for message in messages.iter_mut().rev() {
            if matches!(
                message.get("role").and_then(|role| role.as_str()),
                Some("user" | "assistant")
            ) && add_cache_control_to_text_content(&mut message["content"], &cache_control)
            {
                break;
            }
        }
    }

    if let Some(tools) = body.get_mut("tools").and_then(|tools| tools.as_array_mut()) {
        if let Some(last_tool) = tools.last_mut() {
            last_tool["cache_control"] = cache_control;
        }
    }
}

struct OpenAiCompletionsStreamParser {
    model: Model,
    partial: AssistantMessage,
    has_finish_reason: bool,
    text_index: Option<usize>,
    thinking_index: Option<usize>,
    tool_by_stream_index: HashMap<usize, usize>,
    tool_by_id: HashMap<String, usize>,
    tool_args: HashMap<usize, String>,
}

impl OpenAiCompletionsStreamParser {
    fn new(model: &Model) -> Self {
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
                timestamp: chrono::Utc::now(),
            },
            has_finish_reason: false,
            text_index: None,
            thinking_index: None,
            tool_by_stream_index: HashMap::new(),
            tool_by_id: HashMap::new(),
            tool_args: HashMap::new(),
        }
    }

    fn partial(&self) -> AssistantMessage {
        self.partial.clone()
    }

    fn process_data(&mut self, data: &str) -> Vec<AssistantMessageEvent> {
        if data == "[DONE]" {
            return self.finish_stream();
        }

        let data: serde_json::Value = match serde_json::from_str(data) {
            Ok(data) => data,
            Err(_) => return Vec::new(),
        };

        if let Some(id) = data.get("id").and_then(|id| id.as_str()) {
            if !id.is_empty() {
                self.partial
                    .response_id
                    .get_or_insert_with(|| id.to_string());
            }
        }
        if let Some(response_model) = data.get("model").and_then(|model| model.as_str()) {
            if !response_model.is_empty() && response_model != self.partial.model {
                self.partial
                    .response_model
                    .get_or_insert_with(|| response_model.to_string());
            }
        }
        if let Some(usage) = data.get("usage") {
            self.update_usage(usage);
        }

        let mut events = Vec::new();
        let Some(choice) = data
            .get("choices")
            .and_then(|choices| choices.as_array())
            .and_then(|choices| choices.first())
        else {
            return events;
        };

        if data.get("usage").is_none() {
            if let Some(usage) = choice.get("usage") {
                self.update_usage(usage);
            }
        }

        if let Some(delta) = choice.get("delta") {
            if let Some(content) = delta.get("content").and_then(|content| content.as_str()) {
                if !content.is_empty() {
                    self.push_text_delta(content, &mut events);
                }
            }

            if let Some(reasoning) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .or_else(|| delta.get("reasoning_text"))
                .and_then(|reasoning| reasoning.as_str())
            {
                if !reasoning.is_empty() {
                    self.push_thinking_delta(reasoning, &mut events);
                }
            }

            if let Some(tool_calls) = delta.get("tool_calls").and_then(|calls| calls.as_array()) {
                for tool_call in tool_calls {
                    self.push_tool_call_delta(tool_call, &mut events);
                }
            }

            if let Some(reasoning_details) = delta
                .get("reasoning_details")
                .and_then(|details| details.as_array())
            {
                self.apply_reasoning_details(reasoning_details);
            }
        }

        if let Some(finish_reason) = choice
            .get("finish_reason")
            .and_then(|reason| reason.as_str())
        {
            self.has_finish_reason = true;
            self.partial.stop_reason = match finish_reason {
                "stop" | "end" => StopReason::Stop,
                "length" => StopReason::Length,
                "function_call" | "tool_calls" => StopReason::ToolUse,
                _ => {
                    self.partial.error_message =
                        Some(format!("Provider finish_reason: {finish_reason}"));
                    StopReason::Error
                }
            };
        }

        events
    }

    fn update_usage(&mut self, usage: &serde_json::Value) {
        let prompt_tokens = usage
            .get("prompt_tokens")
            .and_then(|u| u.as_u64())
            .unwrap_or(0) as u32;
        let output = usage
            .get("completion_tokens")
            .and_then(|u| u.as_u64())
            .unwrap_or(0) as u32;
        let cache_read = usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .or_else(|| usage.get("prompt_cache_hit_tokens"))
            .and_then(|u| u.as_u64())
            .unwrap_or(0) as u32;
        let cache_write = usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cache_write_tokens"))
            .or_else(|| usage.get("prompt_cache_miss_tokens"))
            .and_then(|u| u.as_u64())
            .unwrap_or(0) as u32;
        let input = prompt_tokens.saturating_sub(cache_read + cache_write);

        self.partial.usage = Usage {
            input,
            output,
            cache_read,
            cache_write,
            total_tokens: input + output + cache_read + cache_write,
            cost: Cost::default(),
        };
        calculate_cost(&self.model, &mut self.partial.usage);
    }

    fn apply_reasoning_details(&mut self, reasoning_details: &[serde_json::Value]) {
        for detail in reasoning_details {
            if detail.get("type").and_then(|kind| kind.as_str()) != Some("reasoning.encrypted") {
                continue;
            }
            let Some(id) = detail.get("id").and_then(|id| id.as_str()) else {
                continue;
            };
            if detail.get("data").is_none() {
                continue;
            }
            let Some(idx) = self.tool_by_id.get(id).copied() else {
                continue;
            };
            if let Some(AssistantContent::ToolCall(tool_call)) = self.partial.content.get_mut(idx) {
                tool_call.thought_signature = serde_json::to_string(detail).ok();
            }
        }
    }

    fn push_text_delta(&mut self, delta: &str, events: &mut Vec<AssistantMessageEvent>) {
        let idx = match self.text_index {
            Some(idx) => idx,
            None => {
                let idx = self.partial.content.len();
                self.partial
                    .content
                    .push(AssistantContent::Text(TextContent {
                        content_type: "text".to_string(),
                        text: String::new(),
                        text_signature: None,
                    }));
                self.text_index = Some(idx);
                events.push(AssistantMessageEvent::TextStart {
                    content_index: idx,
                    partial: self.partial.clone(),
                });
                idx
            }
        };

        if let Some(AssistantContent::Text(text)) = self.partial.content.get_mut(idx) {
            text.text.push_str(delta);
        }
        events.push(AssistantMessageEvent::TextDelta {
            content_index: idx,
            delta: delta.to_string(),
            partial: self.partial.clone(),
        });
    }

    fn push_thinking_delta(&mut self, delta: &str, events: &mut Vec<AssistantMessageEvent>) {
        let idx = match self.thinking_index {
            Some(idx) => idx,
            None => {
                let idx = self.partial.content.len();
                self.partial
                    .content
                    .push(AssistantContent::Thinking(ThinkingContent {
                        content_type: "thinking".to_string(),
                        thinking: String::new(),
                        thinking_signature: Some("reasoning_content".to_string()),
                        redacted: None,
                    }));
                self.thinking_index = Some(idx);
                events.push(AssistantMessageEvent::ThinkingStart {
                    content_index: idx,
                    partial: self.partial.clone(),
                });
                idx
            }
        };

        if let Some(AssistantContent::Thinking(thinking)) = self.partial.content.get_mut(idx) {
            thinking.thinking.push_str(delta);
        }
        events.push(AssistantMessageEvent::ThinkingDelta {
            content_index: idx,
            delta: delta.to_string(),
            partial: self.partial.clone(),
        });
    }

    fn push_tool_call_delta(
        &mut self,
        tool_call: &serde_json::Value,
        events: &mut Vec<AssistantMessageEvent>,
    ) {
        let stream_index = tool_call
            .get("index")
            .and_then(|index| index.as_u64())
            .map(|index| index as usize);
        let id = tool_call.get("id").and_then(|id| id.as_str()).unwrap_or("");
        let function = tool_call
            .get("function")
            .unwrap_or(&serde_json::Value::Null);
        let name = function
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or("");
        let delta = function
            .get("arguments")
            .and_then(|arguments| arguments.as_str())
            .unwrap_or("");

        let idx = stream_index
            .and_then(|stream_index| self.tool_by_stream_index.get(&stream_index).copied())
            .or_else(|| self.tool_by_id.get(id).copied())
            .unwrap_or_else(|| {
                let idx = self.partial.content.len();
                self.partial
                    .content
                    .push(AssistantContent::ToolCall(ToolCall {
                        content_type: "toolCall".to_string(),
                        id: id.to_string(),
                        name: name.to_string(),
                        arguments: serde_json::json!({}),
                        thought_signature: None,
                    }));
                if let Some(stream_index) = stream_index {
                    self.tool_by_stream_index.insert(stream_index, idx);
                }
                if !id.is_empty() {
                    self.tool_by_id.insert(id.to_string(), idx);
                }
                self.tool_args.insert(idx, String::new());
                events.push(AssistantMessageEvent::ToolCallStart {
                    content_index: idx,
                    partial: self.partial.clone(),
                });
                idx
            });

        if let Some(stream_index) = stream_index {
            self.tool_by_stream_index.insert(stream_index, idx);
        }
        if !id.is_empty() {
            self.tool_by_id.insert(id.to_string(), idx);
        }

        if let Some(AssistantContent::ToolCall(call)) = self.partial.content.get_mut(idx) {
            if call.id.is_empty() && !id.is_empty() {
                call.id = id.to_string();
            }
            if call.name.is_empty() && !name.is_empty() {
                call.name = name.to_string();
            }
            let args = self.tool_args.entry(idx).or_default();
            args.push_str(delta);
            call.arguments = parse_streaming_json(args);
        }

        events.push(AssistantMessageEvent::ToolCallDelta {
            content_index: idx,
            delta: delta.to_string(),
            partial: self.partial.clone(),
        });
    }

    fn finish_stream(&mut self) -> Vec<AssistantMessageEvent> {
        let mut events = Vec::new();

        if let Some(idx) = self.text_index.take() {
            if let Some(AssistantContent::Text(text)) = self.partial.content.get(idx) {
                events.push(AssistantMessageEvent::TextEnd {
                    content_index: idx,
                    content: text.text.clone(),
                    partial: self.partial.clone(),
                });
            }
        }

        if let Some(idx) = self.thinking_index.take() {
            if let Some(AssistantContent::Thinking(thinking)) = self.partial.content.get(idx) {
                events.push(AssistantMessageEvent::ThinkingEnd {
                    content_index: idx,
                    content: thinking.thinking.clone(),
                    partial: self.partial.clone(),
                });
            }
        }

        let mut tool_indices: Vec<_> = self.tool_args.keys().copied().collect();
        tool_indices.sort_unstable();
        for idx in tool_indices {
            if let Some(raw_args) = self.tool_args.remove(&idx) {
                if let Some(AssistantContent::ToolCall(tool_call)) =
                    self.partial.content.get_mut(idx)
                {
                    tool_call.arguments = parse_streaming_json(&raw_args);
                    let tool_call = tool_call.clone();
                    events.push(AssistantMessageEvent::ToolCallEnd {
                        content_index: idx,
                        tool_call,
                        partial: self.partial.clone(),
                    });
                }
            }
        }

        if !self.has_finish_reason {
            self.partial.stop_reason = StopReason::Error;
            self.partial.error_message = Some("Stream ended without finish_reason".to_string());
            events.push(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: self.partial.clone(),
            });
            return events;
        }

        if self.partial.stop_reason == StopReason::Error {
            events.push(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: self.partial.clone(),
            });
            return events;
        }

        events.push(AssistantMessageEvent::Done {
            reason: self.partial.stop_reason.clone(),
            message: self.partial.clone(),
        });
        events
    }
}

fn aborted_message(parser: &OpenAiCompletionsStreamParser) -> AssistantMessage {
    AssistantMessage {
        error_message: Some("Operation aborted".to_string()),
        stop_reason: StopReason::Aborted,
        ..parser.partial()
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

fn stream_openai_completions(
    client: Client,
    model: Model,
    options: Option<StreamOptions>,
    mut body: serde_json::Value,
    messages: Vec<Message>,
) -> RawEventStream {
    let on_payload = options.as_ref().and_then(|o| o.on_payload.clone());
    let on_response = options.as_ref().and_then(|o| o.on_response.clone());
    let signal = options.as_ref().and_then(|o| o.signal.clone());
    let api_key = options
        .as_ref()
        .and_then(|o| o.api_key.clone())
        .unwrap_or_default();
    let url = if is_cloudflare_provider(&model) {
        resolve_cloudflare_base_url(&model)
    } else {
        Ok(model.base_url.clone())
    }
    .map(|base_url| format!("{}/chat/completions", base_url.trim_end_matches('/')));
    let compat = OpenAiCompletionsCompat::resolve(&model);

    let mut headers = reqwest::header::HeaderMap::new();
    if !is_cloudflare_ai_gateway(&model) {
        headers.insert(
            "Authorization",
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", api_key))
                .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("")),
        );
    }
    headers.insert(
        "Content-Type",
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    if let Some(model_headers) = &model.headers {
        for (key, value) in model_headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                reqwest::header::HeaderValue::from_str(value),
            ) {
                headers.insert(name, val);
            }
        }
    }

    if model.provider == Provider::Known(KnownProvider::GithubCopilot) {
        for (key, value) in build_copilot_dynamic_headers(&messages) {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                reqwest::header::HeaderValue::from_str(&value),
            ) {
                headers.insert(name, val);
            }
        }
    }

    if let Some(session_id) = options.as_ref().and_then(|opts| opts.session_id.as_ref()) {
        if compat.send_session_affinity_headers {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(session_id) {
                headers.insert("session_id", value.clone());
                headers.insert("x-client-request-id", value.clone());
                headers.insert("x-session-affinity", value);
            }
        }
    }

    if let Some(custom_headers) = options.as_ref().and_then(|opts| opts.headers.as_ref()) {
        for (key, value) in custom_headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                reqwest::header::HeaderValue::from_str(value),
            ) {
                headers.insert(name, val);
            }
        }
    }

    if is_cloudflare_ai_gateway(&model) {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", api_key)) {
            headers.insert("cf-aig-authorization", value);
        }
    }

    let mut parser = OpenAiCompletionsStreamParser::new(&model);

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

        let max_retries = options.as_ref().and_then(|opts| opts.max_retries).unwrap_or(0);
        let mut attempt = 0_u32;
        let response = loop {
            let mut request = client
                .post(&url)
                .headers(headers.clone())
                .json(&body);
            if let Some(timeout_ms) = options.as_ref().and_then(|opts| opts.timeout_ms) {
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
                Err(e) if (e.is_timeout() || e.is_connect() || e.is_request()) && attempt < max_retries => {
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
                Err(e) => {
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        error: AssistantMessage {
                            error_message: Some(e.to_string()),
                            stop_reason: StopReason::Error,
                            ..parser.partial()
                        },
                    };
                    return;
                }
            }
        };

        if let Some(hook) = on_response {
            let response_info = ProviderResponse {
                status: response.status().as_u16(),
                headers: headers_to_map(response.headers()),
            };
            hook(response_info).await;
        }

        if !response.status().is_success() {
            let status = response.status();
            let mut error_message = format!("HTTP {}", status.as_u16());
            if let Ok(bytes) = response.bytes().await {
                if let Ok(body) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    // OpenRouter-style error: error.metadata.raw
                    if let Some(raw) = body
                        .get("error")
                        .and_then(|e| e.get("metadata"))
                        .and_then(|m| m.get("raw"))
                        .and_then(|r| r.as_str())
                    {
                        error_message.push_str("\n");
                        error_message.push_str(raw);
                    } else if let Some(msg) = body
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                    {
                        error_message.push_str(": ");
                        error_message.push_str(msg);
                    }
                }
            }
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

        loop {
            let event = if let Some(signal) = signal.clone() {
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
            };

            let Some(event) = event else {
                break;
            };

            match event {
                Ok(event) => {
                    for msg_event in parser.process_data(&event.data) {
                        match &msg_event {
                            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => {
                                yield msg_event;
                                return;
                            }
                            _ => {
                                yield msg_event;
                            }
                        }
                    }
                }
                Err(e) => {
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        error: AssistantMessage {
                            error_message: Some(e.to_string()),
                            stop_reason: StopReason::Error,
                            ..parser.partial()
                        },
                    };
                    return;
                }
            }
        }

        for msg_event in parser.finish_stream() {
            yield msg_event;
        }
    })
}

impl ApiProvider for OpenAiCompletionsProvider {
    fn api(&self) -> Api {
        Api::Known(KnownApi::OpenAiCompletions)
    }

    fn stream(
        &self,
        model: &Model,
        context: &crate::types::Context,
        options: Option<&StreamOptions>,
    ) -> RawEventStream {
        let client = self.client.clone();
        let model = model.clone();
        let context = context.clone();
        let options = options.cloned();
        let body = self.build_request_body(&model, &context, options.as_ref());
        let messages = context.messages.clone();

        stream_openai_completions(client, model, options, body, messages)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &crate::types::Context,
        options: Option<&SimpleStreamOptions>,
    ) -> RawEventStream {
        let client = self.client.clone();
        let model = model.clone();
        let mut options = options.cloned();
        if let Some(options) = options.as_mut() {
            if let Some(reasoning) = options.reasoning.clone() {
                options.reasoning = Some(clamp_thinking_level(&model, reasoning));
            }
        }
        let body = self.build_simple_request_body(&model, context, options.as_ref());
        let messages = context.messages.clone();

        stream_openai_completions(
            client,
            model,
            options.map(|options| options.base),
            body,
            messages,
        )
    }
}

/// Register the openai-completions provider
pub fn register_openai_completions_provider() {
    let provider = Arc::new(OpenAiCompletionsProvider::new());
    crate::api_registry::register_api_provider(provider);
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn restore_env(key: &str, previous: Option<String>) {
        unsafe {
            if let Some(previous) = previous {
                std::env::set_var(key, previous);
            } else {
                std::env::remove_var(key);
            }
        }
    }

    fn has_header_line(headers: &str, expected: &str) -> bool {
        headers.lines().any(|line| line == expected)
    }

    fn test_model() -> Model {
        Model {
            id: "deepseek-v4-flash".to_string(),
            name: "DeepSeek V4 Flash".to_string(),
            api: Api::Known(KnownApi::OpenAiCompletions),
            provider: Provider::Known(KnownProvider::Deepseek),
            base_url: "https://api.deepseek.com".to_string(),
            reasoning: true,
            thinking_level_map: Some({
                let mut map = HashMap::new();
                map.insert(ThinkingLevel::Minimal, None);
                map.insert(ThinkingLevel::Low, None);
                map.insert(ThinkingLevel::Medium, None);
                map.insert(ThinkingLevel::High, Some("high".to_string()));
                map.insert(ThinkingLevel::XHigh, Some("max".to_string()));
                map
            }),
            input: vec!["text".to_string()],
            cost: ModelCost {
                input: 0.14,
                output: 0.28,
                cache_read: 0.0028,
                cache_write: 0.0,
            },
            context_window: 128_000,
            max_tokens: 4096,
            headers: None,
            compat: Some(Compat {
                supports_store: None,
                supports_developer_role: None,
                supports_reasoning_effort: None,
                supports_usage_in_streaming: Some(true),
                max_tokens_field: None,
                requires_tool_result_name: Some(false),
                requires_assistant_after_tool_result: Some(false),
                requires_thinking_as_text: None,
                requires_reasoning_content_on_assistant_messages: Some(true),
                thinking_format: Some("deepseek".to_string()),
                supports_strict_mode: None,
                cache_control_format: None,
                send_session_affinity_headers: None,
                supports_long_cache_retention: None,
                open_router_routing: None,
                vercel_gateway_routing: None,
                zai_tool_stream: None,
                supports_eager_tool_input_streaming: None,
                supports_cache_control_on_tools: None,
                force_adaptive_thinking: None,
            }),
        }
    }

    fn chunk(delta: serde_json::Value, finish_reason: Option<&str>) -> String {
        serde_json::json!({
            "id": "response-1",
            "model": "deepseek-v4-flash",
            "choices": [{
                "delta": delta,
                "finish_reason": finish_reason,
            }],
        })
        .to_string()
    }

    fn test_context() -> Context {
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                role: "user".to_string(),
                content: MessageContent::Text("hello".to_string()),
                timestamp: chrono::Utc::now(),
            })],
            tools: None,
        }
    }

    #[test]
    fn openai_completions_builds_deepseek_simple_params_like_pi_ai() {
        let provider = OpenAiCompletionsProvider::new();
        let mut options = SimpleStreamOptions::default();
        options.base.max_tokens = Some(1024);
        options.base.temperature = Some(0.2);
        options.reasoning = Some(ThinkingLevel::XHigh);

        let body =
            provider.build_simple_request_body(&test_model(), &test_context(), Some(&options));

        assert_eq!(body["model"], serde_json::json!("deepseek-v4-flash"));
        assert_eq!(body["stream"], serde_json::json!(true));
        assert_eq!(
            body["stream_options"]["include_usage"],
            serde_json::json!(true)
        );
        assert_eq!(body["temperature"], serde_json::json!(0.2));
        assert_eq!(body["max_completion_tokens"], serde_json::json!(1024));
        assert_eq!(body["max_tokens"], serde_json::Value::Null);
        assert_eq!(body["thinking"], serde_json::json!({ "type": "enabled" }));
        assert_eq!(body["reasoning_effort"], serde_json::json!("max"));
    }

    #[test]
    fn openai_completions_disables_deepseek_thinking_when_reasoning_is_absent() {
        let provider = OpenAiCompletionsProvider::new();

        let body = provider.build_simple_request_body(&test_model(), &test_context(), None);

        assert_eq!(body["thinking"], serde_json::json!({ "type": "disabled" }));
    }

    #[test]
    fn openai_completions_converts_tools_and_tool_history_like_pi_ai() {
        let provider = OpenAiCompletionsProvider::new();
        let mut context = test_context();
        context.tools = Some(vec![Tool {
            name: "lookup".to_string(),
            description: "Lookup facts".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "q": { "type": "string" }
                },
                "required": ["q"]
            }),
        }]);

        let body = provider.build_request_body(&test_model(), &context, None);

        assert_eq!(body["tools"][0]["type"], serde_json::json!("function"));
        assert_eq!(
            body["tools"][0]["function"]["name"],
            serde_json::json!("lookup")
        );
        assert_eq!(
            body["tools"][0]["function"]["strict"],
            serde_json::json!(false)
        );

        let mut history_context = test_context();
        history_context
            .messages
            .push(Message::Assistant(AssistantMessage {
                role: "assistant".to_string(),
                content: vec![AssistantContent::ToolCall(ToolCall {
                    content_type: "toolCall".to_string(),
                    id: "call-1".to_string(),
                    name: "lookup".to_string(),
                    arguments: serde_json::json!({"q": "rust"}),
                    thought_signature: None,
                })],
                api: Api::Known(KnownApi::OpenAiCompletions),
                provider: Provider::Known(KnownProvider::Deepseek),
                model: "deepseek-v4-flash".to_string(),
                response_model: None,
                response_id: None,
                usage: Usage::default(),
                stop_reason: StopReason::ToolUse,
                error_message: None,
                timestamp: chrono::Utc::now(),
            }));

        let history_body = provider.build_request_body(&test_model(), &history_context, None);
        assert_eq!(history_body["tools"], serde_json::json!([]));
    }

    #[test]
    fn openai_completions_converts_tool_results_with_names_images_and_bridges() {
        let provider = OpenAiCompletionsProvider::new();
        let mut model = test_model();
        model.input.push("image".to_string());
        if let Some(compat) = model.compat.as_mut() {
            compat.requires_tool_result_name = Some(true);
            compat.requires_assistant_after_tool_result = Some(true);
        }
        let mut context = Context {
            system_prompt: None,
            messages: vec![
                Message::Assistant(AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        content_type: "toolCall".to_string(),
                        id: "call-1".to_string(),
                        name: "lookup".to_string(),
                        arguments: serde_json::json!({"q": "rust"}),
                        thought_signature: None,
                    })],
                    api: Api::Known(KnownApi::OpenAiCompletions),
                    provider: Provider::Known(KnownProvider::Deepseek),
                    model: "deepseek-v4-flash".to_string(),
                    response_model: None,
                    response_id: None,
                    usage: Usage::default(),
                    stop_reason: StopReason::ToolUse,
                    error_message: None,
                    timestamp: chrono::Utc::now(),
                }),
                Message::ToolResult(ToolResultMessage {
                    role: "toolResult".to_string(),
                    tool_call_id: "call-1".to_string(),
                    tool_name: "lookup".to_string(),
                    content: vec![
                        ToolResultContent::Text(TextContent {
                            content_type: "text".to_string(),
                            text: "result text".to_string(),
                            text_signature: None,
                        }),
                        ToolResultContent::Image(ImageContent {
                            content_type: "image".to_string(),
                            data: "aW1hZ2U=".to_string(),
                            mime_type: "image/png".to_string(),
                        }),
                    ],
                    details: serde_json::json!({}),
                    is_error: false,
                    timestamp: chrono::Utc::now(),
                }),
                Message::User(UserMessage {
                    role: "user".to_string(),
                    content: MessageContent::Text("next".to_string()),
                    timestamp: chrono::Utc::now(),
                }),
            ],
            tools: None,
        };

        let body = provider.build_request_body(&model, &context, None);
        let messages = body["messages"].as_array().expect("messages");

        assert_eq!(messages[1]["role"], serde_json::json!("tool"));
        assert_eq!(messages[1]["content"], serde_json::json!("result text"));
        assert_eq!(messages[1]["tool_call_id"], serde_json::json!("call-1"));
        assert_eq!(messages[1]["name"], serde_json::json!("lookup"));
        assert_eq!(messages[2]["role"], serde_json::json!("assistant"));
        assert_eq!(
            messages[2]["content"],
            serde_json::json!("I have processed the tool results.")
        );
        assert_eq!(messages[3]["role"], serde_json::json!("user"));
        assert_eq!(
            messages[3]["content"][0]["text"],
            serde_json::json!("Attached image(s) from tool result:")
        );
        assert_eq!(
            messages[3]["content"][1]["image_url"]["url"],
            serde_json::json!("data:image/png;base64,aW1hZ2U=")
        );
        assert_eq!(messages[4]["role"], serde_json::json!("user"));
        assert_eq!(messages[4]["content"], serde_json::json!("next"));

        context.messages = vec![
            Message::ToolResult(ToolResultMessage {
                role: "toolResult".to_string(),
                tool_call_id: "call-2".to_string(),
                tool_name: "lookup".to_string(),
                content: vec![ToolResultContent::Text(TextContent {
                    content_type: "text".to_string(),
                    text: "plain".to_string(),
                    text_signature: None,
                })],
                details: serde_json::json!({}),
                is_error: false,
                timestamp: chrono::Utc::now(),
            }),
            Message::User(UserMessage {
                role: "user".to_string(),
                content: MessageContent::Text("after tool".to_string()),
                timestamp: chrono::Utc::now(),
            }),
        ];

        let body = provider.build_request_body(&model, &context, None);
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages[1]["role"], serde_json::json!("assistant"));
        assert_eq!(
            messages[1]["content"],
            serde_json::json!("I have processed the tool results.")
        );
        assert_eq!(messages[2]["content"], serde_json::json!("after tool"));
    }

    #[test]
    fn openai_completions_skips_empty_assistant_and_round_trips_reasoning_details() {
        let provider = OpenAiCompletionsProvider::new();
        let context = Context {
            system_prompt: None,
            messages: vec![
                Message::Assistant(AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![],
                    api: Api::Known(KnownApi::OpenAiCompletions),
                    provider: Provider::Known(KnownProvider::Deepseek),
                    model: "deepseek-v4-flash".to_string(),
                    response_model: None,
                    response_id: None,
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: chrono::Utc::now(),
                }),
                Message::Assistant(AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        content_type: "toolCall".to_string(),
                        id: "call-1".to_string(),
                        name: "lookup".to_string(),
                        arguments: serde_json::json!({}),
                        thought_signature: Some(
                            r#"{"type":"reasoning.encrypted","id":"call-1","data":"sealed"}"#
                                .to_string(),
                        ),
                    })],
                    api: Api::Known(KnownApi::OpenAiCompletions),
                    provider: Provider::Known(KnownProvider::Deepseek),
                    model: "deepseek-v4-flash".to_string(),
                    response_model: None,
                    response_id: None,
                    usage: Usage::default(),
                    stop_reason: StopReason::ToolUse,
                    error_message: None,
                    timestamp: chrono::Utc::now(),
                }),
                Message::ToolResult(ToolResultMessage {
                    role: "toolResult".to_string(),
                    tool_call_id: "call-1".to_string(),
                    tool_name: "lookup".to_string(),
                    content: vec![ToolResultContent::Text(TextContent {
                        content_type: "text".to_string(),
                        text: "ok".to_string(),
                        text_signature: None,
                    })],
                    details: serde_json::json!({}),
                    is_error: false,
                    timestamp: chrono::Utc::now(),
                }),
            ],
            tools: None,
        };

        let body = provider.build_request_body(&test_model(), &context, None);
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], serde_json::json!("assistant"));
        assert_eq!(
            messages[0]["reasoning_details"][0],
            serde_json::json!({
                "type": "reasoning.encrypted",
                "id": "call-1",
                "data": "sealed",
            })
        );
    }

    #[test]
    fn openai_completions_tool_arguments_are_repaired_before_final_parse() {
        let mut parser = OpenAiCompletionsStreamParser::new(&test_model());

        let _ = parser.process_data(&chunk(
            serde_json::json!({
                "tool_calls": [{
                    "index": 0,
                    "id": "call-1",
                    "function": {
                        "name": "write_file",
                        "arguments": "{\"path\":\"C:\\q\",\"text\":\"hello\nworld\"}",
                    },
                }],
            }),
            Some("tool_calls"),
        ));
        let done = parser.process_data("[DONE]");

        assert!(matches!(
            done[0],
            AssistantMessageEvent::ToolCallEnd {
                ref tool_call, ..
            } if tool_call.arguments == serde_json::json!({
                "path": "C:\\q",
                "text": "hello\nworld"
            })
        ));
    }

    #[test]
    fn openai_completions_streaming_tool_arguments_parse_partial_json() {
        let mut parser = OpenAiCompletionsStreamParser::new(&test_model());

        let events = parser.process_data(&chunk(
            serde_json::json!({
                "tool_calls": [{
                    "index": 0,
                    "id": "call-1",
                    "function": {
                        "name": "write_file",
                        "arguments": "{\"path\":\"/tmp/a\"",
                    },
                }],
            }),
            None,
        ));

        assert!(matches!(
            events.as_slice(),
            [
                AssistantMessageEvent::ToolCallStart { .. },
                AssistantMessageEvent::ToolCallDelta {
                    partial: AssistantMessage { content, .. },
                    ..
                },
            ] if matches!(
                content.first(),
                Some(AssistantContent::ToolCall(tool_call))
                    if tool_call.arguments == serde_json::json!({ "path": "/tmp/a" })
            )
        ));
    }

    #[test]
    fn openai_completions_usage_includes_cost() {
        let mut parser = OpenAiCompletionsStreamParser::new(&test_model());

        let _ = parser.process_data(
            &serde_json::json!({
                "choices": [{
                    "delta": { "content": "ok" },
                    "finish_reason": "stop",
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "prompt_cache_hit_tokens": 2,
                    "prompt_cache_miss_tokens": 3,
                    "total_tokens": 15,
                },
            })
            .to_string(),
        );
        let done = parser.process_data("[DONE]");

        assert!(matches!(
            done.last(),
            Some(AssistantMessageEvent::Done { message, .. })
                if message.usage.cost.total > 0.0
        ));
    }

    #[test]
    fn openai_completions_uses_pi_cache_retention_env_default() {
        let provider = OpenAiCompletionsProvider::new();
        let mut model = test_model();
        model.provider = Provider::Known(KnownProvider::OpenAi);
        model.base_url = "https://api.openai.com/v1".to_string();
        model.compat = None;

        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var("PI_CACHE_RETENTION").ok();
        unsafe {
            std::env::set_var("PI_CACHE_RETENTION", "long");
        }

        let body = provider.build_request_body(
            &model,
            &test_context(),
            Some(&StreamOptions {
                session_id: Some("session-1".to_string()),
                ..Default::default()
            }),
        );

        restore_env("PI_CACHE_RETENTION", previous);

        assert_eq!(body["prompt_cache_key"], serde_json::json!("session-1"));
        assert_eq!(body["prompt_cache_retention"], serde_json::json!("24h"));
    }

    #[test]
    fn openai_completions_sets_prompt_cache_key_for_openai_short_retention() {
        let provider = OpenAiCompletionsProvider::new();
        let mut model = test_model();
        model.provider = Provider::Known(KnownProvider::OpenAi);
        model.base_url = "https://api.openai.com/v1".to_string();
        model.compat = None;

        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var("PI_CACHE_RETENTION").ok();
        unsafe {
            std::env::remove_var("PI_CACHE_RETENTION");
        }

        let body = provider.build_request_body(
            &model,
            &test_context(),
            Some(&StreamOptions {
                session_id: Some("session-1".to_string()),
                ..Default::default()
            }),
        );

        restore_env("PI_CACHE_RETENTION", previous);

        assert_eq!(body["prompt_cache_key"], serde_json::json!("session-1"));
        assert_eq!(body["prompt_cache_retention"], serde_json::Value::Null);
    }

    #[test]
    fn openai_completions_reads_usage_from_choice_fallback() {
        let mut parser = OpenAiCompletionsStreamParser::new(&test_model());

        let _ = parser.process_data(
            &serde_json::json!({
                "choices": [{
                    "delta": { "content": "ok" },
                    "finish_reason": "stop",
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 5,
                        "prompt_tokens_details": {
                            "cached_tokens": 2,
                            "cache_write_tokens": 3
                        }
                    },
                }],
            })
            .to_string(),
        );
        let done = parser.process_data("[DONE]");

        assert!(matches!(
            done.last(),
            Some(AssistantMessageEvent::Done { message, .. })
                if message.usage.input == 5
                    && message.usage.output == 5
                    && message.usage.cache_read == 2
                    && message.usage.cache_write == 3
                    && message.usage.total_tokens == 15
                    && message.usage.cost.total > 0.0
        ));
    }

    #[test]
    fn openai_completions_errors_when_stream_ends_without_finish_reason() {
        let mut parser = OpenAiCompletionsStreamParser::new(&test_model());

        let _ = parser.process_data(&chunk(serde_json::json!({"content": "ok"}), None));
        let done = parser.process_data("[DONE]");

        assert!(matches!(
            done.last(),
            Some(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: AssistantMessage {
                    error_message: Some(message),
                    stop_reason: StopReason::Error,
                    ..
                },
            }) if message == "Stream ended without finish_reason"
        ));
    }

    #[test]
    fn openai_completions_maps_reasoning_details_to_tool_thought_signature() {
        let mut parser = OpenAiCompletionsStreamParser::new(&test_model());

        let _ = parser.process_data(&chunk(
            serde_json::json!({
                "tool_calls": [{
                    "index": 0,
                    "id": "call-1",
                    "function": {
                        "name": "lookup",
                        "arguments": "{}",
                    },
                }],
                "reasoning_details": [{
                    "type": "reasoning.encrypted",
                    "id": "call-1",
                    "data": "sealed",
                }],
            }),
            Some("tool_calls"),
        ));
        let done = parser.process_data("[DONE]");

        assert!(matches!(
            done.first(),
            Some(AssistantMessageEvent::ToolCallEnd { tool_call, .. })
                if tool_call.thought_signature.as_deref()
                    == Some(r#"{"data":"sealed","id":"call-1","type":"reasoning.encrypted"}"#)
        ));
    }

    #[test]
    fn deepseek_parser_emits_text_lifecycle_events() {
        let mut parser = OpenAiCompletionsStreamParser::new(&test_model());

        let first = parser.process_data(&chunk(serde_json::json!({"content": "Hel"}), None));
        let second =
            parser.process_data(&chunk(serde_json::json!({"content": "lo"}), Some("stop")));
        let done = parser.process_data("[DONE]");

        assert!(matches!(first[0], AssistantMessageEvent::TextStart { .. }));
        assert!(matches!(
            first[1],
            AssistantMessageEvent::TextDelta { ref delta, .. } if delta == "Hel"
        ));
        assert!(matches!(
            second[0],
            AssistantMessageEvent::TextDelta { ref delta, .. } if delta == "lo"
        ));
        assert!(matches!(
            done[0],
            AssistantMessageEvent::TextEnd { ref content, .. } if content == "Hello"
        ));
        assert!(matches!(
            done[1],
            AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                ref message,
            } if matches!(
                message.content.first(),
                Some(AssistantContent::Text(TextContent { text, .. })) if text == "Hello"
            )
        ));
    }

    #[test]
    fn deepseek_parser_emits_thinking_lifecycle_events() {
        let mut parser = OpenAiCompletionsStreamParser::new(&test_model());

        let first = parser.process_data(&chunk(
            serde_json::json!({"reasoning_content": "plan"}),
            None,
        ));
        let second = parser.process_data(&chunk(
            serde_json::json!({"reasoning_content": " more"}),
            Some("stop"),
        ));
        let done = parser.process_data("[DONE]");

        assert!(matches!(
            first.as_slice(),
            [
                AssistantMessageEvent::ThinkingStart { .. },
                AssistantMessageEvent::ThinkingDelta { delta, .. },
            ] if delta == "plan"
        ));
        assert!(matches!(
            second.as_slice(),
            [AssistantMessageEvent::ThinkingDelta { delta, .. }] if delta == " more"
        ));
        assert!(matches!(
            done[0],
            AssistantMessageEvent::ThinkingEnd { ref content, .. } if content == "plan more"
        ));
    }

    #[test]
    fn deepseek_parser_emits_toolcall_lifecycle_events() {
        let mut parser = OpenAiCompletionsStreamParser::new(&test_model());

        let first = parser.process_data(&chunk(
            serde_json::json!({
                "tool_calls": [{
                    "index": 0,
                    "id": "call-1",
                    "function": {
                        "name": "lookup",
                        "arguments": "{\"q\"",
                    },
                }],
            }),
            None,
        ));
        let second = parser.process_data(&chunk(
            serde_json::json!({
                "tool_calls": [{
                    "index": 0,
                    "function": {
                        "arguments": ":\"rust\"}",
                    },
                }],
            }),
            Some("tool_calls"),
        ));
        let done = parser.process_data("[DONE]");

        assert!(matches!(
            first.as_slice(),
            [
                AssistantMessageEvent::ToolCallStart { .. },
                AssistantMessageEvent::ToolCallDelta { delta, .. },
            ] if delta == "{\"q\""
        ));
        assert!(matches!(
            second.as_slice(),
            [AssistantMessageEvent::ToolCallDelta { delta, .. }] if delta == ":\"rust\"}"
        ));
        assert!(matches!(
            done[0],
            AssistantMessageEvent::ToolCallEnd {
                ref tool_call, ..
            } if tool_call.id == "call-1"
                && tool_call.name == "lookup"
                && tool_call.arguments == serde_json::json!({"q": "rust"})
        ));
        assert!(matches!(
            done[1],
            AssistantMessageEvent::Done {
                reason: StopReason::ToolUse,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn deepseek_stream_returns_aborted_when_signal_is_cancelled_before_request() {
        let provider = OpenAiCompletionsProvider::new();
        let signal = AbortSignal::new();
        signal.cancel();

        let mut stream = provider.stream(
            &test_model(),
            &test_context(),
            Some(&StreamOptions {
                signal: Some(signal),
                ..Default::default()
            }),
        );

        let event = stream.next().await.expect("aborted event");
        assert!(matches!(
            event,
            AssistantMessageEvent::Error {
                reason: StopReason::Aborted,
                error: AssistantMessage {
                    stop_reason: StopReason::Aborted,
                    ..
                },
            }
        ));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn deepseek_stream_invokes_on_response_with_actual_status_and_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let _ = socket.read(&mut buf).await.unwrap();
            let body = format!(
                "data: {}\n\ndata: [DONE]\n\n",
                chunk(serde_json::json!({"content": "ok"}), Some("stop"))
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nx-test-response: yes\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let captured = Arc::new(Mutex::new(None::<ProviderResponse>));
        let captured_for_hook = captured.clone();
        let provider = OpenAiCompletionsProvider::new();
        let mut model = test_model();
        model.base_url = format!("http://{}", addr);

        let mut stream = provider.stream(
            &model,
            &test_context(),
            Some(&StreamOptions {
                on_response: Some(Arc::new(move |response| {
                    let captured = captured_for_hook.clone();
                    Box::pin(async move {
                        *captured.lock().unwrap() = Some(response);
                    })
                })),
                ..Default::default()
            }),
        );

        while stream.next().await.is_some() {}
        server.await.unwrap();

        let response = captured.lock().unwrap().clone().expect("response hook");
        assert_eq!(response.status, 200);
        assert_eq!(
            response.headers.get("x-test-response").map(String::as_str),
            Some("yes")
        );
    }

    #[tokio::test]
    async fn deepseek_stream_errors_when_sse_ends_without_finish_reason() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let _ = socket.read(&mut buf).await.unwrap();
            let body = format!(
                "data: {}\n\n",
                chunk(serde_json::json!({"content": "ok"}), None)
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let provider = OpenAiCompletionsProvider::new();
        let mut model = test_model();
        model.base_url = format!("http://{}", addr);
        let mut stream = provider.stream(&model, &test_context(), None);

        let mut error_message = None;
        while let Some(event) = stream.next().await {
            if let AssistantMessageEvent::Error { error, .. } = event {
                error_message = error.error_message;
            }
        }
        server.await.unwrap();

        assert_eq!(
            error_message.as_deref(),
            Some("Stream ended without finish_reason")
        );
    }

    #[tokio::test]
    async fn deepseek_stream_retries_retryable_response_and_requests_usage() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let attempts = Arc::new(Mutex::new(0_usize));
        let request_bodies = Arc::new(Mutex::new(Vec::new()));
        let attempts_for_server = attempts.clone();
        let bodies_for_server = request_bodies.clone();

        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0_u8; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                let request = String::from_utf8_lossy(&buf[..n]);
                if let Some((_, body)) = request.split_once("\r\n\r\n") {
                    bodies_for_server.lock().unwrap().push(body.to_string());
                }

                let attempt = {
                    let mut attempts = attempts_for_server.lock().unwrap();
                    *attempts += 1;
                    *attempts
                };
                if attempt == 1 {
                    let response = "HTTP/1.1 429 Too Many Requests\r\nretry-after-ms: 1\r\ncontent-length: 0\r\n\r\n";
                    socket.write_all(response.as_bytes()).await.unwrap();
                } else {
                    let body = format!(
                        "data: {}\n\ndata: [DONE]\n\n",
                        chunk(serde_json::json!({"content": "ok"}), Some("stop"))
                    );
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                }
            }
        });

        let provider = OpenAiCompletionsProvider::new();
        let mut model = test_model();
        model.base_url = format!("http://{}", addr);
        let mut stream = provider.stream(
            &model,
            &test_context(),
            Some(&StreamOptions {
                max_retries: Some(1),
                max_retry_delay_ms: Some(10),
                ..Default::default()
            }),
        );

        let mut final_message = None;
        while let Some(event) = stream.next().await {
            if let AssistantMessageEvent::Done { message, .. } = event {
                final_message = Some(message);
            }
        }
        server.await.unwrap();

        assert_eq!(*attempts.lock().unwrap(), 2);
        assert!(matches!(
            final_message,
            Some(AssistantMessage {
                stop_reason: StopReason::Stop,
                ..
            })
        ));
        let first_body: serde_json::Value =
            serde_json::from_str(&request_bodies.lock().unwrap()[0]).unwrap();
        assert_eq!(
            first_body["stream_options"]["include_usage"],
            serde_json::json!(true)
        );
    }

    #[tokio::test]
    async fn openai_cloudflare_gateway_resolves_base_url_and_uses_cf_auth_header() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured_request = Arc::new(Mutex::new(String::new()));
        let captured_for_server = captured_request.clone();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 8192];
            let n = socket.read(&mut buf).await.unwrap();
            *captured_for_server.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();

            let body = format!(
                "data: {}\n\ndata: [DONE]\n\n",
                chunk(serde_json::json!({"content": "ok"}), Some("stop"))
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let provider = OpenAiCompletionsProvider::new();
        let mut model = test_model();
        model.provider = Provider::Known(KnownProvider::CloudflareAiGateway);
        model.base_url = format!(
            "http://127.0.0.1:{}/{{FLOWN_AI_TEST_CLOUDFLARE_OPENAI_PATH}}",
            addr.port()
        );
        model.compat = None;
        let mut stream = {
            let _guard = ENV_LOCK.lock().unwrap();
            let previous = std::env::var("FLOWN_AI_TEST_CLOUDFLARE_OPENAI_PATH").ok();
            unsafe {
                std::env::set_var("FLOWN_AI_TEST_CLOUDFLARE_OPENAI_PATH", "cf-openai");
            }
            let stream = provider.stream(
                &model,
                &test_context(),
                Some(&StreamOptions {
                    api_key: Some("cf-test-key".to_string()),
                    ..Default::default()
                }),
            );
            restore_env("FLOWN_AI_TEST_CLOUDFLARE_OPENAI_PATH", previous);
            stream
        };

        while stream.next().await.is_some() {}
        server.await.unwrap();

        let request = captured_request.lock().unwrap().clone();
        assert!(request.starts_with("POST /cf-openai/chat/completions HTTP/1.1"));
        let headers = request
            .split_once("\r\n\r\n")
            .expect("request headers")
            .0
            .to_ascii_lowercase();
        assert!(has_header_line(
            &headers,
            "cf-aig-authorization: bearer cf-test-key"
        ));
        assert!(!has_header_line(
            &headers,
            "authorization: bearer cf-test-key"
        ));
    }

    #[tokio::test]
    async fn openai_github_copilot_adds_dynamic_headers_for_agent_vision_request() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured_request = Arc::new(Mutex::new(String::new()));
        let captured_for_server = captured_request.clone();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 8192];
            let n = socket.read(&mut buf).await.unwrap();
            *captured_for_server.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();

            let body = format!(
                "data: {}\n\ndata: [DONE]\n\n",
                chunk(serde_json::json!({"content": "ok"}), Some("stop"))
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let provider = OpenAiCompletionsProvider::new();
        let mut model = test_model();
        model.provider = Provider::Known(KnownProvider::GithubCopilot);
        model.base_url = format!("http://{}", addr);
        model.input.push("image".to_string());
        model.compat = None;
        let context = Context {
            system_prompt: None,
            messages: vec![
                Message::User(UserMessage {
                    role: "user".to_string(),
                    content: MessageContent::Blocks(vec![UserContentBlock::Image(ImageContent {
                        content_type: "image".to_string(),
                        data: "aW1hZ2U=".to_string(),
                        mime_type: "image/png".to_string(),
                    })]),
                    timestamp: chrono::Utc::now(),
                }),
                Message::Assistant(AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![AssistantContent::Text(TextContent {
                        content_type: "text".to_string(),
                        text: "ok".to_string(),
                        text_signature: None,
                    })],
                    api: Api::Known(KnownApi::OpenAiCompletions),
                    provider: Provider::Known(KnownProvider::GithubCopilot),
                    model: "copilot-test".to_string(),
                    response_model: None,
                    response_id: None,
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: chrono::Utc::now(),
                }),
            ],
            tools: None,
        };

        let mut stream = provider.stream(
            &model,
            &context,
            Some(&StreamOptions {
                api_key: Some("copilot-test-key".to_string()),
                ..Default::default()
            }),
        );

        while stream.next().await.is_some() {}
        server.await.unwrap();

        let request = captured_request.lock().unwrap().clone();
        let headers = request
            .split_once("\r\n\r\n")
            .expect("request headers")
            .0
            .to_ascii_lowercase();
        assert!(headers.contains("x-initiator: agent"));
        assert!(headers.contains("openai-intent: conversation-edits"));
        assert!(headers.contains("copilot-vision-request: true"));
    }
}

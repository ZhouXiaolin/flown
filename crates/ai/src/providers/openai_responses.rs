use crate::api_registry::{ApiProvider, AssistantMessageEventStream, RawEventStream};
use crate::models::{clamp_thinking_level, transform_messages};
use crate::providers::common::{
    build_copilot_dynamic_headers, is_cloudflare_ai_gateway, is_cloudflare_provider,
    resolve_cloudflare_base_url,
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

/// Internal OpenAI Responses stream options.
#[derive(Debug, Clone, Default)]
struct OpenAiResponsesStreamOptions {
    pub base: StreamOptions,
    pub reasoning_effort: Option<String>,
    pub reasoning_summary: Option<String>,
    pub service_tier: Option<String>,
}

struct OpenAiResponsesApiProvider;

#[derive(Debug, Clone, PartialEq, Eq)]
enum CurrentResponsesItem {
    Reasoning,
    Message {
        id: Option<String>,
        phase: Option<TextSignaturePhase>,
        kind: Option<CurrentMessageContentKind>,
    },
    FunctionCall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CurrentMessageContentKind {
    OutputText,
    Refusal,
}

fn aborted_message(partial: &AssistantMessage) -> AssistantMessage {
    AssistantMessage {
        error_message: Some("Operation aborted".to_string()),
        stop_reason: StopReason::Aborted,
        ..partial.clone()
    }
}

fn response_url(model: &Model) -> Result<String, String> {
    if is_cloudflare_provider(model) {
        resolve_cloudflare_base_url(model)
            .map(|base_url| format!("{}/responses", base_url.trim_end_matches('/')))
    } else {
        Ok(format!(
            "{}/responses",
            model.base_url.trim_end_matches('/')
        ))
    }
}

fn headers_to_reqwest(headers: &HashMap<String, String>) -> reqwest::header::HeaderMap {
    let mut map = reqwest::header::HeaderMap::new();
    for (key, value) in headers {
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(key.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            map.insert(name, val);
        }
    }
    map
}

fn build_messages(model: &Model, context: &Context) -> Vec<Message> {
    transform_messages(&context.messages, model, None)
}

fn convert_responses_input(model: &Model, context: &Context) -> Vec<serde_json::Value> {
    let mut input = Vec::new();

    if let Some(system_prompt) = &context.system_prompt {
        let role = if model.reasoning
            && model
                .compat
                .as_ref()
                .and_then(|compat| compat.supports_developer_role)
                .unwrap_or(true)
        {
            "developer"
        } else {
            "system"
        };
        input.push(serde_json::json!({
            "role": role,
            "content": system_prompt,
        }));
    }

    for message in build_messages(model, context) {
        match message {
            Message::User(user) => match user.content {
                MessageContent::Text(text) => {
                    input.push(serde_json::json!({
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": text,
                        }],
                    }));
                }
                MessageContent::Blocks(blocks) => {
                    let content: Vec<serde_json::Value> = blocks
                        .into_iter()
                        .map(|block| match block {
                            UserContentBlock::Text(text) => serde_json::json!({
                                "type": "input_text",
                                "text": text.text,
                            }),
                            UserContentBlock::Image(image) => serde_json::json!({
                                "type": "input_image",
                                "detail": "auto",
                                "image_url": format!("data:{};base64,{}", image.mime_type, image.data),
                            }),
                        })
                        .collect();
                    if !content.is_empty() {
                        input.push(serde_json::json!({
                            "role": "user",
                            "content": content,
                        }));
                    }
                }
            },
            Message::Assistant(assistant) => {
                for block in assistant.content {
                    match block {
                        AssistantContent::Thinking(thinking) => {
                            if let Some(signature) = thinking.thinking_signature {
                                if let Ok(value) =
                                    serde_json::from_str::<serde_json::Value>(&signature)
                                {
                                    input.push(value);
                                }
                            }
                        }
                        AssistantContent::Text(text) => {
                            let parsed = text
                                .text_signature
                                .as_deref()
                                .map(parse_text_signature)
                                .unwrap_or_else(|| TextSignatureV1::new(""));
                            let mut item = serde_json::json!({
                                "type": "message",
                                "role": "assistant",
                                "status": "completed",
                                "content": [{
                                    "type": "output_text",
                                    "text": text.text,
                                    "annotations": [],
                                }],
                            });
                            if !parsed.id.is_empty() {
                                item["id"] = serde_json::json!(parsed.id);
                            }
                            if let Some(phase) = parsed.phase {
                                item["phase"] = serde_json::json!(match phase {
                                    TextSignaturePhase::Commentary => "commentary",
                                    TextSignaturePhase::FinalAnswer => "final_answer",
                                });
                            }
                            input.push(item);
                        }
                        AssistantContent::ToolCall(tool_call) => {
                            let (call_id, item_id) = tool_call
                                .id
                                .split_once('|')
                                .map(|(call_id, item_id)| {
                                    (call_id.to_string(), Some(item_id.to_string()))
                                })
                                .unwrap_or_else(|| (tool_call.id.clone(), None));
                            let mut item = serde_json::json!({
                                "type": "function_call",
                                "call_id": call_id,
                                "name": tool_call.name,
                                "arguments": serde_json::to_string(&tool_call.arguments).unwrap_or_else(|_| "{}".to_string()),
                            });
                            if let Some(item_id) = item_id {
                                item["id"] = serde_json::json!(item_id);
                            }
                            input.push(item);
                        }
                    }
                }
            }
            Message::ToolResult(tool_result) => {
                let call_id = tool_result
                    .tool_call_id
                    .split_once('|')
                    .map(|(call_id, _)| call_id.to_string())
                    .unwrap_or_else(|| tool_result.tool_call_id.clone());
                let text_output = tool_result
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        ToolResultContent::Text(text) => Some(text.text.clone()),
                        ToolResultContent::Image(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                input.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": if text_output.is_empty() { "(no output)" } else { &text_output },
                }));
            }
        }
    }

    input
}

fn convert_responses_tools(tools: &[Tool]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
                "strict": false,
            })
        })
        .collect()
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

const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;

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

fn build_body(
    model: &Model,
    context: &Context,
    options: Option<&OpenAiResponsesStreamOptions>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model.id,
        "stream": true,
        "input": convert_responses_input(model, context),
        "store": false,
    });

    if let Some(max_tokens) = options.and_then(|o| o.base.max_tokens) {
        body["max_output_tokens"] = serde_json::json!(max_tokens);
    }
    if let Some(temperature) = options.and_then(|o| o.base.temperature) {
        body["temperature"] = serde_json::json!(temperature);
    }
    if let Some(service_tier) = options.and_then(|o| o.service_tier.clone()) {
        body["service_tier"] = serde_json::json!(service_tier);
    }

    let reasoning_effort = options.and_then(|o| o.reasoning_effort.clone());
    let reasoning_summary = options.and_then(|o| o.reasoning_summary.clone());
    if model.reasoning {
        if reasoning_effort.is_some() || reasoning_summary.is_some() {
            let effort = reasoning_effort
                .as_deref()
                .map(|level| match level {
                    "minimal" | "low" | "medium" | "high" | "xhigh" => level,
                    _ => "medium",
                })
                .unwrap_or("medium");
            let mut reasoning = serde_json::Map::new();
            reasoning.insert("effort".to_string(), serde_json::json!(effort));
            if let Some(summary) = reasoning_summary {
                reasoning.insert("summary".to_string(), serde_json::json!(summary));
            }
            body["reasoning"] = serde_json::Value::Object(reasoning);
            body["include"] = serde_json::json!(["reasoning.encrypted_content"]);
        }
    }

    if context
        .tools
        .as_ref()
        .is_some_and(|tools| !tools.is_empty())
    {
        body["tools"] = serde_json::json!(convert_responses_tools(
            context.tools.as_ref().expect("checked is_some_and")
        ));
    }

    body
}

fn map_response_status(status: Option<&str>) -> StopReason {
    match status {
        Some("incomplete") => StopReason::Length,
        Some("failed") | Some("cancelled") => StopReason::Error,
        _ => StopReason::Stop,
    }
}

fn update_usage_from_response(output: &mut AssistantMessage, response: &serde_json::Value) {
    let usage = response.get("usage");
    let input_tokens = usage
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;
    let output_tokens = usage
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;
    let total_tokens = usage
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(|value| value.as_u64())
        .unwrap_or((input_tokens + output_tokens) as u64) as u32;
    let cached_tokens = usage
        .and_then(|usage| usage.get("input_tokens_details"))
        .and_then(|details| details.get("cached_tokens"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;
    output.usage.input = input_tokens.saturating_sub(cached_tokens);
    output.usage.output = output_tokens;
    output.usage.cache_read = cached_tokens;
    output.usage.cache_write = 0;
    output.usage.total_tokens = total_tokens;
}

fn stream_openai_responses(
    client: Client,
    model: Model,
    context: Context,
    options: Option<OpenAiResponsesStreamOptions>,
    mut body: serde_json::Value,
) -> RawEventStream {
    let on_payload = options.as_ref().and_then(|o| o.base.on_payload.clone());
    let on_response = options.as_ref().and_then(|o| o.base.on_response.clone());
    let signal = options.as_ref().and_then(|o| o.base.signal.clone());
    let api_key = options
        .as_ref()
        .and_then(|o| o.base.api_key.clone())
        .unwrap_or_default();
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
        headers.extend(headers_to_reqwest(model_headers));
    }

    if model.provider == Provider::Known(KnownProvider::GithubCopilot) {
        for (key, value) in build_copilot_dynamic_headers(&context.messages) {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                reqwest::header::HeaderValue::from_str(&value),
            ) {
                headers.insert(name, val);
            }
        }
    }

    if let Some(session_id) = options
        .as_ref()
        .and_then(|opts| opts.base.session_id.as_ref())
    {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(session_id) {
            headers.insert("session_id", value.clone());
            headers.insert("x-client-request-id", value);
        }
    }

    if let Some(custom_headers) = options.as_ref().and_then(|opts| opts.base.headers.as_ref()) {
        headers.extend(headers_to_reqwest(custom_headers));
    }

    if is_cloudflare_ai_gateway(&model) {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", api_key)) {
            headers.insert("cf-aig-authorization", value);
        }
    }

    let mut output = AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
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
    };
    let mut current_item: Option<CurrentResponsesItem> = None;

    Box::pin(stream! {
        let url = match response_url(&model) {
            Ok(url) => url,
            Err(error) => {
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: AssistantMessage {
                        error_message: Some(error),
                        stop_reason: StopReason::Error,
                        ..output.clone()
                    },
                };
                return;
            }
        };

        if signal.as_ref().is_some_and(|signal| signal.is_cancelled()) {
            yield AssistantMessageEvent::Error {
                reason: StopReason::Aborted,
                error: aborted_message(&output),
            };
            return;
        }

        if let Some(hook) = on_payload
            && let Some(modified) = hook(body.clone()).await
        {
                body = modified;
        }

        let max_retries = options.as_ref().and_then(|opts| opts.base.max_retries).unwrap_or(0);
        let mut attempt = 0_u32;
        let response = loop {
            let mut request = client.post(&url).headers(headers.clone()).json(&body);
            if let Some(timeout_ms) = options.as_ref().and_then(|opts| opts.base.timeout_ms) {
                request = request.timeout(Duration::from_millis(timeout_ms));
            }

            let response = if let Some(signal) = signal.clone() {
                let send = request.send().fuse();
                futures::pin_mut!(send);
                futures::select! {
                    _ = signal.cancelled().fuse() => {
                        yield AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: aborted_message(&output),
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
                    let delay_ms = retry_delay_ms(attempt, Some(response.headers()), options.as_ref().map(|o| &o.base));
                    if let Some(signal) = signal.clone() {
                        let delay = futures_timer::Delay::new(Duration::from_millis(delay_ms)).fuse();
                        futures::pin_mut!(delay);
                        futures::select! {
                            _ = signal.cancelled().fuse() => {
                                yield AssistantMessageEvent::Error {
                                    reason: StopReason::Aborted,
                                    error: aborted_message(&output),
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
                    let delay_ms = retry_delay_ms(attempt, None, options.as_ref().map(|o| &o.base));
                    if let Some(signal) = signal.clone() {
                        let delay = futures_timer::Delay::new(Duration::from_millis(delay_ms)).fuse();
                        futures::pin_mut!(delay);
                        futures::select! {
                            _ = signal.cancelled().fuse() => {
                                yield AssistantMessageEvent::Error {
                                    reason: StopReason::Aborted,
                                    error: aborted_message(&output),
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
                            ..output.clone()
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
            let mut error_message = format!("HTTP {}", response.status().as_u16());
            if let Ok(bytes) = response.bytes().await
                && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let Some(msg) = value.get("error").and_then(|error| error.get("message")).and_then(|msg| msg.as_str())
            {
                error_message.push_str(": ");
                error_message.push_str(msg);
            }
            yield AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: AssistantMessage {
                    error_message: Some(error_message),
                    stop_reason: StopReason::Error,
                    ..output.clone()
                },
            };
            return;
        }

        yield AssistantMessageEvent::Start { partial: output.clone() };

        let event_stream = response.bytes_stream().eventsource();
        futures::pin_mut!(event_stream);

        loop {
            let idle = futures_timer::Delay::new(Duration::from_secs(60));
            futures::pin_mut!(idle);
            let event = if let Some(signal) = signal.clone() {
                futures::pin_mut!(signal);
                futures::select! {
                    _ = signal.cancelled().fuse() => {
                        yield AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: aborted_message(&output),
                        };
                        return;
                    }
                    event = event_stream.next().fuse() => event,
                    _ = (&mut idle).fuse() => {
                        yield AssistantMessageEvent::Error {
                            reason: StopReason::Error,
                            error: AssistantMessage {
                                error_message: Some("stream idle timeout (no data for 60s)".into()),
                                stop_reason: StopReason::Error,
                                ..output.clone()
                            },
                        };
                        return;
                    }
                }
            } else {
                futures::select! {
                    event = event_stream.next().fuse() => event,
                    _ = (&mut idle).fuse() => {
                        yield AssistantMessageEvent::Error {
                            reason: StopReason::Error,
                            error: AssistantMessage {
                                error_message: Some("stream idle timeout (no data for 60s)".into()),
                                stop_reason: StopReason::Error,
                                ..output.clone()
                            },
                        };
                        return;
                    }
                }
            };

            let Some(event) = event else {
                break;
            };

            let Ok(event) = event else {
                continue;
            };

            if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.data)
                && let Some(response_event) = payload.get("type").and_then(|v| v.as_str())
            {
                    match response_event {
                        "response.created" => {
                            output.response_id = payload
                                .get("response")
                                .and_then(|response| response.get("id"))
                                .and_then(|value| value.as_str())
                                .map(ToString::to_string);
                        }
                        "response.output_item.added" => {
                            if let Some(item) = payload.get("item") {
                                match item.get("type").and_then(|v| v.as_str()) {
                                    Some("reasoning") => {
                                        current_item = Some(CurrentResponsesItem::Reasoning);
                                        output.content.push(AssistantContent::Thinking(ThinkingContent {
                                            content_type: "thinking".to_string(),
                                            thinking: String::new(),
                                            thinking_signature: None,
                                            redacted: None,
                                        }));
                                        yield AssistantMessageEvent::ThinkingStart {
                                            content_index: output.content.len() - 1,
                                            partial: output.clone(),
                                        };
                                    }
                                    Some("message") => {
                                        current_item = Some(CurrentResponsesItem::Message {
                                            id: item.get("id").and_then(|value| value.as_str()).map(ToString::to_string),
                                            phase: item.get("phase").and_then(|value| value.as_str()).and_then(|phase| match phase {
                                                "commentary" => Some(TextSignaturePhase::Commentary),
                                                "final_answer" => Some(TextSignaturePhase::FinalAnswer),
                                                _ => None,
                                            }),
                                            kind: None,
                                        });
                                        output.content.push(AssistantContent::Text(TextContent {
                                            content_type: "text".to_string(),
                                            text: String::new(),
                                            text_signature: None,
                                        }));
                                        yield AssistantMessageEvent::TextStart {
                                            content_index: output.content.len() - 1,
                                            partial: output.clone(),
                                        };
                                    }
                                    Some("function_call") => {
                                        current_item = Some(CurrentResponsesItem::FunctionCall);
                                        output.content.push(AssistantContent::ToolCall(ToolCall {
                                            content_type: "toolCall".to_string(),
                                            id: format!(
                                                "{}|{}",
                                                item.get("call_id").and_then(|v| v.as_str()).unwrap_or_default(),
                                                item.get("id").and_then(|v| v.as_str()).unwrap_or_default()
                                            ),
                                            name: item.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                                            arguments: serde_json::Value::Null,
                                            thought_signature: None,
                                        }));
                                        yield AssistantMessageEvent::ToolCallStart {
                                            content_index: output.content.len() - 1,
                                            partial: output.clone(),
                                        };
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "response.reasoning_summary_part.added" => {}
                        "response.reasoning_summary_part.done" => {
                            if let Some(AssistantContent::Thinking(block)) = output.content.last_mut() {
                                block.thinking.push_str("\n\n");
                                yield AssistantMessageEvent::ThinkingDelta {
                                    content_index: output.content.len() - 1,
                                    delta: "\n\n".to_string(),
                                    partial: output.clone(),
                                };
                            }
                        }
                        "response.reasoning_summary_text.delta" => {
                            if let Some(AssistantContent::Thinking(block)) = output.content.last_mut()
                                && let Some(delta) = payload.get("delta").and_then(|v| v.as_str())
                            {
                                block.thinking.push_str(delta);
                                yield AssistantMessageEvent::ThinkingDelta {
                                    content_index: output.content.len() - 1,
                                    delta: delta.to_string(),
                                    partial: output.clone(),
                                };
                            }
                        }
                        "response.reasoning_text.delta" => {
                            if let Some(AssistantContent::Thinking(block)) = output.content.last_mut()
                                && let Some(delta) = payload.get("delta").and_then(|v| v.as_str())
                            {
                                block.thinking.push_str(delta);
                                yield AssistantMessageEvent::ThinkingDelta {
                                    content_index: output.content.len() - 1,
                                    delta: delta.to_string(),
                                    partial: output.clone(),
                                };
                            }
                        }
                        "response.content_part.added" => {
                            if let Some(CurrentResponsesItem::Message { kind, .. }) = current_item.as_mut() {
                                *kind = match payload
                                    .get("part")
                                    .and_then(|part| part.get("type"))
                                    .and_then(|value| value.as_str())
                                {
                                    Some("output_text") => Some(CurrentMessageContentKind::OutputText),
                                    Some("refusal") => Some(CurrentMessageContentKind::Refusal),
                                    _ => *kind,
                                };
                            }
                        }
                        "response.output_text.delta" => {
                            if let Some(AssistantContent::Text(block)) = output.content.last_mut()
                                && let Some(delta) = payload.get("delta").and_then(|v| v.as_str())
                            {
                                block.text.push_str(delta);
                                yield AssistantMessageEvent::TextDelta {
                                    content_index: output.content.len() - 1,
                                    delta: delta.to_string(),
                                    partial: output.clone(),
                                };
                            }
                        }
                        "response.refusal.delta" => {
                            if let Some(AssistantContent::Text(block)) = output.content.last_mut()
                                && let Some(delta) = payload.get("delta").and_then(|v| v.as_str())
                            {
                                block.text.push_str(delta);
                                yield AssistantMessageEvent::TextDelta {
                                    content_index: output.content.len() - 1,
                                    delta: delta.to_string(),
                                    partial: output.clone(),
                                };
                            }
                        }
                        "response.function_call_arguments.delta" => {
                            if let Some(AssistantContent::ToolCall(block)) = output.content.last_mut()
                                && let Some(delta) = payload.get("delta").and_then(|v| v.as_str())
                            {
                                let merged = match block.arguments {
                                    serde_json::Value::String(ref partial) => {
                                        format!("{partial}{delta}")
                                    }
                                    _ => delta.to_string(),
                                };
                                block.arguments = serde_json::Value::String(merged.clone());
                                yield AssistantMessageEvent::ToolCallDelta {
                                    content_index: output.content.len() - 1,
                                    delta: delta.to_string(),
                                    partial: output.clone(),
                                };
                            }
                        }
                        "response.function_call_arguments.done" => {
                            if let Some(AssistantContent::ToolCall(block)) = output.content.last_mut() {
                                let final_args = payload
                                    .get("arguments")
                                    .and_then(|value| value.as_str())
                                    .map(parse_streaming_json)
                                    .unwrap_or_else(|| match &block.arguments {
                                        serde_json::Value::String(raw) => parse_streaming_json(raw),
                                        other => other.clone(),
                                    });
                                block.arguments = final_args;
                            }
                        }
                        "response.output_item.done" => {
                            if let Some(item) = payload.get("item") {
                                match item.get("type").and_then(|value| value.as_str()) {
                                    Some("reasoning") => {
                                        let content_index = output.content.len().saturating_sub(1);
                                        if let Some(AssistantContent::Thinking(block)) = output.content.last_mut() {
                                            if let Some(signature) = serde_json::to_string(item).ok() {
                                                block.thinking_signature = Some(signature);
                                            }
                                            yield AssistantMessageEvent::ThinkingEnd {
                                                content_index,
                                                content: block.thinking.clone(),
                                                partial: output.clone(),
                                            };
                                        }
                                    }
                                    Some("message") => {
                                        let content_index = output.content.len().saturating_sub(1);
                                        if let Some(AssistantContent::Text(block)) = output.content.last_mut() {
                                            let full_text = item
                                                .get("content")
                                                .and_then(|content| content.as_array())
                                                .map(|parts| {
                                                    parts.iter()
                                                        .filter_map(|part| {
                                                            part.get("text")
                                                                .and_then(|value| value.as_str())
                                                                .or_else(|| part.get("refusal").and_then(|value| value.as_str()))
                                                        })
                                                        .collect::<Vec<_>>()
                                                        .join("")
                                                })
                                                .unwrap_or_else(|| block.text.clone());
                                            block.text = full_text.clone();
                                            let mut signature = TextSignatureV1::new(
                                                item.get("id")
                                                    .and_then(|value| value.as_str())
                                                    .unwrap_or_default()
                                            );
                                            if let Some(phase) = item.get("phase").and_then(|value| value.as_str()) {
                                                signature.phase = match phase {
                                                    "commentary" => Some(TextSignaturePhase::Commentary),
                                                    "final_answer" => Some(TextSignaturePhase::FinalAnswer),
                                                    _ => None,
                                                };
                                            }
                                            if !signature.id.is_empty() {
                                                block.text_signature = serde_json::to_string(&signature).ok();
                                            }
                                            yield AssistantMessageEvent::TextEnd {
                                                content_index,
                                                content: full_text,
                                                partial: output.clone(),
                                            };
                                        }
                                    }
                                    Some("function_call") => {
                                        let content_index = output.content.len().saturating_sub(1);
                                        if let Some(AssistantContent::ToolCall(block)) = output.content.last_mut() {
                                            if let Some(arguments) = item.get("arguments").and_then(|value| value.as_str()) {
                                                block.arguments = parse_streaming_json(arguments);
                                            } else if let serde_json::Value::String(raw) = &block.arguments {
                                                block.arguments = parse_streaming_json(raw);
                                            }
                                            yield AssistantMessageEvent::ToolCallEnd {
                                                content_index,
                                                tool_call: block.clone(),
                                                partial: output.clone(),
                                            };
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            current_item = None;
                        }
                        "response.completed" => {
                            if let Some(response) = payload.get("response") {
                                output.response_id = response
                                    .get("id")
                                    .and_then(|value| value.as_str())
                                    .map(ToString::to_string);
                                output.stop_reason = map_response_status(
                                    response.get("status").and_then(|value| value.as_str())
                                );
                                update_usage_from_response(&mut output, response);
                            } else {
                                output.stop_reason = StopReason::Stop;
                            }
                            if output
                                .content
                                .iter()
                                .any(|content| matches!(content, AssistantContent::ToolCall(_)))
                                && output.stop_reason == StopReason::Stop
                            {
                                output.stop_reason = StopReason::ToolUse;
                            }
                            yield AssistantMessageEvent::Done {
                                reason: output.stop_reason.clone(),
                                message: output.clone(),
                            };
                            return;
                        }
                        "response.failed" => {
                            output.error_message = payload
                                .get("response")
                                .and_then(|response| response.get("error"))
                                .and_then(|error| error.get("message"))
                                .and_then(|value| value.as_str())
                                .map(ToString::to_string);
                            output.stop_reason = StopReason::Error;
                            yield AssistantMessageEvent::Error {
                                reason: StopReason::Error,
                                error: output.clone(),
                            };
                            return;
                        }
                        "error" => {
                            output.error_message = Some(
                                payload
                                    .get("message")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or("Unknown error")
                                    .to_string()
                            );
                            output.stop_reason = StopReason::Error;
                            yield AssistantMessageEvent::Error {
                                reason: StopReason::Error,
                                error: output.clone(),
                            };
                            return;
                        }
                        _ => {}
                    }
            }
        }

        yield AssistantMessageEvent::Done { reason: StopReason::Stop, message: output };
    })
}

impl ApiProvider for OpenAiResponsesApiProvider {
    fn api(&self) -> Api {
        Api::Known(KnownApi::OpenAiResponses)
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
    ) -> AssistantMessageEventStream {
        let client = Client::new();
        let model = model.clone();
        let context = context.clone();
        let options = options.cloned().map(|base| OpenAiResponsesStreamOptions {
            base,
            ..Default::default()
        });
        let body = build_body(&model, &context, options.as_ref());
        AssistantMessageEventStream::from_raw(stream_openai_responses(
            client, model, context, options, body,
        ))
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        let client = Client::new();
        let model = model.clone();
        let context = context.clone();
        let options = options
            .cloned()
            .map(|options| OpenAiResponsesStreamOptions {
                base: options.base,
                reasoning_effort: options.reasoning.map(|level| {
                    match clamp_thinking_level(&model, level) {
                        ThinkingLevel::Off => "minimal".to_string(),
                        ThinkingLevel::Minimal => "minimal".to_string(),
                        ThinkingLevel::Low => "low".to_string(),
                        ThinkingLevel::Medium => "medium".to_string(),
                        ThinkingLevel::High => "high".to_string(),
                        ThinkingLevel::XHigh => "xhigh".to_string(),
                    }
                }),
                reasoning_summary: None,
                service_tier: None,
            });
        let body = build_body(&model, &context, options.as_ref());
        AssistantMessageEventStream::from_raw(stream_openai_responses(
            client, model, context, options, body,
        ))
    }
}

pub fn stream_openai_responses_public(
    model: &Model,
    context: &Context,
    options: Option<&StreamOptions>,
) -> AssistantMessageEventStream {
    OpenAiResponsesApiProvider.stream(model, context, options)
}

pub fn stream_simple_openai_responses(
    model: &Model,
    context: &Context,
    options: Option<&SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    OpenAiResponsesApiProvider.stream_simple(model, context, options)
}

pub(crate) fn register_openai_responses_provider() {
    crate::api_registry::register_api_provider(Arc::new(OpenAiResponsesApiProvider));
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn test_model(base_url: String) -> Model {
        Model {
            id: "gpt-5-mini".to_string(),
            name: "GPT-5 mini".to_string(),
            api: Api::Known(KnownApi::OpenAiResponses),
            provider: Provider::Known(KnownProvider::OpenAi),
            base_url,
            reasoning: true,
            thinking_level_map: None,
            input: vec!["text".to_string(), "image".to_string()],
            cost: ModelCost {
                input: 0.1,
                output: 0.2,
                cache_read: 0.01,
                cache_write: 0.0,
            },
            context_window: 128_000,
            max_tokens: 16_384,
            headers: None,
            compat: None,
        }
    }

    fn test_context() -> Context {
        Context {
            system_prompt: Some("be concise".to_string()),
            messages: vec![Message::User(UserMessage {
                role: "user".to_string(),
                content: MessageContent::Text("hello".to_string()),
                timestamp: chrono::Utc::now(),
            })],
            tools: Some(vec![Tool {
                name: "lookup".to_string(),
                description: "Lookup facts".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "q": { "type": "string" } },
                    "required": ["q"]
                }),
            }]),
        }
    }

    #[test]
    fn build_body_matches_openai_responses_shape() {
        let body = build_body(
            &test_model("https://api.openai.com/v1".to_string()),
            &test_context(),
            Some(&OpenAiResponsesStreamOptions {
                base: StreamOptions {
                    max_tokens: Some(128),
                    temperature: Some(0.4),
                    ..Default::default()
                },
                reasoning_effort: Some("medium".to_string()),
                reasoning_summary: Some("auto".to_string()),
                service_tier: Some("priority".to_string()),
            }),
        );

        assert_eq!(body["model"], serde_json::json!("gpt-5-mini"));
        assert_eq!(body["stream"], serde_json::json!(true));
        assert_eq!(body["store"], serde_json::json!(false));
        assert_eq!(body["max_output_tokens"], serde_json::json!(128));
        assert_eq!(body["temperature"], serde_json::json!(0.4));
        assert_eq!(body["service_tier"], serde_json::json!("priority"));
        assert_eq!(body["input"][0]["role"], serde_json::json!("developer"));
        assert_eq!(body["input"][0]["content"], serde_json::json!("be concise"));
        assert_eq!(body["input"][1]["role"], serde_json::json!("user"));
        assert_eq!(
            body["input"][1]["content"][0]["type"],
            serde_json::json!("input_text")
        );
        assert_eq!(body["tools"][0]["type"], serde_json::json!("function"));
        assert_eq!(body["tools"][0]["name"], serde_json::json!("lookup"));
        assert_eq!(body["reasoning"]["effort"], serde_json::json!("medium"));
        assert_eq!(body["reasoning"]["summary"], serde_json::json!("auto"));
    }

    #[tokio::test]
    async fn stream_openai_responses_emits_text_and_usage() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 16384];
            let _ = socket.read(&mut buf).await.unwrap();
            let body = concat!(
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
                "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"message\",\"id\":\"msg_1\"}}\n\n",
                "data: {\"type\":\"response.content_part.added\",\"part\":{\"type\":\"output_text\"}}\n\n",
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n",
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"lo\"}\n\n",
                "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello\"}]}}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"total_tokens\":12,\"input_tokens_details\":{\"cached_tokens\":3}}}}\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let mut stream = stream_openai_responses_public(
            &test_model(format!("http://{}", addr)),
            &test_context(),
            Some(&StreamOptions {
                api_key: Some("test-key".to_string()),
                ..Default::default()
            }),
        );

        let mut saw_text_end = false;
        let mut final_message = None;
        while let Some(event) = stream.next().await {
            match event {
                AssistantMessageEvent::TextEnd { content, .. } => {
                    assert_eq!(content, "Hello");
                    saw_text_end = true;
                }
                AssistantMessageEvent::Done { message, .. } => {
                    final_message = Some(message);
                }
                _ => {}
            }
        }

        server.await.unwrap();

        assert!(saw_text_end);
        let final_message = final_message.expect("done event");
        assert_eq!(final_message.response_id.as_deref(), Some("resp_1"));
        assert_eq!(final_message.stop_reason, StopReason::Stop);
        assert_eq!(final_message.usage.input, 7);
        assert_eq!(final_message.usage.output, 2);
        assert_eq!(final_message.usage.cache_read, 3);
        assert_eq!(final_message.usage.total_tokens, 12);
    }
}

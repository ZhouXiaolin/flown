use flown_ai::{
    Api, AssistantContent, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream,
    AbortSignal, CacheRetention, Context, Cost, Model, Provider, SimpleStreamOptions, StopReason,
    RawEventStream, TextContent, ThinkingBudgets, ThinkingContent, ThinkingLevel, ToolCall,
    Transport, Usage,
};
use futures::{FutureExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProxyAssistantMessageEvent {
    #[serde(rename = "start")]
    Start,
    #[serde(rename = "text_start")]
    TextStart {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
    },
    #[serde(rename = "text_delta")]
    TextDelta {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "text_end")]
    TextEnd {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        #[serde(
            rename = "contentSignature",
            alias = "content_signature",
            skip_serializing_if = "Option::is_none"
        )]
        content_signature: Option<String>,
    },
    #[serde(rename = "thinking_start")]
    ThinkingStart {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
    },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "thinking_end")]
    ThinkingEnd {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        #[serde(
            rename = "contentSignature",
            alias = "content_signature",
            skip_serializing_if = "Option::is_none"
        )]
        content_signature: Option<String>,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        id: String,
        #[serde(rename = "toolName", alias = "tool_name")]
        tool_name: String,
    },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        #[serde(rename = "contentIndex", alias = "content_index")]
        content_index: usize,
    },
    #[serde(rename = "done")]
    Done { reason: StopReason, usage: Usage },
    #[serde(rename = "error")]
    Error {
        reason: StopReason,
        #[serde(
            rename = "errorMessage",
            alias = "error_message",
            skip_serializing_if = "Option::is_none"
        )]
        error_message: Option<String>,
        usage: Usage,
    },
}

#[derive(Clone)]
pub struct ProxyStreamOptions {
    pub signal: Option<AbortSignal>,
    pub auth_token: String,
    pub proxy_url: String,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub reasoning: Option<ThinkingLevel>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub metadata: Option<HashMap<String, serde_json::Value>>,
    pub transport: Option<Transport>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub max_retry_delay_ms: Option<u64>,
}

impl ProxyStreamOptions {
    pub fn from_simple_stream_options(
        auth_token: impl Into<String>,
        proxy_url: impl Into<String>,
        options: SimpleStreamOptions,
    ) -> Self {
        Self {
            signal: options.base.signal,
            auth_token: auth_token.into(),
            proxy_url: proxy_url.into(),
            temperature: options.base.temperature,
            max_tokens: options.base.max_tokens,
            reasoning: options.reasoning,
            cache_retention: options.base.cache_retention,
            session_id: options.base.session_id,
            headers: options.base.headers,
            metadata: options.base.metadata,
            transport: options.base.transport,
            thinking_budgets: options.thinking_budgets,
            max_retry_delay_ms: options.base.max_retry_delay_ms,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("{0}")]
    InvalidEvent(String),
}

#[derive(Debug, Clone)]
struct ProxyToolPartial {
    tool_call: ToolCall,
    partial_json: String,
}

#[derive(Debug, Default)]
pub struct ProxyPartialState {
    tool_partials: HashMap<usize, ProxyToolPartial>,
}

pub fn create_proxy_partial(model: &Model) -> AssistantMessage {
    AssistantMessage {
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
    }
}

pub fn process_proxy_event(
    proxy_event: ProxyAssistantMessageEvent,
    partial: &mut AssistantMessage,
) -> Result<Option<AssistantMessageEvent>, ProxyError> {
    let mut state = ProxyPartialState::default();
    process_proxy_event_with_state(proxy_event, partial, &mut state)
}

pub fn process_proxy_event_with_state(
    proxy_event: ProxyAssistantMessageEvent,
    partial: &mut AssistantMessage,
    state: &mut ProxyPartialState,
) -> Result<Option<AssistantMessageEvent>, ProxyError> {
    match proxy_event {
        ProxyAssistantMessageEvent::Start => Ok(Some(AssistantMessageEvent::Start {
            partial: partial.clone(),
        })),
        ProxyAssistantMessageEvent::TextStart { content_index } => {
            set_content(
                partial,
                content_index,
                AssistantContent::Text(TextContent {
                    content_type: "text".to_string(),
                    text: String::new(),
                    text_signature: None,
                }),
            );
            Ok(Some(AssistantMessageEvent::TextStart {
                content_index,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::TextDelta {
            content_index,
            delta,
        } => {
            let Some(AssistantContent::Text(text)) = partial.content.get_mut(content_index) else {
                return Err(ProxyError::InvalidEvent(
                    "received text_delta for non-text content".to_string(),
                ));
            };
            text.text.push_str(&delta);
            Ok(Some(AssistantMessageEvent::TextDelta {
                content_index,
                delta,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::TextEnd {
            content_index,
            content_signature,
        } => {
            let Some(AssistantContent::Text(text)) = partial.content.get_mut(content_index) else {
                return Err(ProxyError::InvalidEvent(
                    "received text_end for non-text content".to_string(),
                ));
            };
            text.text_signature = content_signature;
            Ok(Some(AssistantMessageEvent::TextEnd {
                content_index,
                content: text.text.clone(),
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ThinkingStart { content_index } => {
            set_content(
                partial,
                content_index,
                AssistantContent::Thinking(ThinkingContent {
                    content_type: "thinking".to_string(),
                    thinking: String::new(),
                    thinking_signature: None,
                    redacted: None,
                }),
            );
            Ok(Some(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
        } => {
            let Some(AssistantContent::Thinking(thinking)) = partial.content.get_mut(content_index)
            else {
                return Err(ProxyError::InvalidEvent(
                    "received thinking_delta for non-thinking content".to_string(),
                ));
            };
            thinking.thinking.push_str(&delta);
            Ok(Some(AssistantMessageEvent::ThinkingDelta {
                content_index,
                delta,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ThinkingEnd {
            content_index,
            content_signature,
        } => {
            let Some(AssistantContent::Thinking(thinking)) = partial.content.get_mut(content_index)
            else {
                return Err(ProxyError::InvalidEvent(
                    "received thinking_end for non-thinking content".to_string(),
                ));
            };
            thinking.thinking_signature = content_signature;
            Ok(Some(AssistantMessageEvent::ThinkingEnd {
                content_index,
                content: thinking.thinking.clone(),
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ToolCallStart {
            content_index,
            id,
            tool_name,
        } => {
            let tool_call = ToolCall {
                content_type: "toolCall".to_string(),
                id,
                name: tool_name,
                arguments: serde_json::json!({}),
                thought_signature: None,
            };
            state.tool_partials.insert(
                content_index,
                ProxyToolPartial {
                    tool_call: tool_call.clone(),
                    partial_json: String::new(),
                },
            );
            set_content(
                partial,
                content_index,
                AssistantContent::ToolCall(tool_call),
            );
            Ok(Some(AssistantMessageEvent::ToolCallStart {
                content_index,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ToolCallDelta {
            content_index,
            delta,
        } => {
            let tool_partial =
                state
                    .tool_partials
                    .entry(content_index)
                    .or_insert_with(|| ProxyToolPartial {
                        tool_call: match partial.content.get(content_index) {
                            Some(AssistantContent::ToolCall(tool_call)) => tool_call.clone(),
                            _ => ToolCall {
                                content_type: "toolCall".to_string(),
                                id: String::new(),
                                name: String::new(),
                                arguments: serde_json::json!({}),
                                thought_signature: None,
                            },
                        },
                        partial_json: match partial.content.get(content_index) {
                            Some(AssistantContent::ToolCall(tool_call))
                                if tool_call.arguments.is_object()
                                    && !tool_call.arguments.as_object().unwrap().is_empty() =>
                            {
                                serde_json::to_string(&tool_call.arguments).unwrap_or_default()
                            }
                            _ => String::new(),
                        },
                    });
            tool_partial.partial_json.push_str(&delta);
            tool_partial.tool_call.arguments = parse_streaming_json(&tool_partial.partial_json);
            set_content(
                partial,
                content_index,
                AssistantContent::ToolCall(tool_partial.tool_call.clone()),
            );
            Ok(Some(AssistantMessageEvent::ToolCallDelta {
                content_index,
                delta,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ToolCallEnd { content_index } => {
            let tool_call = match state.tool_partials.remove(&content_index) {
                Some(tool_partial) => tool_partial.tool_call,
                None => match partial.content.get(content_index) {
                    Some(AssistantContent::ToolCall(tool_call)) => tool_call.clone(),
                    _ => {
                        return Err(ProxyError::InvalidEvent(
                            "received toolcall_end for non-toolCall content".to_string(),
                        ));
                    }
                },
            };
            set_content(
                partial,
                content_index,
                AssistantContent::ToolCall(tool_call.clone()),
            );
            Ok(Some(AssistantMessageEvent::ToolCallEnd {
                content_index,
                tool_call,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::Done { reason, usage } => {
            partial.stop_reason = reason.clone();
            partial.usage = usage;
            Ok(Some(AssistantMessageEvent::Done {
                reason,
                message: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::Error {
            reason,
            error_message,
            usage,
        } => {
            partial.stop_reason = reason.clone();
            partial.error_message = error_message;
            partial.usage = usage;
            Ok(Some(AssistantMessageEvent::Error {
                reason,
                error: partial.clone(),
            }))
        }
    }
}

pub fn stream_proxy(
    model: Model,
    context: Context,
    options: ProxyStreamOptions,
) -> AssistantMessageEventStream {
    AssistantMessageEventStream::from_stream(Box::pin(async_stream::stream! {
        let mut partial = create_proxy_partial(&model);
        let mut state = ProxyPartialState::default();
        let client = reqwest::Client::new();
        let url = format!("{}/api/stream", options.proxy_url.trim_end_matches('/'));
        let response = client
            .post(url)
            .bearer_auth(&options.auth_token)
            .json(&serde_json::json!({
                "model": model,
                "context": context,
                "options": serializable_options(&options),
            }))
            .send()
            .await;

        let response = match response {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                let status = response.status();
                partial.stop_reason = StopReason::Error;
                partial.error_message = Some(format!("Proxy error: {status}"));
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: partial,
                };
                return;
            }
            Err(error) => {
                let reason = if options
                    .signal
                    .as_ref()
                    .is_some_and(AbortSignal::is_cancelled)
                {
                    StopReason::Aborted
                } else {
                    StopReason::Error
                };
                partial.stop_reason = reason.clone();
                partial.error_message = Some(error.to_string());
                yield AssistantMessageEvent::Error {
                    reason,
                    error: partial,
                };
                return;
            }
        };

        let mut buffer = String::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = if let Some(signal) = options.signal.clone() {
            futures::pin_mut!(signal);
            futures::select! {
                _ = signal.cancelled().fuse() => {
                    partial.stop_reason = StopReason::Aborted;
                    partial.error_message = Some("Request aborted by user".to_string());
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Aborted,
                        error: partial,
                    };
                    return;
                }
                chunk = stream.next().fuse() => chunk,
            }
        } else {
            stream.next().await
        } {
            match chunk {
                Ok(bytes) => {
                    buffer.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(newline) = buffer.find('\n') {
                        let line = buffer[..newline].trim_end_matches('\r').to_string();
                        buffer = buffer[newline + 1..].to_string();
                        let Some(data) = line.strip_prefix("data: ") else {
                            continue;
                        };
                        let data = data.trim();
                        if data.is_empty() {
                            continue;
                        }
                        let Ok(proxy_event) = serde_json::from_str::<ProxyAssistantMessageEvent>(data) else {
                            continue;
                        };
                        match process_proxy_event_with_state(proxy_event, &mut partial, &mut state) {
                            Ok(Some(event)) => yield event,
                            Ok(None) => {}
                            Err(error) => {
                                partial.stop_reason = StopReason::Error;
                                partial.error_message = Some(error.to_string());
                                yield AssistantMessageEvent::Error {
                                    reason: StopReason::Error,
                                    error: partial,
                                };
                                return;
                            }
                        }
                    }
                }
                Err(error) => {
                    partial.stop_reason = StopReason::Error;
                    partial.error_message = Some(error.to_string());
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        error: partial,
                    };
                    return;
                }
            }
        }
    }))
}

fn serializable_options(options: &ProxyStreamOptions) -> serde_json::Value {
    serde_json::json!({
        "temperature": options.temperature,
        "maxTokens": options.max_tokens,
        "reasoning": options.reasoning,
        "cacheRetention": options.cache_retention,
        "sessionId": options.session_id,
        "headers": options.headers,
        "metadata": options.metadata,
        "transport": options.transport,
        "thinkingBudgets": options.thinking_budgets,
        "maxRetryDelayMs": options.max_retry_delay_ms,
    })
}

fn set_content(partial: &mut AssistantMessage, content_index: usize, content: AssistantContent) {
    if partial.content.len() <= content_index {
        partial.content.resize_with(content_index + 1, || {
            AssistantContent::Text(TextContent {
                content_type: "text".to_string(),
                text: String::new(),
                text_signature: None,
            })
        });
    }
    partial.content[content_index] = content;
}

fn parse_streaming_json(raw: &str) -> serde_json::Value {
    if raw.trim().is_empty() {
        return serde_json::json!({});
    }

    serde_json::from_str(raw)
        .or_else(|_| serde_json::from_str(&complete_partial_json(raw)))
        .unwrap_or_else(|_| serde_json::json!({}))
}

fn complete_partial_json(raw: &str) -> String {
    let mut completed = String::with_capacity(raw.len() + 8);
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for ch in raw.chars() {
        completed.push(ch);
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if stack.pop() != Some(ch) {
                    return raw.to_string();
                }
            }
            _ => {}
        }
    }

    if in_string {
        completed.push('"');
    }
    while matches!(completed.chars().last(), Some(ch) if ch.is_whitespace()) {
        completed.pop();
    }
    if completed.ends_with(',') {
        completed.pop();
    }
    while let Some(ch) = stack.pop() {
        completed.push(ch);
    }
    completed
}

use crate::types::*;
use once_cell::sync::Lazy;
use std::collections::HashMap;

/// Built-in model table, loaded at first access. Mirrors pi-ai's generated
/// model table (`MODELS` import in `models.ts`): read-only and populated from
/// `models.generated.json`. pi-ai exposes no runtime model registration, so
/// neither does this crate.
static BUILTIN_MODEL_REGISTRY: Lazy<HashMap<String, HashMap<String, Model>>> = Lazy::new(|| {
    serde_json::from_str(include_str!("models.generated.json"))
        .expect("built-in model registry JSON is valid")
});

/// Get a model by provider and model ID
pub fn get_model(provider: &str, model_id: &str) -> Option<Model> {
    BUILTIN_MODEL_REGISTRY
        .get(provider)
        .and_then(|models| models.get(model_id).cloned())
}

/// Get all providers
pub fn get_providers() -> Vec<String> {
    let mut providers: Vec<String> = BUILTIN_MODEL_REGISTRY.keys().cloned().collect();
    providers.sort();
    providers
}

/// Get all models for a provider
pub fn get_models(provider: &str) -> Vec<Model> {
    let mut models: Vec<Model> = BUILTIN_MODEL_REGISTRY
        .get(provider)
        .map(|models| models.values().cloned().collect())
        .unwrap_or_default();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models
}

/// Calculate cost for usage, matching pi-ai's `calculateCost`:
/// Anthropic charges 2× base input for 1h cache writes, so the
/// `cache_write_1h` subset is priced at `model.cost.input * 2` while the
/// remaining (short-retention) writes use `model.cost.cache_write`.
pub fn calculate_cost(model: &Model, usage: &mut Usage) -> Cost {
    let long_write = usage.cache_write_1h.unwrap_or(0) as f64;
    let short_write = (usage.cache_write as f64) - long_write;
    usage.cost = Cost {
        input: (model.cost.input / 1_000_000.0) * usage.input as f64,
        output: (model.cost.output / 1_000_000.0) * usage.output as f64,
        cache_read: (model.cost.cache_read / 1_000_000.0) * usage.cache_read as f64,
        cache_write: (model.cost.cache_write * short_write + model.cost.input * 2.0 * long_write)
            / 1_000_000.0,
        total: 0.0,
    };
    usage.cost.total =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
    usage.cost.clone()
}

const EXTENDED_THINKING_LEVELS: [ThinkingLevel; 6] = [
    ThinkingLevel::Off,
    ThinkingLevel::Minimal,
    ThinkingLevel::Low,
    ThinkingLevel::Medium,
    ThinkingLevel::High,
    ThinkingLevel::XHigh,
];

/// Return the reasoning levels supported by a model, matching pi-ai's
/// `getSupportedThinkingLevels` semantics.
pub fn get_supported_thinking_levels(model: &Model) -> Vec<ThinkingLevel> {
    if !model.reasoning {
        return vec![ThinkingLevel::Off];
    }

    EXTENDED_THINKING_LEVELS
        .iter()
        .filter(|level| {
            let mapped = model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(*level));
            if matches!(mapped, Some(None)) {
                return false;
            }
            if **level == ThinkingLevel::XHigh {
                return mapped.is_some();
            }
            true
        })
        .cloned()
        .collect()
}

/// Clamp a requested thinking level to the closest level supported by a model.
pub fn clamp_thinking_level(model: &Model, level: ThinkingLevel) -> ThinkingLevel {
    let available = get_supported_thinking_levels(model);
    if available.contains(&level) {
        return level;
    }

    let requested_index = EXTENDED_THINKING_LEVELS
        .iter()
        .position(|candidate| *candidate == level);
    let Some(requested_index) = requested_index else {
        return available.first().cloned().unwrap_or(ThinkingLevel::Off);
    };

    for candidate in EXTENDED_THINKING_LEVELS.iter().skip(requested_index + 1) {
        if available.contains(candidate) {
            return candidate.clone();
        }
    }
    for candidate in EXTENDED_THINKING_LEVELS[..requested_index].iter().rev() {
        if available.contains(candidate) {
            return candidate.clone();
        }
    }

    available.first().cloned().unwrap_or(ThinkingLevel::Off)
}

/// Check model identity by provider and id, matching pi-ai's `modelsAreEqual`.
pub fn models_are_equal(left: Option<&Model>, right: Option<&Model>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    left.id == right.id && left.provider == right.provider
}

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";

fn text_block(text: impl Into<String>) -> TextContent {
    TextContent {
        content_type: "text".to_string(),
        text: text.into(),
        text_signature: None,
    }
}

fn replace_user_images_with_placeholder(
    content: &[UserContentBlock],
    placeholder: &str,
) -> Vec<UserContentBlock> {
    let mut result = Vec::new();
    let mut previous_was_placeholder = false;

    for block in content {
        match block {
            UserContentBlock::Image(_) => {
                if !previous_was_placeholder {
                    result.push(UserContentBlock::Text(text_block(placeholder)));
                }
                previous_was_placeholder = true;
            }
            UserContentBlock::Text(text) => {
                previous_was_placeholder = text.text == placeholder;
                result.push(UserContentBlock::Text(text.clone()));
            }
        }
    }

    result
}

fn replace_tool_images_with_placeholder(
    content: &[ToolResultContent],
    placeholder: &str,
) -> Vec<ToolResultContent> {
    let mut result = Vec::new();
    let mut previous_was_placeholder = false;

    for block in content {
        match block {
            ToolResultContent::Image(_) => {
                if !previous_was_placeholder {
                    result.push(ToolResultContent::Text(text_block(placeholder)));
                }
                previous_was_placeholder = true;
            }
            ToolResultContent::Text(text) => {
                previous_was_placeholder = text.text == placeholder;
                result.push(ToolResultContent::Text(text.clone()));
            }
        }
    }

    result
}

fn downgrade_unsupported_images(messages: &[Message], model: &Model) -> Vec<Message> {
    if model.input.iter().any(|input| input == "image") {
        return messages.to_vec();
    }

    messages
        .iter()
        .map(|message| match message {
            Message::User(user) => {
                let mut user = user.clone();
                if let MessageContent::Blocks(blocks) = &user.content {
                    user.content = MessageContent::Blocks(replace_user_images_with_placeholder(
                        blocks,
                        NON_VISION_USER_IMAGE_PLACEHOLDER,
                    ));
                }
                Message::User(user)
            }
            Message::ToolResult(result) => {
                let mut result = result.clone();
                result.content = replace_tool_images_with_placeholder(
                    &result.content,
                    NON_VISION_TOOL_IMAGE_PLACEHOLDER,
                );
                Message::ToolResult(result)
            }
            Message::Assistant(_) => message.clone(),
        })
        .collect()
}

/// Normalize conversation history before provider-specific conversion.
///
/// This mirrors pi-ai's `transformMessages` (`providers/transform-messages.ts`):
/// image downgrades for text-only models, cross-model thinking/tool-call
/// cleanup, optional tool-call id normalization, and synthetic tool results
/// for orphaned tool calls.
///
/// Internal — pi-ai keeps `transformMessages` as a provider-internal helper
/// and does not export it, so neither does this crate. Built-in providers call
/// it directly.
pub(crate) fn transform_messages(
    messages: &[Message],
    model: &Model,
    normalize_tool_call_id: Option<&dyn Fn(&str, &Model, &AssistantMessage) -> String>,
) -> Vec<Message> {
    let mut tool_call_id_map = HashMap::<String, String>::new();
    let image_aware_messages = downgrade_unsupported_images(messages, model);

    let transformed: Vec<Message> = image_aware_messages
        .into_iter()
        .map(|message| match message {
            Message::User(_) => message,
            Message::ToolResult(mut result) => {
                if let Some(normalized) = tool_call_id_map.get(&result.tool_call_id) {
                    result.tool_call_id = normalized.clone();
                }
                Message::ToolResult(result)
            }
            Message::Assistant(mut assistant) => {
                let is_same_model = assistant.provider == model.provider
                    && assistant.api == model.api
                    && assistant.model == model.id;
                let source_assistant = assistant.clone();
                let source_content = std::mem::take(&mut assistant.content);
                let mut content = Vec::new();

                for block in source_content {
                    match block {
                        AssistantContent::Thinking(thinking) => {
                            if thinking.redacted.unwrap_or(false) {
                                if is_same_model {
                                    content.push(AssistantContent::Thinking(thinking));
                                }
                                continue;
                            }
                            if is_same_model && thinking.thinking_signature.is_some() {
                                content.push(AssistantContent::Thinking(thinking));
                            } else if !thinking.thinking.trim().is_empty() {
                                content.push(AssistantContent::Text(text_block(thinking.thinking)));
                            }
                        }
                        AssistantContent::Text(text) => {
                            content.push(AssistantContent::Text(text));
                        }
                        AssistantContent::ToolCall(mut tool_call) => {
                            if !is_same_model {
                                tool_call.thought_signature = None;
                                if let Some(normalize) = normalize_tool_call_id {
                                    let normalized =
                                        normalize(&tool_call.id, model, &source_assistant);
                                    if normalized != tool_call.id {
                                        tool_call_id_map
                                            .insert(tool_call.id.clone(), normalized.clone());
                                        tool_call.id = normalized;
                                    }
                                }
                            }
                            content.push(AssistantContent::ToolCall(tool_call));
                        }
                    }
                }

                assistant.content = content;
                Message::Assistant(assistant)
            }
        })
        .collect();

    let mut result = Vec::new();
    let mut pending_tool_calls = Vec::<ToolCall>::new();
    let mut existing_tool_result_ids = std::collections::HashSet::<String>::new();

    fn insert_synthetic_tool_results(
        result: &mut Vec<Message>,
        pending_tool_calls: &mut Vec<ToolCall>,
        existing_tool_result_ids: &mut std::collections::HashSet<String>,
    ) {
        if pending_tool_calls.is_empty() {
            return;
        }

        for tool_call in pending_tool_calls.drain(..) {
            if !existing_tool_result_ids.contains(&tool_call.id) {
                result.push(Message::ToolResult(ToolResultMessage {
                    role: "toolResult".to_string(),
                    tool_call_id: tool_call.id,
                    tool_name: tool_call.name,
                    content: vec![ToolResultContent::Text(text_block("No result provided"))],
                    details: serde_json::Value::Null,
                    is_error: true,
                    timestamp: chrono::Utc::now(),
                }));
            }
        }
        existing_tool_result_ids.clear();
    }

    for message in transformed {
        match &message {
            Message::Assistant(assistant) => {
                insert_synthetic_tool_results(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                );

                if matches!(
                    assistant.stop_reason,
                    StopReason::Error | StopReason::Aborted
                ) {
                    continue;
                }

                let tool_calls: Vec<_> = assistant
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        AssistantContent::ToolCall(tool_call) => Some(tool_call.clone()),
                        _ => None,
                    })
                    .collect();
                if !tool_calls.is_empty() {
                    pending_tool_calls = tool_calls;
                    existing_tool_result_ids.clear();
                }

                result.push(message);
            }
            Message::ToolResult(result_message) => {
                existing_tool_result_ids.insert(result_message.tool_call_id.clone());
                result.push(message);
            }
            Message::User(_) => {
                insert_synthetic_tool_results(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                );
                result.push(message);
            }
        }
    }

    insert_synthetic_tool_results(
        &mut result,
        &mut pending_tool_calls,
        &mut existing_tool_result_ids,
    );

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AssistantContent, ImageContent, TextContent, ThinkingContent, ToolCall, ToolResultContent,
        UserContentBlock, UserMessage,
    };

    /// `transform_messages` is provider-internal (mirroring pi-ai's unexported
    /// `providers/transform-messages.ts`), so its semantics are covered here at
    /// crate visibility rather than in the external alignment test.
    #[test]
    fn transform_messages_matches_pi_ai_cross_provider_semantics() {
        let target = get_model("deepseek", "deepseek-v4-flash").expect("model");
        let source_assistant = AssistantMessage {
            role: "assistant".to_string(),
            content: vec![
                AssistantContent::Thinking(ThinkingContent {
                    content_type: "thinking".to_string(),
                    thinking: "private plan".to_string(),
                    thinking_signature: Some("foreign-thinking".to_string()),
                    redacted: None,
                }),
                AssistantContent::Thinking(ThinkingContent {
                    content_type: "thinking".to_string(),
                    thinking: "[redacted]".to_string(),
                    thinking_signature: Some("opaque".to_string()),
                    redacted: Some(true),
                }),
                AssistantContent::ToolCall(ToolCall {
                    content_type: "toolCall".to_string(),
                    id: "call|unsafe/id".to_string(),
                    name: "lookup".to_string(),
                    arguments: serde_json::json!({ "q": "rust" }),
                    thought_signature: Some("foreign-tool".to_string()),
                }),
            ],
            api: Api::Known(KnownApi::AnthropicMessages),
            provider: Provider::Known(KnownProvider::Anthropic),
            model: "claude".to_string(),
            response_model: None,
            response_id: None,
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            error_message: None,
            diagnostics: None,
            timestamp: chrono::Utc::now(),
        };
        let messages = vec![
            Message::User(UserMessage {
                role: "user".to_string(),
                content: MessageContent::Blocks(vec![
                    UserContentBlock::Image(ImageContent {
                        content_type: "image".to_string(),
                        data: "abc".to_string(),
                        mime_type: "image/png".to_string(),
                    }),
                    UserContentBlock::Image(ImageContent {
                        content_type: "image".to_string(),
                        data: "def".to_string(),
                        mime_type: "image/png".to_string(),
                    }),
                ]),
                timestamp: chrono::Utc::now(),
            }),
            Message::Assistant(source_assistant),
        ];

        let transformed = transform_messages(
            &messages,
            &target,
            Some(&|id, _model, _source| id.replace(['|', '/'], "_")),
        );

        assert!(matches!(
            &transformed[0],
            Message::User(UserMessage {
                content: MessageContent::Blocks(blocks),
                ..
            }) if blocks.len() == 1
                && matches!(
                    &blocks[0],
                    UserContentBlock::Text(TextContent { text, .. })
                        if text == "(image omitted: model does not support images)"
                )
        ));
        assert!(matches!(
            &transformed[1],
            Message::Assistant(AssistantMessage { content, .. })
                if matches!(
                    content.as_slice(),
                    [
                        AssistantContent::Text(TextContent { text, .. }),
                        AssistantContent::ToolCall(ToolCall { id, thought_signature, .. }),
                    ] if text == "private plan"
                        && id == "call_unsafe_id"
                        && thought_signature.is_none()
                )
        ));
        assert!(matches!(
            &transformed[2],
            Message::ToolResult(result)
                if result.tool_call_id == "call_unsafe_id"
                    && result.tool_name == "lookup"
                    && result.is_error
                    && matches!(
                        result.content.as_slice(),
                        [ToolResultContent::Text(TextContent { text, .. })]
                            if text == "No result provided"
                    )
        ));
    }
}

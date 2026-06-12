use crate::types::{
    CacheRetention, Message, MessageContent, Model, Provider, ToolResultContent, UserContentBlock,
};
use std::collections::HashMap;

pub(crate) fn resolve_cache_retention(cache_retention: Option<&CacheRetention>) -> CacheRetention {
    if let Some(cache_retention) = cache_retention {
        return cache_retention.clone();
    }

    if std::env::var("PI_CACHE_RETENTION").ok().as_deref() == Some("long") {
        CacheRetention::Long
    } else {
        CacheRetention::Short
    }
}

pub(crate) fn is_cloudflare_provider(model: &Model) -> bool {
    matches!(
        &model.provider,
        Provider::Known(crate::types::KnownProvider::CloudflareWorkersAi)
            | Provider::Known(crate::types::KnownProvider::CloudflareAiGateway)
    )
}

pub(crate) fn is_cloudflare_ai_gateway(model: &Model) -> bool {
    matches!(
        &model.provider,
        Provider::Known(crate::types::KnownProvider::CloudflareAiGateway)
    )
}

pub(crate) fn resolve_cloudflare_base_url(model: &Model) -> Result<String, String> {
    let url = &model.base_url;
    if !url.contains('{') {
        return Ok(url.clone());
    }

    let mut resolved = String::with_capacity(url.len());
    let mut cursor = 0;
    while let Some(start) = url[cursor..].find('{') {
        let start = cursor + start;
        resolved.push_str(&url[cursor..start]);
        let Some(end_offset) = url[start + 1..].find('}') else {
            resolved.push_str(&url[start..]);
            return Ok(resolved);
        };
        let end = start + 1 + end_offset;
        let name = &url[start + 1..end];
        if is_env_placeholder(name) {
            let value = std::env::var(name).map_err(|_| {
                format!(
                    "{name} is required for provider {} but is not set.",
                    model.provider
                )
            })?;
            resolved.push_str(&value);
        } else {
            resolved.push_str(&url[start..=end]);
        }
        cursor = end + 1;
    }
    resolved.push_str(&url[cursor..]);
    Ok(resolved)
}

fn is_env_placeholder(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_uppercase() || first == '_')
        && chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

pub(crate) fn build_copilot_dynamic_headers(messages: &[Message]) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    headers.insert(
        "X-Initiator".to_string(),
        if infer_copilot_initiator(messages) == "agent" {
            "agent"
        } else {
            "user"
        }
        .to_string(),
    );
    headers.insert(
        "Openai-Intent".to_string(),
        "conversation-edits".to_string(),
    );
    if has_copilot_vision_input(messages) {
        headers.insert("Copilot-Vision-Request".to_string(), "true".to_string());
    }
    headers
}

fn infer_copilot_initiator(messages: &[Message]) -> &'static str {
    match messages.last() {
        Some(Message::User(_)) | None => "user",
        Some(Message::Assistant(_) | Message::ToolResult(_)) => "agent",
    }
}

fn has_copilot_vision_input(messages: &[Message]) -> bool {
    messages.iter().any(|message| match message {
        Message::User(user) => match &user.content {
            MessageContent::Text(_) => false,
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .any(|block| matches!(block, UserContentBlock::Image(_))),
        },
        Message::ToolResult(result) => result
            .content
            .iter()
            .any(|content| matches!(content, ToolResultContent::Image(_))),
        Message::Assistant(_) => false,
    })
}

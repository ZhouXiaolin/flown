use flown_ai::{
    Api, AssistantContent, AssistantMessage, ImageContent, KnownApi, KnownProvider, MessageContent,
    Provider, StopReason, TextContent, ThinkingContent, ToolCall, ToolResultContent,
    ToolResultMessage, Usage, UserMessage,
};
use serde::{Deserialize, Serialize};

/// Session tree entry base fields
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTreeEntryBase {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub id: String,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub timestamp: String,
}

/// Payload for `active_tools_change` entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveToolsChange {
    #[serde(rename = "activeToolNames")]
    pub active_tool_names: Vec<String>,
}

/// Wrapper for AgentMessage that handles serialization without duplicate role fields
#[derive(Debug, Clone)]
pub struct SessionMessage(pub crate::types::AgentMessage);

impl Serialize for SessionMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let msg = &self.0;
        match msg {
            crate::types::AgentMessage::User(m) => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("role", "user")?;
                map.serialize_entry("content", &m.content)?;
                map.serialize_entry("timestamp", &m.timestamp)?;
                map.end()
            }
            crate::types::AgentMessage::Assistant(m) => {
                let mut map = serializer.serialize_map(Some(9))?;
                map.serialize_entry("role", "assistant")?;
                // Serialize content blocks without the enum tag to avoid duplicate "type" fields
                let content_blocks: Vec<serde_json::Value> = m
                    .content
                    .iter()
                    .map(|c| match c {
                        AssistantContent::Text(t) => {
                            serde_json::json!({
                                "type": "text",
                                "text": t.text,
                                "textSignature": t.text_signature,
                            })
                        }
                        AssistantContent::Thinking(t) => {
                            serde_json::json!({
                                "type": "thinking",
                                "thinking": t.thinking,
                                "thinkingSignature": t.thinking_signature,
                                "redacted": t.redacted,
                            })
                        }
                        AssistantContent::ToolCall(tc) => {
                            serde_json::json!({
                                "type": "toolCall",
                                "id": tc.id,
                                "name": tc.name,
                                "arguments": tc.arguments,
                            })
                        }
                    })
                    .collect();
                map.serialize_entry("content", &content_blocks)?;
                map.serialize_entry("api", &m.api)?;
                map.serialize_entry("provider", &m.provider)?;
                map.serialize_entry("model", &m.model)?;
                map.serialize_entry("usage", &m.usage)?;
                map.serialize_entry("stopReason", &m.stop_reason)?;
                map.serialize_entry("timestamp", &m.timestamp)?;
                if let Some(err) = &m.error_message {
                    map.serialize_entry("errorMessage", err)?;
                }
                map.end()
            }
            crate::types::AgentMessage::ToolResult(m) => {
                let mut map = serializer.serialize_map(Some(7))?;
                map.serialize_entry("role", "toolResult")?;
                map.serialize_entry("toolCallId", &m.tool_call_id)?;
                map.serialize_entry("toolName", &m.tool_name)?;
                let content_blocks = tool_result_content_to_json(&m.content);
                map.serialize_entry("content", &content_blocks)?;
                map.serialize_entry("details", &m.details)?;
                map.serialize_entry("isError", &m.is_error)?;
                map.serialize_entry("timestamp", &m.timestamp)?;
                map.end()
            }
            crate::types::AgentMessage::Custom(m) => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("role", "custom")?;
                map.serialize_entry("customType", &m.custom_type)?;
                map.serialize_entry("content", &m.content)?;
                map.serialize_entry("timestamp", &m.timestamp)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for SessionMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let role = value.get("role").and_then(|v| v.as_str()).unwrap_or("");

        match role {
            "user" => {
                let msg: UserMessage =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(SessionMessage(crate::types::AgentMessage::User(msg)))
            }
            "assistant" => {
                // Custom deserialization for assistant message
                let api = value
                    .get("api")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or(Api::Known(KnownApi::OpenAiCompletions));
                let provider = value
                    .get("provider")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or(Provider::Known(KnownProvider::OpenAi));
                let model = value
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let usage = value
                    .get("usage")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let stop_reason = value
                    .get("stopReason")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or(StopReason::Stop);
                let timestamp = value
                    .get("timestamp")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_else(|| chrono::Utc::now());
                let error_message = value
                    .get("errorMessage")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                // Parse content blocks
                let content = if let Some(content_arr) =
                    value.get("content").and_then(|v| v.as_array())
                {
                    content_arr
                        .iter()
                        .filter_map(|block| {
                            let block_type = block.get("type").and_then(|v| v.as_str())?;
                            match block_type {
                                "text" => {
                                    let text = block
                                        .get("text")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let text_signature = block
                                        .get("textSignature")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                    Some(AssistantContent::Text(TextContent {
                                        content_type: "text".to_string(),
                                        text,
                                        text_signature,
                                    }))
                                }
                                "thinking" => {
                                    let thinking = block
                                        .get("thinking")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let thinking_signature = block
                                        .get("thinkingSignature")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                    let redacted = block.get("redacted").and_then(|v| v.as_bool());
                                    Some(AssistantContent::Thinking(ThinkingContent {
                                        content_type: "thinking".to_string(),
                                        thinking,
                                        thinking_signature,
                                        redacted,
                                    }))
                                }
                                "toolCall" => {
                                    let id = block
                                        .get("id")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let name = block
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let arguments = block
                                        .get("arguments")
                                        .cloned()
                                        .unwrap_or(serde_json::Value::Null);
                                    Some(AssistantContent::ToolCall(ToolCall {
                                        content_type: "toolCall".to_string(),
                                        id,
                                        name,
                                        arguments,
                                        thought_signature: None,
                                    }))
                                }
                                _ => None,
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                Ok(SessionMessage(crate::types::AgentMessage::Assistant(
                    AssistantMessage {
                        role: "assistant".to_string(),
                        content,
                        api,
                        provider,
                        model,
                        response_model: None,
                        response_id: None,
                        usage,
                        stop_reason,
                        error_message,
                        diagnostics: None,
                        timestamp,
                    },
                )))
            }
            "toolResult" => {
                let tool_call_id = value
                    .get("toolCallId")
                    .or_else(|| value.get("tool_call_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_name = value
                    .get("toolName")
                    .or_else(|| value.get("tool_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = value
                    .get("content")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(tool_result_content_from_json)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let details = value
                    .get("details")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let is_error = value
                    .get("isError")
                    .or_else(|| value.get("is_error"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let timestamp = value
                    .get("timestamp")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_else(|| chrono::Utc::now());
                Ok(SessionMessage(crate::types::AgentMessage::ToolResult(
                    ToolResultMessage {
                        role: "toolResult".to_string(),
                        tool_call_id,
                        tool_name,
                        content,
                        details,
                        is_error,
                        timestamp,
                    },
                )))
            }
            _ => Err(serde::de::Error::custom(format!("unknown role: {}", role))),
        }
    }
}

fn tool_result_content_to_json(content: &[ToolResultContent]) -> Vec<serde_json::Value> {
    content
        .iter()
        .map(|block| match block {
            ToolResultContent::Text(text) => serde_json::json!({
                "type": "text",
                "text": text.text,
                "textSignature": text.text_signature,
            }),
            ToolResultContent::Image(image) => serde_json::json!({
                "type": "image",
                "data": image.data,
                "mimeType": image.mime_type,
            }),
        })
        .collect()
}

fn tool_result_content_from_json(block: &serde_json::Value) -> Option<ToolResultContent> {
    match block.get("type").and_then(|v| v.as_str())? {
        "text" => Some(ToolResultContent::Text(TextContent {
            content_type: "text".to_string(),
            text: block
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            text_signature: block
                .get("textSignature")
                .or_else(|| block.get("text_signature"))
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
        })),
        "image" => Some(ToolResultContent::Image(ImageContent {
            content_type: "image".to_string(),
            data: block
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            mime_type: block
                .get("mimeType")
                .or_else(|| block.get("mime_type"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })),
        _ => None,
    }
}

/// All session tree entry variants
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SessionTreeEntry {
    #[serde(rename = "message")]
    Message {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        message: SessionMessage,
    },
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "thinkingLevel")]
        thinking_level: String,
    },
    #[serde(rename = "model_change")]
    ModelChange {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
    },
    #[serde(rename = "active_tools_change")]
    ActiveToolsChange {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "activeToolNames")]
        active_tool_names: Vec<String>,
    },
    #[serde(rename = "compaction")]
    Compaction {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        summary: String,
        #[serde(rename = "firstKeptEntryId")]
        first_kept_entry_id: String,
        #[serde(rename = "tokensBefore")]
        tokens_before: u64,
        details: Option<serde_json::Value>,
        #[serde(rename = "fromHook")]
        from_hook: Option<bool>,
    },
    #[serde(rename = "branch_summary")]
    BranchSummary {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "fromId")]
        from_id: String,
        summary: String,
        details: Option<serde_json::Value>,
        #[serde(rename = "fromHook")]
        from_hook: Option<bool>,
    },
    #[serde(rename = "custom_message")]
    CustomMessage {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "customType")]
        custom_type: String,
        content: CustomMessageContent,
        display: bool,
        details: Option<serde_json::Value>,
    },
    #[serde(rename = "label")]
    Label {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "targetId")]
        target_id: String,
        label: Option<String>,
    },
    #[serde(rename = "session_info")]
    SessionInfo {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        name: Option<String>,
    },
    #[serde(rename = "custom")]
    Custom {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "customType")]
        custom_type: String,
        data: serde_json::Value,
    },
    #[serde(rename = "leaf")]
    Leaf {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "targetId")]
        target_id: Option<String>,
    },
}

/// Custom message content (text or content blocks)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CustomMessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// Content block for custom messages
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "contentType")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text(TextContent),
    #[serde(rename = "image")]
    Image(ImageContent),
}

impl SessionTreeEntry {
    pub fn id(&self) -> &str {
        match self {
            Self::Message { id, .. }
            | Self::ThinkingLevelChange { id, .. }
            | Self::ModelChange { id, .. }
            | Self::ActiveToolsChange { id, .. }
            | Self::Compaction { id, .. }
            | Self::BranchSummary { id, .. }
            | Self::CustomMessage { id, .. }
            | Self::Custom { id, .. }
            | Self::Label { id, .. }
            | Self::SessionInfo { id, .. }
            | Self::Leaf { id, .. } => id,
        }
    }

    pub fn parent_id(&self) -> Option<&str> {
        match self {
            Self::Message { parent_id, .. }
            | Self::ThinkingLevelChange { parent_id, .. }
            | Self::ModelChange { parent_id, .. }
            | Self::ActiveToolsChange { parent_id, .. }
            | Self::Compaction { parent_id, .. }
            | Self::BranchSummary { parent_id, .. }
            | Self::CustomMessage { parent_id, .. }
            | Self::Custom { parent_id, .. }
            | Self::Label { parent_id, .. }
            | Self::SessionInfo { parent_id, .. }
            | Self::Leaf { parent_id, .. } => parent_id.as_deref(),
        }
    }

    pub fn timestamp(&self) -> &str {
        match self {
            Self::Message { timestamp, .. }
            | Self::ThinkingLevelChange { timestamp, .. }
            | Self::ModelChange { timestamp, .. }
            | Self::ActiveToolsChange { timestamp, .. }
            | Self::Compaction { timestamp, .. }
            | Self::BranchSummary { timestamp, .. }
            | Self::CustomMessage { timestamp, .. }
            | Self::Custom { timestamp, .. }
            | Self::Label { timestamp, .. }
            | Self::SessionInfo { timestamp, .. }
            | Self::Leaf { timestamp, .. } => timestamp,
        }
    }

    /// Returns the effective leaf ID after this entry.
    /// For Leaf entries, returns the target_id (navigation pointer).
    /// For all others, returns the entry's own id.
    pub fn leaf_id_after(&self) -> Option<String> {
        match self {
            Self::Leaf { target_id, .. } => target_id.clone(),
            _ => Some(self.id().to_string()),
        }
    }

    /// Returns the entry type as a string
    pub fn entry_type(&self) -> &str {
        match self {
            Self::Message { .. } => "message",
            Self::ThinkingLevelChange { .. } => "thinking_level_change",
            Self::ModelChange { .. } => "model_change",
            Self::ActiveToolsChange { .. } => "active_tools_change",
            Self::Compaction { .. } => "compaction",
            Self::BranchSummary { .. } => "branch_summary",
            Self::CustomMessage { .. } => "custom_message",
            Self::Custom { .. } => "custom",
            Self::Label { .. } => "label",
            Self::SessionInfo { .. } => "session_info",
            Self::Leaf { .. } => "leaf",
        }
    }
}

/// Session context built from walking the tree
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub messages: Vec<crate::types::AgentMessage>,
    pub thinking_level: String,
    pub model: Option<(String, String)>, // (provider, model_id)
    pub active_tool_names: Option<Vec<String>>,
}

/// Session metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

/// JSONL session metadata with file path info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonlSessionMetadata {
    #[serde(flatten)]
    pub base: SessionMetadata,
    pub cwd: String,
    pub path: String,
    #[serde(rename = "parentSessionPath")]
    pub parent_session_path: Option<String>,
}

/// Session fork options
#[derive(Debug, Clone, Default)]
pub struct SessionForkOptions {
    pub entry_id: Option<String>,
    pub position: Option<ForkPosition>,
}

#[derive(Debug, Clone)]
pub enum ForkPosition {
    At,
    Before,
}

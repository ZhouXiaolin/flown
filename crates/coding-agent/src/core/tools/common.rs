use flown_agent::types::{AgentToolError, AgentToolResult};
use flown_ai::types::{TextContent, ToolResultContent};
use serde_json::Value;

pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;

pub fn required_string(args: &Value, field: &str) -> Result<String, AgentToolError> {
    optional_string(args, field)?.ok_or_else(|| AgentToolError::new(format!("missing `{field}`")))
}

pub fn optional_string(args: &Value, field: &str) -> Result<Option<String>, AgentToolError> {
    match args.get(field) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(AgentToolError::new(format!("`{field}` must be a string"))),
        None => Ok(None),
    }
}

pub fn optional_usize(args: &Value, field: &str) -> Result<Option<usize>, AgentToolError> {
    match optional_u64(args, field)? {
        Some(value) => usize::try_from(value)
            .map(Some)
            .map_err(|_| AgentToolError::new(format!("`{field}` is too large"))),
        None => Ok(None),
    }
}

pub fn optional_u64(args: &Value, field: &str) -> Result<Option<u64>, AgentToolError> {
    match args.get(field) {
        Some(Value::Number(value)) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| AgentToolError::new(format!("`{field}` must be a positive integer"))),
        Some(_) => Err(AgentToolError::new(format!(
            "`{field}` must be a positive integer"
        ))),
        None => Ok(None),
    }
}

pub fn text_result(text: impl Into<String>, details: Value) -> AgentToolResult {
    AgentToolResult {
        content: vec![text_block(text)],
        details,
        terminate: None,
    }
}

pub fn text_block(text: impl Into<String>) -> ToolResultContent {
    ToolResultContent::Text(TextContent {
        content_type: "text".to_string(),
        text: text.into(),
        text_signature: None,
    })
}

pub fn tool_error(error: impl std::fmt::Display) -> AgentToolError {
    AgentToolError::new(error.to_string())
}

pub fn append_status(text: &str, status: &str) -> String {
    if text.is_empty() {
        status.to_string()
    } else {
        format!("{text}\n\n{status}")
    }
}

pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

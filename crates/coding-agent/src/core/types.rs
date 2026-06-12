use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MCP server connection status
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpServerStatus {
    Connected,
    Disconnected,
    Error,
}

/// Information about an MCP server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub status: McpServerStatus,
    pub command: String,
    pub args: Vec<String>,
    pub tool_count: usize,
    pub error: Option<String>,
}

/// A tool exposed by any source (built-in or MCP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub label: String,
    pub description: String,
    pub input_schema: Value,
    pub source: Option<String>,
}

/// Workflow source specification
#[derive(Debug, Clone)]
pub enum WorkflowSource {
    Path { path: String },
    Inline { code: String },
    Named { name: String },
}

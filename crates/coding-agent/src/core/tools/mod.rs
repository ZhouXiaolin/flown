mod bash;
mod common;
mod edit;
mod read;
mod write;

use std::sync::Arc;

use flown_agent::harness::env::types::ExecutionEnv;
use flown_agent::types::{AgentTool, AgentToolError, AgentToolResult, ToolExecutionMode};
use serde_json::{Value, json};

use crate::core::mcp::McpManager;

pub fn built_in_coding_tools(
    env: Arc<dyn ExecutionEnv>,
    mcp: Option<Arc<tokio::sync::Mutex<McpManager>>>,
) -> Vec<AgentTool> {
    let mut tools = vec![
        read::tool(env.clone()),
        bash::tool(env.clone()),
        edit::tool(env.clone()),
        write::tool(env),
    ];

    // Add MCP tools
    if let Some(mcp) = mcp {
        tools.extend(mcp_tools(mcp));
    }

    tools
}

/// Convert MCP tools to AgentTool instances.
fn mcp_tools(mcp: Arc<tokio::sync::Mutex<McpManager>>) -> Vec<AgentTool> {
    // We need to get the tool infos synchronously for tool registration
    // The actual call_tool will be async
    let tool_infos = {
        let manager = match mcp.try_lock() {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };
        manager.tool_infos()
    };

    tool_infos
        .into_iter()
        .map(|info| {
            let mcp = mcp.clone();
            let tool_name = info.name.clone();
            let description = info.description.clone();
            let parameters = info.input_schema.clone();

            AgentTool {
                name: info.name,
                label: info.label,
                description,
                parameters,
                execute: Arc::new(move |_id, args, _abort, _update| {
                    let mcp = mcp.clone();
                    let tool_name = tool_name.clone();
                    Box::pin(async move {
                        let result = {
                            let mut manager = mcp.lock().await;
                            manager.call_tool(&tool_name, args).await
                        };

                        match result {
                            Ok(output) => Ok(AgentToolResult {
                                content: vec![flown_ai::types::ToolResultContent::Text(
                                    flown_ai::types::TextContent {
                                        content_type: "text".to_string(),
                                        text: output,
                                        text_signature: None,
                                    },
                                )],
                                details: Value::Null,
                                terminate: None,
                            }),
                            Err(e) => Err(AgentToolError::new(format!("MCP error: {e}"))),
                        }
                    })
                }),
                prepare_arguments: None,
                execution_mode: Some(ToolExecutionMode::Sequential),
            }
        })
        .collect()
}

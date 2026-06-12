mod bash;
mod common;
mod edit;
mod read;
mod write;

use std::sync::Arc;

use flown_agent::harness::env::types::ExecutionEnv;
use flown_agent::types::AgentTool;

use crate::core::mcp::McpManager;

pub fn built_in_coding_tools(
    env: Arc<dyn ExecutionEnv>,
    mcp: Option<Arc<tokio::sync::Mutex<McpManager>>>,
) -> Vec<AgentTool> {
    vec![
        read::tool(env.clone(), mcp),
        bash::tool(env.clone()),
        edit::tool(env.clone()),
        write::tool(env),
    ]
}

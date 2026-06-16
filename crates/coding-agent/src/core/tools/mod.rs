mod bash;
mod common;
mod edit;
mod read;
mod write;

use std::sync::Arc;

use flown_agent::harness::env::types::ExecutionEnv;
use flown_agent::types::AgentTool;

pub fn built_in_coding_tools(env: Arc<dyn ExecutionEnv>) -> Vec<AgentTool> {
    vec![
        read::tool(env.clone()),
        bash::tool(env.clone()),
        edit::tool(env.clone()),
        write::tool(env),
    ]
}


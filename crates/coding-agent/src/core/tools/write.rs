use std::sync::Arc;

use flown_agent::harness::env::types::ExecutionEnv;
use flown_agent::types::{AgentTool, ToolExecutionMode};
use serde_json::json;

use serde_json::Value;

use super::common::*;

pub fn tool(env: Arc<dyn ExecutionEnv>) -> AgentTool {
    AgentTool {
        name: "write".to_string(),
        label: "Write".to_string(),
        description: "Create or overwrite a UTF-8 text file through the session filesystem"
            .to_string(),
        parameters: json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            }
        }),
        execute: Arc::new(move |_id, args, _abort, _update| {
            let env = env.clone();
            Box::pin(async move {
                let path = required_string(&args, "path")?;
                let content = required_string(&args, "content")?;
                let resolved_path = env.absolute_path(&path).map_err(tool_error)?;

                if let Some(parent) = std::path::Path::new(&resolved_path).parent()
                    && !parent.as_os_str().is_empty()
                {
                    env.create_dir(&parent.to_string_lossy(), true)
                        .await
                        .map_err(tool_error)?;
                }
                env.write_file(&resolved_path, content.as_bytes())
                    .await
                    .map_err(tool_error)?;

                Ok(text_result(
                    format!("Successfully wrote {} bytes to {path}", content.len()),
                    Value::Null,
                ))
            })
        }),
        prepare_arguments: None,
        execution_mode: Some(ToolExecutionMode::Sequential),
    }
}

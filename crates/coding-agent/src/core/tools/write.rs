use std::sync::Arc;

use flown_agent::{AgentTool, ExecutionEnv, ToolExecutionMode};
use serde_json::json;

use serde_json::Value;

use super::common::*;

pub fn tool(env: Arc<dyn ExecutionEnv>) -> AgentTool {
    AgentTool {
        name: "write".to_string(),
        label: "write".to_string(),
        description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".to_string(),
        parameters: json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": { "type": "string", "description": "Path to the file to write (relative or absolute)" },
                "content": { "type": "string", "description": "Content to write to the file" }
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
                    // Count characters (UTF-8 code points) rather than bytes,
                    // matching pi-mono's `content.length`.
                    format!("Successfully wrote {} characters to {path}", content.chars().count()),
                    Value::Null,
                ))
            })
        }),
        prepare_arguments: None,
        execution_mode: Some(ToolExecutionMode::Sequential),
    }
}

use flown_agent::{
    Agent, AgentEvent, AgentMessage, AgentOptions, AgentState, AgentTool, AgentToolResult,
};
use omp_ai::init;
use omp_ai::types::*;
use futures::stream::StreamExt;
use std::process::Command;
use std::sync::Arc;

fn create_bash_tool() -> AgentTool {
    AgentTool {
        name: "bash".to_string(),
        label: "Bash".to_string(),
        description: "Execute bash commands and return the output".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute"
                }
            },
            "required": ["command"]
        }),
        execute: Arc::new(|_tool_call_id, args, _signal, _on_update| {
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("echo 'No command provided'");

            let output = Command::new("bash").arg("-c").arg(command).output();

            let result = match output {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let exit_code = output.status.code().unwrap_or(-1);

                    let result = if exit_code == 0 {
                        format!("{}\n(exit code: 0)", stdout.trim())
                    } else {
                        format!(
                            "{}\n{}\n(exit code: {})",
                            stdout.trim(),
                            stderr.trim(),
                            exit_code
                        )
                    };

                    AgentToolResult {
                        content: vec![ToolResultContent::Text(TextContent {
                            content_type: "text".to_string(),
                            text: result,
                            text_signature: None,
                        })],
                        details: serde_json::json!({
                            "command": command,
                            "exit_code": exit_code,
                            "stdout": stdout,
                            "stderr": stderr
                        }),
                        terminate: None,
                    }
                }
                Err(e) => AgentToolResult {
                    content: vec![ToolResultContent::Text(TextContent {
                        content_type: "text".to_string(),
                        text: format!("Failed to execute command: {}", e),
                        text_signature: None,
                    })],
                    details: serde_json::json!({
                        "command": command,
                        "error": e.to_string()
                    }),
                    terminate: None,
                },
            };
            Box::pin(async move { Ok(result) })
        }),
        prepare_arguments: None,
        execution_mode: None,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init();

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentState {
            system_prompt: "You are a helpful assistant that can execute bash commands. Use the bash tool to run commands and help the user accomplish their tasks.".to_string(),
            model: omp_ai::get_model("deepseek", "deepseek-v4-flash")
                .expect("MiMo-V2.5  model not found"),
            thinking_level: ThinkingLevel::Minimal,
            messages: Vec::new(),
            is_streaming: false,
            streaming_message: None,
            pending_tool_calls: Vec::new(),
            error_message: None,
        }),
        get_api_key: Some(Arc::new(|_provider| {
            Box::pin(async { std::env::var("DEEPSEEK_API_KEY").ok() })
        })),
        ..Default::default()
    });

    agent.set_tools(vec![create_bash_tool()]);

    println!("Agent created!");
    println!("System prompt: {}", agent.state().system_prompt);
    println!("Model: {}", agent.state().model.name);
    println!();

    println!("Sending prompt to agent...");
    let mut stream = agent.prompt(
        "Write a Python script to calculate fibonacci(100) and run it. Show me the result."
            .to_string(),
    );

    let mut in_thinking = false;

    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::AgentStart => {
                println!("Agent started processing...");
            }
            AgentEvent::MessageStart { message } => match &message {
                AgentMessage::User(user) => {
                    if let MessageContent::Text(text) = &user.content {
                        println!("User: {}", text);
                    }
                }
                AgentMessage::Assistant(_) => {
                    print!("Assistant: ");
                }
                _ => {}
            },
            AgentEvent::MessageUpdate {
                assistant_message_event,
                ..
            } => match assistant_message_event {
                AssistantMessageEvent::TextDelta { delta, .. } => {
                    if in_thinking {
                        println!();
                        in_thinking = false;
                    }
                    print!("{}", delta);
                }
                AssistantMessageEvent::ThinkingStart { .. } => {
                    print!("[thinking] ");
                    in_thinking = true;
                }
                AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                    print!("{}", delta);
                }
                AssistantMessageEvent::ThinkingEnd { .. } => {
                    println!();
                    in_thinking = false;
                }
                _ => {}
            },
            AgentEvent::MessageEnd { message } => match &message {
                AgentMessage::Assistant(assistant) => {
                    if in_thinking {
                        println!();
                        in_thinking = false;
                    }
                    println!();
                    println!("--- Stop reason: {:?} ---", assistant.stop_reason);
                    if let Some(err) = &assistant.error_message {
                        println!("Error: {}", err);
                    }
                }
                AgentMessage::ToolResult(result) => {
                    println!("Tool result:");
                    for content in &result.content {
                        if let ToolResultContent::Text(text) = content {
                            println!("{}", text.text);
                        }
                    }
                    println!("---");
                }
                _ => {}
            },
            AgentEvent::ToolExecutionStart {
                tool_name, args, ..
            } => {
                let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                println!("[Executing tool: {}] $ {}", tool_name, cmd);
            }
            AgentEvent::ToolExecutionEnd { .. } => {}
            AgentEvent::AgentEnd { messages } => {
                println!();
                println!("=== Agent finished! Total messages: {} ===", messages.len());
            }
            _ => {}
        }
    }

    Ok(())
}

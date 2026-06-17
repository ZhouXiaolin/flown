use flown_agent::{Agent, AgentEvent, AgentMessage, AgentOptions, AgentTool, AgentToolResult};
use flown_ai::register_built_in_api_providers;
use flown_ai::types::*;
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
    register_built_in_api_providers();

    let mut options = AgentOptions::default();
    options.get_api_key = Some(Arc::new(|_provider| {
        Box::pin(async { std::env::var("DEEPSEEK_API_KEY").ok() })
    }));
    let agent = Agent::new(options);
    agent.set_tools(vec![create_bash_tool()]);
    agent.set_system_prompt(
        "You are a helpful coding agent. Use the bash tool to run commands."
            .to_string(),
    );

    println!("Agent ready. Model: {}", agent.state().model.name);
    println!();

    // Subscribe to lifecycle events (callback model). Listeners are awaited in
    // subscription order and are part of the run's settlement — wait_for_idle()
    // does not resolve until the agent_end listener returns.
    let _sub = agent.subscribe(Arc::new(|event, _signal| {
        Box::pin(async move {
            match event {
                AgentEvent::MessageStart { message } => {
                    if let AgentMessage::User(user) = &message {
                        if let MessageContent::Text(text) = &user.content {
                            println!("User: {}", text);
                        }
                    } else if matches!(message, AgentMessage::Assistant(_)) {
                        print!("Assistant: ");
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                }
                AgentEvent::MessageUpdate {
                    assistant_message_event,
                    ..
                } => match assistant_message_event {
                    AssistantMessageEvent::TextDelta { delta, .. } => {
                        print!("{delta}");
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                    _ => {}
                },
                AgentEvent::MessageEnd { message } => {
                    if let AgentMessage::Assistant(assistant) = &message {
                        println!();
                        println!("--- stop reason: {:?} ---", assistant.stop_reason);
                        if let Some(err) = &assistant.error_message {
                            println!("Error: {}", err);
                        }
                    }
                }
                AgentEvent::TurnEnd { .. } => println!(),
                AgentEvent::AgentEnd { .. } => println!("\n[agent done]"),
                _ => {}
            }
        })
    }));

    let prompt = std::env::args().nth(1).unwrap_or_else(|| {
        "List the files in the current directory using bash.".to_string()
    });
    agent
        .prompt(flown_agent::PromptInput::Text(prompt))
        .await
        .expect("prompt failed");
    agent.wait_for_idle().await;

    Ok(())
}

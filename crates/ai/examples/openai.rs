use flown_ai::*;
use flown_ai::{get_model, register_built_in_api_providers, stream_simple};
use futures::stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize the AI module
    register_built_in_api_providers();

    // Get the DeepSeek V4 Flash model
    let model =
        get_model("deepseek", "deepseek-v4-flash").expect("DeepSeek V4 Flash model not found");

    println!("Model: {} ({})", model.name, model.id);
    println!("Provider: {:?}", model.provider);
    println!("API: {:?}", model.api);
    println!("Context Window: {}", model.context_window);
    println!("Max Tokens: {}", model.max_tokens);
    println!();

    // Create a simple context
    let context = Context {
        system_prompt: Some("You are a helpful assistant.".to_string()),
        messages: vec![Message::User(UserMessage {
            role: "user".to_string(),
            content: MessageContent::Text("Hello! Can you tell me a short joke?".to_string()),
            timestamp: chrono::Utc::now(),
        })],
        tools: None,
    };

    // Stream the response
    println!("Sending request to DeepSeek...");
    println!();

    let options = SimpleStreamOptions {
        base: StreamOptions {
            api_key: std::env::var("DEEPSEEK_API_KEY").ok(),
            ..Default::default()
        },
        reasoning: None,
        thinking_budgets: None,
    };
    let mut stream = stream_simple(&model, &context, Some(&options))?;

    let mut in_thinking = false;

    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::Start { .. } => {
                // Response started
            }
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
            AssistantMessageEvent::Done { message, .. } => {
                if in_thinking {
                    println!();
                }
                println!();
                println!("Done! Stop reason: {:?}", message.stop_reason);
                println!(
                    "Usage: {} input, {} output tokens",
                    message.usage.input, message.usage.output
                );
            }
            AssistantMessageEvent::Error { error, .. } => {
                eprintln!("Error: {:?}", error.error_message);
            }
            _ => {}
        }
    }

    Ok(())
}

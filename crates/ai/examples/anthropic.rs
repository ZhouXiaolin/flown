use flown_ai::*;
use flown_ai::{get_model, register_built_in_api_providers, stream_simple};
use futures::stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    register_built_in_api_providers();

    let api_key = std::env::var("MIMO_API_KEY")?;
    let model =
        get_model("xiaomi-token-plan-cn", "mimo-v2.5").expect("mimo-v2.5-pro model not found");

    println!("Model: {} ({})", model.name, model.id);
    println!("Provider: {:?}", model.provider);
    println!("API: {:?}", model.api);
    println!("Base URL: {}", model.base_url);
    println!();

    let context = Context {
        system_prompt: Some("You are a concise assistant.".to_string()),
        messages: vec![Message::User(UserMessage {
            role: "user".to_string(),
            content: MessageContent::Text("用一句话介绍一下 Rust 的所有权。".to_string()),
            timestamp: chrono::Utc::now(),
        })],
        tools: None,
    };

    let options = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some(api_key),
            max_tokens: Some(1024),
            ..Default::default()
        },
        reasoning: Some(ThinkingLevel::Off),
        thinking_budgets: None,
    };

    let mut stream = stream_simple(&model, &context, Some(&options))?;

    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::TextDelta { delta, .. } => {
                print!("{}", delta);
            }
            AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                print!("{}", delta);
            }
            AssistantMessageEvent::Done { message, .. } => {
                println!();
                println!("Done. Stop reason: {:?}", message.stop_reason);
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

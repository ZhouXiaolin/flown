use flown_agent::{Agent, AgentEvent, AgentMessage, AgentOptions, PromptInput};
use flown_ai::register_built_in_api_providers;
use flown_ai::types::{MessageContent, UserMessage};
use std::sync::Arc;

/// Build a user text message (UserMessage has no `text()` constructor).
fn user_text(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage {
        role: "user".to_string(),
        content: MessageContent::Text(text.to_string()),
        timestamp: chrono::Utc::now(),
    })
}

#[tokio::test]
async fn subscribe_receives_events_in_order() {
    register_built_in_api_providers();
    let agent = Agent::new(AgentOptions::default());
    agent.set_system_prompt("You are a test agent.".to_string());

    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let order_clone = order.clone();
    let _sub = agent.subscribe(Arc::new(move |event, _signal| {
        let order_clone = order_clone.clone();
        Box::pin(async move {
            order_clone.lock().unwrap().push(match event {
                AgentEvent::AgentStart => "agent_start".to_string(),
                AgentEvent::TurnStart => "turn_start".to_string(),
                AgentEvent::AgentEnd { .. } => "agent_end".to_string(),
                _ => "other".to_string(),
            });
        })
    }));

    // No tools, no real provider registered for the default model → the run
    // fails, which must surface as an error assistant message event sequence,
    // not a panic. agent_end must still fire.
    let _ = agent.prompt(PromptInput::Messages(vec![user_text("hi")])).await;
    agent.wait_for_idle().await;

    let observed = order.lock().unwrap().clone();
    assert!(observed.first().map(|s| s == "agent_start").unwrap_or(false));
    assert!(observed.last().map(|s| s == "agent_end").unwrap_or(false));
}

#[tokio::test]
async fn prompt_while_busy_returns_already_processing() {
    register_built_in_api_providers();
    let mut options = AgentOptions::default();
    // A stream_fn that never resolves keeps the agent busy so the second
    // prompt observes the occupied run slot.
    options.stream_fn = Some(Arc::new(|_model, _ctx, _opts| {
        // A stream that never yields — keeps the agent busy so the second
        // prompt observes the occupied run slot.
        let stream: flown_ai::api_registry::RawEventStream =
            Box::pin(futures::stream::pending::<flown_ai::AssistantMessageEvent>());
        flown_ai::AssistantMessageEventStream::from_stream(stream)
    }));
    let agent = Agent::new(options);
    agent.set_system_prompt("x".to_string());

    // Clone shares state (all Agent fields are Arc-backed).
    let agent_busy = agent.clone();
    let busy = tokio::spawn(async move {
        let _ = agent_busy.prompt(PromptInput::Text("hi".into())).await;
    });
    // Yield so the first prompt grabs the run slot.
    tokio::task::yield_now().await;

    let second = agent.prompt(PromptInput::Text("again".into())).await;
    assert!(matches!(second, Err(e) if e.to_string().contains("already processing")));

    busy.abort();
    agent.abort();
    agent.wait_for_idle().await;
}

#[tokio::test]
async fn continue_with_no_messages_returns_no_messages() {
    register_built_in_api_providers();
    let agent = Agent::new(AgentOptions::default());
    let result = agent.continue_run().await;
    assert!(matches!(result, Err(e) if e.to_string().contains("No messages")));
}

#[tokio::test]
async fn steer_and_follow_up_queue_messages() {
    register_built_in_api_providers();
    let agent = Agent::new(AgentOptions::default());
    agent.steer(user_text("steer"));
    agent.follow_up(user_text("followup"));
    assert!(agent.has_queued_messages());
    agent.clear_all_queues();
    assert!(!agent.has_queued_messages());
}

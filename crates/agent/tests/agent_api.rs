use flown_agent::{Agent, AgentEvent, AgentMessage, AgentOptions, PromptInput};
use flown_ai::register_built_in_api_providers;
use flown_ai::{MessageContent, UserMessage};
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
    agent.state().update(|state| {
        state.system_prompt = "You are a test agent.".to_string();
    });

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
    let _ = agent
        .prompt(PromptInput::Messages(vec![user_text("hi")]))
        .await;
    agent.wait_for_idle().await;

    let observed = order.lock().unwrap().clone();
    assert!(
        observed
            .first()
            .map(|s| s == "agent_start")
            .unwrap_or(false)
    );
    assert!(observed.last().map(|s| s == "agent_end").unwrap_or(false));
}

#[tokio::test]
async fn prompt_while_busy_returns_already_processing() {
    register_built_in_api_providers();
    let mut options = AgentOptions::default();
    // A stream_fn that stays active until the run is aborted keeps the agent
    // busy long enough for the second prompt to observe the occupied run slot.
    options.stream_fn = Some(Arc::new(|model, _ctx, opts| {
        let signal = opts.and_then(|opts| opts.base.signal.clone());
        let stream: flown_ai::RawEventStream = Box::pin(async_stream::stream! {
            let partial = flown_ai::AssistantMessage {
                role: "assistant".to_string(),
                content: vec![],
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                response_model: None,
                response_id: None,
                usage: flown_ai::Usage::default(),
                stop_reason: flown_ai::StopReason::Stop,
                error_message: None,
                diagnostics: None,
                timestamp: chrono::Utc::now(),
            };
            yield flown_ai::AssistantMessageEvent::Start {
                partial: partial.clone(),
            };

            if let Some(signal) = signal {
                signal.cancelled().await;
                let mut aborted = partial;
                aborted.stop_reason = flown_ai::StopReason::Aborted;
                aborted.error_message = Some("aborted".to_string());
                yield flown_ai::AssistantMessageEvent::Error {
                    reason: flown_ai::StopReason::Aborted,
                    error: aborted,
                };
            } else {
                futures::future::pending::<()>().await;
            }
        });
        flown_ai::AssistantMessageEventStream::from_stream(stream)
    }));
    let agent = Agent::new(options);
    agent.state().update(|state| {
        state.system_prompt = "x".to_string();
    });

    // Clone shares state (all Agent fields are Arc-backed).
    let agent_busy = agent.clone();
    let _busy = tokio::spawn(async move {
        let _ = agent_busy.prompt(PromptInput::Text("hi".into())).await;
    });
    // Yield so the first prompt grabs the run slot.
    tokio::task::yield_now().await;

    let second = agent.prompt(PromptInput::Text("again".into())).await;
    assert!(matches!(second, Err(e) if e.to_string().contains("already processing")));

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

#[tokio::test]
async fn abort_does_not_clear_queued_messages() {
    register_built_in_api_providers();
    let agent = Agent::new(AgentOptions::default());
    agent.steer(user_text("steer"));
    agent.follow_up(user_text("followup"));
    agent.abort();
    assert!(agent.has_queued_messages());
}

#[tokio::test]
async fn state_handle_updates_and_reads() {
    let agent = Agent::new(AgentOptions::default());
    agent.state().update(|state| {
        state.system_prompt = "updated".to_string();
    });
    assert_eq!(agent.state().snapshot().system_prompt, "updated");
}

#[tokio::test]
async fn unsubscribe_removes_only_the_target_listener() {
    register_built_in_api_providers();
    let agent = Agent::new(AgentOptions::default());
    agent.state().update(|state| {
        state.system_prompt = "listener test".to_string();
    });
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));

    let events_first = events.clone();
    let first = agent.subscribe(Arc::new(move |_event, _signal| {
        let events_first = events_first.clone();
        Box::pin(async move {
            events_first.lock().unwrap().push("first".to_string());
        })
    }));

    let events_second = events.clone();
    let second = agent.subscribe(Arc::new(move |_event, _signal| {
        let events_second = events_second.clone();
        Box::pin(async move {
            events_second.lock().unwrap().push("second".to_string());
        })
    }));

    let events_third = events.clone();
    let _third = agent.subscribe(Arc::new(move |_event, _signal| {
        let events_third = events_third.clone();
        Box::pin(async move {
            events_third.lock().unwrap().push("third".to_string());
        })
    }));

    first.unsubscribe();
    drop(second);

    let transient = agent.subscribe(Arc::new(|_event, _signal| Box::pin(async move {})));
    drop(transient);

    let _ = agent
        .prompt(PromptInput::Messages(vec![user_text("hi")]))
        .await;
    agent.wait_for_idle().await;

    let observed = events.lock().unwrap().clone();
    assert!(!observed.is_empty());
    assert!(observed.iter().all(|entry| entry == "third"));
}

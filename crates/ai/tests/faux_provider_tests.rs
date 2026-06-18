use flown_ai::{
    Api, AssistantContent, AssistantMessageEvent, Context, FauxContentBlock, FauxResponseStep,
    Provider, StopReason, complete_simple, faux_assistant_message, faux_text,
    register_faux_provider, stream_simple,
};

#[test]
fn register_faux_provider_exposes_model_and_queue_controls() {
    let registration = register_faux_provider(Default::default());

    assert_eq!(registration.models().len(), 1);
    assert!(registration.get_model(None).is_some());
    assert_eq!(registration.call_count(), 0);
    assert_eq!(registration.pending_response_count(), 0);
}

#[tokio::test]
async fn faux_provider_streams_queued_responses_and_complete_simple_resolves() {
    let registration = register_faux_provider(Default::default());
    registration.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        "hello",
        Default::default(),
    ))]);

    let model = registration.get_model(None).unwrap();
    let context = Context {
        system_prompt: Some("system".to_string()),
        messages: vec![],
        tools: None,
    };

    let mut stream = stream_simple(&model, &context, None).unwrap();
    let mut events = Vec::new();
    while let Some(event) = futures::StreamExt::next(&mut stream).await {
        events.push(event);
    }

    assert!(matches!(events.last(), Some(AssistantMessageEvent::Done { .. })));
    assert_eq!(registration.call_count(), 1);

    registration.append_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        "world",
        Default::default(),
    ))]);
    let completed = complete_simple(&model, &context, None).await.unwrap();
    assert_eq!(
        completed
            .content
            .iter()
            .find_map(|block| match block {
                AssistantContent::Text(text) => Some(text.text.clone()),
                _ => None,
            }),
        Some("world".to_string())
    );
}

#[test]
fn faux_helpers_build_expected_messages() {
    let message = faux_assistant_message(
        vec![FauxContentBlock::Text(faux_text("hello"))],
        Default::default(),
    );

    assert_eq!(message.role, "assistant");
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.api, Api::Custom("faux".to_string()));
    assert_eq!(message.provider, Provider::Custom("faux".to_string()));
}

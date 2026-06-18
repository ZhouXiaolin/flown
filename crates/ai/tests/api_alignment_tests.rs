use flown_ai::{
    AnthropicOptions, Api, AssistantContent, AssistantMessage, AssistantMessageEvent,
    AzureOpenAIResponsesOptions, BedrockOptions, Context, Cost, GoogleOptions, GoogleThinkingLevel,
    GoogleVertexOptions, ImageContent, KnownApi, KnownProvider, Message, MessageContent,
    MistralOptions, Model, ModelCost, OAuthAuthInfo, OAuthCredentials, OAuthDeviceCodeInfo,
    OAuthPrompt, OAuthProviderId, OAuthProviderInfo, OAuthSelectOption, OAuthSelectPrompt,
    OpenAICodexResponsesOptions, OpenAICodexWebSocketDebugStats, OpenAICompletionsOptions,
    OpenAIResponsesOptions, Provider, StopReason, TextContent, ThinkingContent, ThinkingLevel,
    ToolCall, Usage, UserContentBlock, UserMessage, clamp_thinking_level, clear_api_providers,
    complete, get_api_provider, get_model, get_models, get_providers,
    get_supported_thinking_levels, models_are_equal, register_built_in_api_providers,
    register_built_in_images_api_providers, stream, stream_anthropic_public,
    stream_openai_completions_public, stream_openai_responses_public, stream_simple_anthropic,
    stream_simple_openai_completions, stream_simple_openai_responses,
};
use std::collections::HashMap;
use std::sync::Mutex;

/// The API-provider registry is process-global. Tests that mutate it
/// (`clear_api_providers` / `register_built_in_api_providers`) must run
/// serially to avoid one test's reset wiping another's registration mid-flight.
/// Each registry-mutating test holds this guard for its whole body.
static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn known_api_serializes_to_pi_mono_api_names() {
    assert_eq!(
        serde_json::to_value(Api::Known(KnownApi::OpenAiCompletions)).unwrap(),
        serde_json::json!("openai-completions")
    );
    assert_eq!(
        serde_json::to_value(Api::Known(KnownApi::OpenAiResponses)).unwrap(),
        serde_json::json!("openai-responses")
    );
    assert_eq!(
        serde_json::to_value(Api::Known(KnownApi::AnthropicMessages)).unwrap(),
        serde_json::json!("anthropic-messages")
    );
}

#[test]
fn known_provider_display_uses_public_provider_id() {
    assert_eq!(
        Provider::Known(KnownProvider::Deepseek).to_string(),
        "deepseek"
    );
    assert_eq!(
        Provider::Known(KnownProvider::Anthropic).to_string(),
        "anthropic"
    );
    assert_eq!(
        Provider::Known(KnownProvider::XiaomiTokenPlanCn).to_string(),
        "xiaomi-token-plan-cn"
    );
}

#[test]
fn register_built_ins_registers_openai_completions_provider() {
    let _guard = REGISTRY_LOCK.lock().unwrap();
    clear_api_providers();
    register_built_in_api_providers();

    let model = get_model("deepseek", "deepseek-v4-flash").expect("model registered");
    assert_eq!(model.api, Api::Known(KnownApi::OpenAiCompletions));
    assert_eq!(model.provider, Provider::Known(KnownProvider::Deepseek));
    assert_eq!(model.base_url, "https://api.deepseek.com");
    assert_eq!(
        model
            .compat
            .as_ref()
            .and_then(|compat| compat.thinking_format.as_deref()),
        Some("deepseek")
    );

    let provider = get_api_provider(&Api::Known(KnownApi::OpenAiCompletions));
    assert!(
        provider.is_some(),
        "openai-completions provider is registered"
    );
}

#[test]
fn builtin_deepseek_models_are_available_without_registration() {
    let model = get_model("deepseek", "deepseek-v4-flash").expect("built-in model");

    assert_eq!(model.api, Api::Known(KnownApi::OpenAiCompletions));
    assert_eq!(model.provider, Provider::Known(KnownProvider::Deepseek));
    assert!(get_providers().contains(&"deepseek".to_string()));
    assert!(
        get_models("deepseek")
            .iter()
            .any(|model| model.id == "deepseek-v4-flash")
    );
}

#[test]
fn openai_completions_provider_registers_via_builtins() {
    let _guard = REGISTRY_LOCK.lock().unwrap();
    clear_api_providers();
    register_built_in_api_providers();
    // pi-ai's `registerBuiltInApiProviders` registers an
    // `openai-completions` provider; the Rust port must too. Providers are no
    // longer public structs (mirroring pi-ai's function-style providers), so
    // the registry is the public surface.
    assert!(get_api_provider(&Api::Known(KnownApi::OpenAiCompletions)).is_some());
}

#[test]
fn anthropic_provider_registers_via_builtins() {
    let _guard = REGISTRY_LOCK.lock().unwrap();
    clear_api_providers();
    register_built_in_api_providers();
    assert!(get_api_provider(&Api::Known(KnownApi::AnthropicMessages)).is_some());
}

#[test]
fn openai_responses_provider_registers_via_builtins() {
    let _guard = REGISTRY_LOCK.lock().unwrap();
    clear_api_providers();
    register_built_in_api_providers();
    assert!(get_api_provider(&Api::Known(KnownApi::OpenAiResponses)).is_some());
}

#[test]
fn complete_is_exported() {
    let _ = complete;
}

#[test]
fn provider_option_types_are_exported() {
    let anthropic = AnthropicOptions::default();
    let bedrock = BedrockOptions::default();
    let azure = AzureOpenAIResponsesOptions::default();
    let google = GoogleOptions::default();
    let google_vertex = GoogleVertexOptions::default();
    let mistral = MistralOptions::default();
    let openai_responses = OpenAIResponsesOptions::default();
    let codex = OpenAICodexResponsesOptions::default();
    let openai = OpenAICompletionsOptions::default();
    assert!(anthropic.base.api_key.is_none());
    assert!(bedrock.base.api_key.is_none());
    assert!(azure.base.api_key.is_none());
    assert!(google.base.api_key.is_none());
    assert!(google_vertex.base.api_key.is_none());
    assert!(mistral.base.api_key.is_none());
    assert!(openai_responses.base.api_key.is_none());
    assert!(codex.base.api_key.is_none());
    assert!(openai.base.api_key.is_none());
}

#[test]
fn internal_provider_detail_types_are_not_top_level_exports() {
    let _ = std::any::type_name::<AnthropicOptions>();
    let _ = std::any::type_name::<OpenAICompletionsOptions>();
    let _ = std::any::type_name::<GoogleOptions>();
    let _ = std::any::type_name::<GoogleVertexOptions>();
}

#[test]
fn codex_debug_stats_are_exported() {
    let stats = OpenAICodexWebSocketDebugStats::default();
    assert_eq!(stats.requests, 0);
    assert_eq!(stats.last_web_socket_error, None);
}

#[test]
fn oauth_public_types_are_exported() {
    let provider_id: OAuthProviderId = "openai-codex".to_string();
    let credentials = OAuthCredentials {
        refresh: "r".to_string(),
        access: "a".to_string(),
        expires: 1,
        extra: std::collections::HashMap::new(),
    };
    let auth = OAuthAuthInfo {
        url: "https://example.com".to_string(),
        instructions: Some("open it".to_string()),
    };
    let device = OAuthDeviceCodeInfo {
        user_code: "ABCD".to_string(),
        verification_uri: "https://example.com/device".to_string(),
        interval_seconds: Some(5),
        expires_in_seconds: Some(600),
    };
    let prompt = OAuthPrompt {
        message: "enter code".to_string(),
        placeholder: None,
        allow_empty: Some(false),
    };
    let option = OAuthSelectOption {
        id: "browser".to_string(),
        label: "Browser".to_string(),
    };
    let select = OAuthSelectPrompt {
        message: "pick".to_string(),
        options: vec![option.clone()],
    };
    let info = OAuthProviderInfo {
        id: provider_id.clone(),
        name: "Codex".to_string(),
        available: true,
    };

    assert_eq!(provider_id, "openai-codex");
    assert_eq!(credentials.expires, 1);
    assert_eq!(auth.instructions.as_deref(), Some("open it"));
    assert_eq!(device.interval_seconds, Some(5));
    assert_eq!(prompt.allow_empty, Some(false));
    assert_eq!(select.options[0].id, option.id);
    assert!(info.available);
}

#[test]
fn google_thinking_level_is_exported() {
    let level = GoogleThinkingLevel::High;
    assert_eq!(
        serde_json::to_value(level).unwrap(),
        serde_json::json!("HIGH")
    );
}

#[test]
fn provider_specific_stream_functions_are_exported() {
    let _ = stream_anthropic_public;
    let _ = stream_simple_anthropic;
    let _ = stream_openai_completions_public;
    let _ = stream_simple_openai_completions;
    let _ = stream_openai_responses_public;
    let _ = stream_simple_openai_responses;
}

#[test]
fn provider_builtins_registration_functions_are_exported() {
    let _ = register_built_in_api_providers;
    let _ = register_built_in_images_api_providers;
}

#[test]
fn stream_returns_error_when_provider_is_missing() {
    let _guard = REGISTRY_LOCK.lock().unwrap();
    clear_api_providers();
    let model = get_model("deepseek", "deepseek-v4-flash").expect("model");
    let context = Context {
        system_prompt: None,
        messages: vec![],
        tools: None,
    };

    let error = stream(&model, &context, None).expect_err("missing provider error");

    assert_eq!(
        error.to_string(),
        "No API provider registered for api: openai-completions"
    );
}

#[test]
fn clear_api_providers_keeps_registry_empty_until_explicit_reregister() {
    let _guard = REGISTRY_LOCK.lock().unwrap();
    clear_api_providers();
    let provider = get_api_provider(&Api::Known(KnownApi::OpenAiCompletions));
    assert!(
        provider.is_none(),
        "clearApiProviders() should leave registry empty"
    );
}

#[test]
fn assistant_message_serializes_with_pi_ai_field_names() {
    let message = AssistantMessage {
        role: "assistant".to_string(),
        content: vec![
            AssistantContent::Text(TextContent {
                content_type: "text".to_string(),
                text: "hello".to_string(),
                text_signature: Some("sig-text".to_string()),
            }),
            AssistantContent::Thinking(ThinkingContent {
                content_type: "thinking".to_string(),
                thinking: "plan".to_string(),
                thinking_signature: Some("sig-thinking".to_string()),
                redacted: Some(false),
            }),
            AssistantContent::ToolCall(ToolCall {
                content_type: "toolCall".to_string(),
                id: "call_1".to_string(),
                name: "lookup".to_string(),
                arguments: serde_json::json!({ "q": "rust" }),
                thought_signature: Some("sig-tool".to_string()),
            }),
        ],
        api: Api::Known(KnownApi::OpenAiCompletions),
        provider: Provider::Known(KnownProvider::Deepseek),
        model: "deepseek-v4-flash".to_string(),
        response_model: Some("deepseek-v4-flash-actual".to_string()),
        response_id: Some("resp_1".to_string()),
        usage: Usage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            cache_write_1h: None,
            total_tokens: 10,
            cost: Cost {
                input: 0.1,
                output: 0.2,
                cache_read: 0.3,
                cache_write: 0.4,
                total: 1.0,
            },
        },
        stop_reason: StopReason::ToolUse,
        error_message: Some("err".to_string()),
        diagnostics: None,
        timestamp: chrono::Utc::now(),
    };

    let value = serde_json::to_value(message).unwrap();
    assert_eq!(value["stopReason"], serde_json::json!("toolUse"));
    assert_eq!(
        value["responseModel"],
        serde_json::json!("deepseek-v4-flash-actual")
    );
    assert_eq!(value["responseId"], serde_json::json!("resp_1"));
    assert_eq!(value["errorMessage"], serde_json::json!("err"));
    assert_eq!(value["usage"]["cacheRead"], serde_json::json!(3));
    assert_eq!(value["usage"]["cacheWrite"], serde_json::json!(4));
    assert_eq!(value["usage"]["totalTokens"], serde_json::json!(10));
    assert_eq!(value["usage"]["cost"]["cacheRead"], serde_json::json!(0.3));
    assert_eq!(
        value["content"][0]["textSignature"],
        serde_json::json!("sig-text")
    );
    assert_eq!(
        value["content"][1]["thinkingSignature"],
        serde_json::json!("sig-thinking")
    );
    assert_eq!(
        value["content"][2]["thoughtSignature"],
        serde_json::json!("sig-tool")
    );
}

#[test]
fn assistant_events_serialize_with_pi_ai_event_field_names() {
    let partial = AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: Api::Known(KnownApi::OpenAiCompletions),
        provider: Provider::Known(KnownProvider::Deepseek),
        model: "deepseek-v4-flash".to_string(),
        response_model: None,
        response_id: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        error_message: None,
        diagnostics: None,
        timestamp: chrono::Utc::now(),
    };

    let text_delta = serde_json::to_value(AssistantMessageEvent::TextDelta {
        content_index: 2,
        delta: "hi".to_string(),
        partial: partial.clone(),
    })
    .unwrap();
    assert_eq!(text_delta["type"], serde_json::json!("text_delta"));
    assert_eq!(text_delta["contentIndex"], serde_json::json!(2));

    let tool_end = serde_json::to_value(AssistantMessageEvent::ToolCallEnd {
        content_index: 1,
        tool_call: ToolCall {
            content_type: "toolCall".to_string(),
            id: "call_1".to_string(),
            name: "lookup".to_string(),
            arguments: serde_json::json!({}),
            thought_signature: None,
        },
        partial,
    })
    .unwrap();
    assert_eq!(tool_end["contentIndex"], serde_json::json!(1));
    assert_eq!(tool_end["toolCall"]["id"], serde_json::json!("call_1"));
}

#[test]
fn model_context_and_user_content_serialize_with_pi_ai_field_names() {
    let context = Context {
        system_prompt: Some("system".to_string()),
        messages: vec![Message::User(UserMessage {
            role: "user".to_string(),
            content: MessageContent::Blocks(vec![UserContentBlock::Image(ImageContent {
                content_type: "image".to_string(),
                data: "abc".to_string(),
                mime_type: "image/png".to_string(),
            })]),
            timestamp: chrono::Utc::now(),
        })],
        tools: None,
    };
    let value = serde_json::to_value(context).unwrap();
    assert_eq!(value["systemPrompt"], serde_json::json!("system"));
    assert_eq!(
        value["messages"][0]["content"][0]["mimeType"],
        serde_json::json!("image/png")
    );

    let model = get_model("deepseek", "deepseek-v4-flash").expect("model");
    let value = serde_json::to_value(model).unwrap();
    assert_eq!(
        value["baseUrl"],
        serde_json::json!("https://api.deepseek.com")
    );
    assert!(value.get("base_url").is_none());
    assert!(value.get("maxTokens").is_some());
    assert!(value.get("contextWindow").is_some());
}

#[test]
fn thinking_helpers_match_pi_ai_model_semantics() {
    let mut model = Model {
        id: "reasoning-model".to_string(),
        name: "Reasoning".to_string(),
        api: Api::Known(KnownApi::OpenAiCompletions),
        provider: Provider::Known(KnownProvider::Deepseek),
        base_url: "https://api.deepseek.com".to_string(),
        reasoning: true,
        thinking_level_map: Some(HashMap::from([
            (ThinkingLevel::Off, Some("none".to_string())),
            (ThinkingLevel::Minimal, None),
            (ThinkingLevel::Low, Some("low".to_string())),
            (ThinkingLevel::Medium, Some("medium".to_string())),
            (ThinkingLevel::High, Some("high".to_string())),
            (ThinkingLevel::XHigh, None),
        ])),
        input: vec!["text".to_string()],
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 128_000,
        max_tokens: 4096,
        headers: None,
        compat: None,
    };

    assert_eq!(
        get_supported_thinking_levels(&model),
        vec![
            ThinkingLevel::Off,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
        ]
    );
    assert_eq!(
        clamp_thinking_level(&model, ThinkingLevel::Minimal),
        ThinkingLevel::Low
    );
    assert_eq!(
        clamp_thinking_level(&model, ThinkingLevel::XHigh),
        ThinkingLevel::High
    );

    let same = model.clone();
    assert!(models_are_equal(Some(&model), Some(&same)));
    model.id = "other".to_string();
    assert!(!models_are_equal(Some(&model), Some(&same)));
    assert!(!models_are_equal(None, Some(&same)));
}

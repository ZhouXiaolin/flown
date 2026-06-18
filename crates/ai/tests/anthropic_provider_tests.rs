use flown_ai::{
    AbortSignal, Api, ApiProvider, AssistantContent, AssistantMessage, AssistantMessageEvent,
    Context, ImageContent, KnownApi, KnownProvider, Message, MessageContent, Model, ModelCost,
    Provider, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingLevel, Tool,
    UserContentBlock, UserMessage, clear_api_providers, get_api_provider,
    register_built_in_api_providers,
};
use futures::StreamExt;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

static ENV_LOCK: Mutex<()> = Mutex::new(());
static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

fn restore_env(key: &str, previous: Option<String>) {
    unsafe {
        if let Some(previous) = previous {
            std::env::set_var(key, previous);
        } else {
            std::env::remove_var(key);
        }
    }
}

fn has_header_line(headers: &str, expected: &str) -> bool {
    headers.lines().any(|line| line == expected)
}

fn anthropic_model(base_url: String) -> Model {
    Model {
        id: "mimo-v2-flash".to_string(),
        name: "MiMo-V2-Flash".to_string(),
        api: Api::Known(KnownApi::AnthropicMessages),
        provider: Provider::Known(KnownProvider::XiaomiTokenPlanCn),
        base_url,
        reasoning: true,
        thinking_level_map: None,
        input: vec!["text".to_string()],
        cost: ModelCost {
            input: 0.1,
            output: 0.3,
            cache_read: 0.01,
            cache_write: 0.0,
        },
        context_window: 262_144,
        max_tokens: 65_536,
        headers: None,
        compat: None,
    }
}

/// Resolve the built-in Anthropic provider from the registry. Providers are no
/// longer public structs (mirroring pi-ai's function-style providers), so
/// tests acquire the `dyn ApiProvider` the same way embedders do.
fn anthropic_provider() -> Arc<dyn ApiProvider> {
    let _guard = REGISTRY_LOCK.lock().unwrap();
    clear_api_providers();
    register_built_in_api_providers();
    get_api_provider(&Api::Known(KnownApi::AnthropicMessages))
        .expect("anthropic-messages provider registered by builtins")
}

fn test_context() -> Context {
    Context {
        system_prompt: Some("be concise".to_string()),
        messages: vec![Message::User(UserMessage {
            role: "user".to_string(),
            content: MessageContent::Text("hello".to_string()),
            timestamp: chrono::Utc::now(),
        })],
        tools: Some(vec![Tool {
            name: "lookup".to_string(),
            description: "Lookup facts".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "q": { "type": "string" }
                },
                "required": ["q"]
            }),
        }]),
    }
}

fn anthropic_event(event: &str, data: serde_json::Value) -> String {
    format!("event: {event}\ndata: {}\n\n", data)
}

#[tokio::test]
async fn anthropic_stream_posts_pi_ai_message_payload_to_v1_messages() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured_request = Arc::new(Mutex::new(String::new()));
    let captured_for_server = captured_request.clone();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0_u8; 8192];
        let n = socket.read(&mut buf).await.unwrap();
        *captured_for_server.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();

        let body = [
            anthropic_event(
                "message_start",
                serde_json::json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_1",
                        "usage": { "input_tokens": 3, "output_tokens": 0 }
                    }
                }),
            ),
            anthropic_event(
                "content_block_start",
                serde_json::json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": { "type": "text", "text": "" }
                }),
            ),
            anthropic_event(
                "content_block_delta",
                serde_json::json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": { "type": "text_delta", "text": "hi" }
                }),
            ),
            anthropic_event(
                "content_block_stop",
                serde_json::json!({ "type": "content_block_stop", "index": 0 }),
            ),
            anthropic_event(
                "message_delta",
                serde_json::json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": "end_turn" },
                    "usage": { "output_tokens": 1 }
                }),
            ),
            anthropic_event(
                "message_stop",
                serde_json::json!({ "type": "message_stop" }),
            ),
        ]
        .join("");
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let provider = anthropic_provider();
    let model = anthropic_model(format!("http://{}", addr));
    let mut stream = provider.stream(
        &model,
        &test_context(),
        Some(&StreamOptions {
            api_key: Some("test-key".to_string()),
            max_tokens: Some(128),
            temperature: Some(0.4),
            ..Default::default()
        }),
    );

    let mut final_message = None;
    while let Some(event) = stream.next().await {
        if let AssistantMessageEvent::Done { message, .. } = event {
            final_message = Some(message);
        }
    }
    server.await.unwrap();

    let request = captured_request.lock().unwrap().clone();
    assert!(request.starts_with("POST /v1/messages HTTP/1.1"));
    assert!(request.to_ascii_lowercase().contains("x-api-key: test-key"));
    assert!(request.contains("anthropic-version: 2023-06-01"));

    let (_, body) = request.split_once("\r\n\r\n").expect("request body");
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["model"], serde_json::json!("mimo-v2-flash"));
    assert_eq!(body["stream"], serde_json::json!(true));
    assert_eq!(body["max_tokens"], serde_json::json!(128));
    assert_eq!(body["temperature"], serde_json::json!(0.4));
    assert_eq!(body["system"][0]["text"], serde_json::json!("be concise"));
    assert_eq!(
        body["system"][0]["cache_control"],
        serde_json::json!({ "type": "ephemeral" })
    );
    assert_eq!(body["messages"][0]["role"], serde_json::json!("user"));
    assert_eq!(
        body["messages"][0]["content"],
        serde_json::json!([{
            "type": "text",
            "text": "hello",
            "cache_control": { "type": "ephemeral" }
        }])
    );
    assert_eq!(body["tools"][0]["name"], serde_json::json!("lookup"));
    assert_eq!(
        body["tools"][0]["eager_input_streaming"],
        serde_json::json!(true)
    );
    assert_eq!(
        body["tools"][0]["cache_control"],
        serde_json::json!({ "type": "ephemeral" })
    );
    assert_eq!(
        body["tools"][0]["input_schema"],
        serde_json::json!({
            "type": "object",
            "properties": { "q": { "type": "string" } },
            "required": ["q"]
        })
    );

    let message = final_message.expect("done message");
    assert_eq!(message.response_id.as_deref(), Some("msg_1"));
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.usage.input, 3);
    assert_eq!(message.usage.output, 1);
    assert!(matches!(
        message.content.first(),
        Some(AssistantContent::Text(TextContent { text, .. })) if text == "hi"
    ));
}

#[tokio::test]
async fn anthropic_stream_repairs_tool_json_arguments() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0_u8; 4096];
        let _ = socket.read(&mut buf).await.unwrap();

        let body = [
            anthropic_event(
                "content_block_start",
                serde_json::json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": "write_file",
                        "input": {}
                    }
                }),
            ),
            anthropic_event(
                "content_block_delta",
                serde_json::json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": "{\"path\":\"C:\\q\"}"
                    }
                }),
            ),
            anthropic_event(
                "content_block_stop",
                serde_json::json!({ "type": "content_block_stop", "index": 0 }),
            ),
            anthropic_event(
                "message_delta",
                serde_json::json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": "tool_use" },
                    "usage": { "output_tokens": 4 }
                }),
            ),
            anthropic_event(
                "message_stop",
                serde_json::json!({ "type": "message_stop" }),
            ),
        ]
        .join("");
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let provider = anthropic_provider();
    let model = anthropic_model(format!("http://{}", addr));
    let mut stream = provider.stream(
        &model,
        &test_context(),
        Some(&StreamOptions {
            api_key: Some("test-key".to_string()),
            ..Default::default()
        }),
    );

    let mut final_message: Option<AssistantMessage> = None;
    while let Some(event) = stream.next().await {
        if let AssistantMessageEvent::Done { message, .. } = event {
            final_message = Some(message);
        }
    }
    server.await.unwrap();

    let message = final_message.expect("done message");
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert!(matches!(
        message.content.first(),
        Some(AssistantContent::ToolCall(tool_call))
            if tool_call.id == "toolu_1"
                && tool_call.name == "write_file"
                && tool_call.arguments == serde_json::json!({ "path": "C:\\q" })
    ));
}

#[tokio::test]
async fn anthropic_oauth_maps_claude_code_tool_names_both_directions() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured_request = Arc::new(Mutex::new(String::new()));
    let captured_for_server = captured_request.clone();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0_u8; 8192];
        let n = socket.read(&mut buf).await.unwrap();
        *captured_for_server.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();

        let body = [
            anthropic_event(
                "content_block_start",
                serde_json::json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": "Bash",
                        "input": {}
                    }
                }),
            ),
            anthropic_event(
                "content_block_stop",
                serde_json::json!({ "type": "content_block_stop", "index": 0 }),
            ),
            anthropic_event(
                "message_delta",
                serde_json::json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": "tool_use" },
                    "usage": { "output_tokens": 1 }
                }),
            ),
            anthropic_event(
                "message_stop",
                serde_json::json!({ "type": "message_stop" }),
            ),
        ]
        .join("");
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let provider = anthropic_provider();
    let model = anthropic_model(format!("http://{}", addr));
    let mut context = test_context();
    context.tools = Some(vec![Tool {
        name: "bash".to_string(),
        description: "Run a command".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
    }]);

    let mut stream = provider.stream(
        &model,
        &context,
        Some(&StreamOptions {
            api_key: Some("sk-ant-oat-test".to_string()),
            ..Default::default()
        }),
    );

    let mut final_message: Option<AssistantMessage> = None;
    while let Some(event) = stream.next().await {
        if let AssistantMessageEvent::Done { message, .. } = event {
            final_message = Some(message);
        }
    }
    server.await.unwrap();

    let request = captured_request.lock().unwrap().clone();
    let headers = request
        .split_once("\r\n\r\n")
        .expect("request headers")
        .0
        .to_ascii_lowercase();
    assert!(headers.contains("authorization: bearer sk-ant-oat-test"));
    assert!(!headers.contains("x-api-key: sk-ant-oat-test"));
    assert!(headers.contains("anthropic-beta:"));
    assert!(headers.contains("claude-code-20250219"));
    assert!(headers.contains("oauth-2025-04-20"));
    assert!(headers.contains("user-agent: claude-cli/2.1.75"));
    assert!(headers.contains("x-app: cli"));

    let (_, body) = request.split_once("\r\n\r\n").expect("request body");
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["tools"][0]["name"], serde_json::json!("Bash"));

    let message = final_message.expect("done message");
    assert!(matches!(
        message.content.first(),
        Some(AssistantContent::ToolCall(tool_call))
            if tool_call.name == "bash"
    ));
}

#[tokio::test]
async fn anthropic_stream_errors_when_sse_ends_before_message_stop() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0_u8; 4096];
        let _ = socket.read(&mut buf).await.unwrap();

        let body = anthropic_event(
            "message_start",
            serde_json::json!({
                "type": "message_start",
                "message": {
                    "id": "msg_1",
                    "usage": { "input_tokens": 1 }
                }
            }),
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let provider = anthropic_provider();
    let model = anthropic_model(format!("http://{}", addr));
    let mut stream = provider.stream(
        &model,
        &test_context(),
        Some(&StreamOptions {
            api_key: Some("test-key".to_string()),
            ..Default::default()
        }),
    );

    let mut error_message = None;
    while let Some(event) = stream.next().await {
        if let AssistantMessageEvent::Error { error, .. } = event {
            error_message = error.error_message;
        }
    }
    server.await.unwrap();

    assert_eq!(
        error_message.as_deref(),
        Some("Anthropic stream ended before message_stop")
    );
}

#[tokio::test]
async fn anthropic_stream_includes_http_error_response_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0_u8; 4096];
        let _ = socket.read(&mut buf).await.unwrap();

        let body = r#"{"error":{"message":"unsupported field: thinking"}}"#;
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let provider = anthropic_provider();
    let model = anthropic_model(format!("http://{}", addr));
    let mut stream = provider.stream(
        &model,
        &test_context(),
        Some(&StreamOptions {
            api_key: Some("test-key".to_string()),
            ..Default::default()
        }),
    );

    let mut error_message = None;
    while let Some(event) = stream.next().await {
        if let AssistantMessageEvent::Error { error, .. } = event {
            error_message = error.error_message;
        }
    }
    server.await.unwrap();

    let error_message = error_message.expect("error message");
    assert!(error_message.contains("HTTP 400"));
    assert!(error_message.contains("unsupported field: thinking"));
}

#[tokio::test]
async fn anthropic_simple_stream_maps_reasoning_to_thinking_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured_request = Arc::new(Mutex::new(String::new()));
    let captured_for_server = captured_request.clone();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0_u8; 8192];
        let n = socket.read(&mut buf).await.unwrap();
        *captured_for_server.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();

        let body = anthropic_event(
            "message_stop",
            serde_json::json!({ "type": "message_stop" }),
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let provider = anthropic_provider();
    let mut model = anthropic_model(format!("http://{}", addr));
    model.reasoning = true;
    let mut options = SimpleStreamOptions::default();
    options.base.api_key = Some("test-key".to_string());
    options.reasoning = Some(ThinkingLevel::High);

    let mut stream = provider.stream_simple(&model, &test_context(), Some(&options));
    while stream.next().await.is_some() {}
    server.await.unwrap();

    let request = captured_request.lock().unwrap().clone();
    let (_, body) = request.split_once("\r\n\r\n").expect("request body");
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(
        body["thinking"],
        serde_json::json!({
            "type": "enabled",
            "budget_tokens": 16384,
            "display": "summarized"
        })
    );
    assert_eq!(body["max_tokens"], serde_json::json!(65_536));
}

#[tokio::test]
async fn anthropic_simple_stream_expands_max_tokens_for_thinking_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured_request = Arc::new(Mutex::new(String::new()));
    let captured_for_server = captured_request.clone();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0_u8; 8192];
        let n = socket.read(&mut buf).await.unwrap();
        *captured_for_server.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();

        let body = anthropic_event(
            "message_stop",
            serde_json::json!({ "type": "message_stop" }),
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let provider = anthropic_provider();
    let mut model = anthropic_model(format!("http://{}", addr));
    model.reasoning = true;
    let mut options = SimpleStreamOptions::default();
    options.base.api_key = Some("test-key".to_string());
    options.base.max_tokens = Some(1024);
    options.reasoning = Some(ThinkingLevel::High);

    let mut stream = provider.stream_simple(&model, &test_context(), Some(&options));
    while stream.next().await.is_some() {}
    server.await.unwrap();

    let request = captured_request.lock().unwrap().clone();
    let (_, body) = request.split_once("\r\n\r\n").expect("request body");
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["max_tokens"], serde_json::json!(17_408));
    assert_eq!(body["thinking"]["budget_tokens"], serde_json::json!(16_384));
}

#[tokio::test]
async fn anthropic_stream_returns_aborted_when_signal_is_cancelled_after_payload_hook() {
    let provider = anthropic_provider();
    let signal = AbortSignal::new();
    let signal_for_hook = signal.clone();

    let mut stream = provider.stream(
        &anthropic_model("http://127.0.0.1:1".to_string()),
        &test_context(),
        Some(&StreamOptions {
            signal: Some(signal),
            on_payload: Some(Arc::new(move |_| {
                let signal = signal_for_hook.clone();
                Box::pin(async move {
                    signal.cancel();
                    None
                })
            })),
            ..Default::default()
        }),
    );

    let event = stream.next().await.expect("aborted event");
    assert!(matches!(
        event,
        AssistantMessageEvent::Error {
            reason: StopReason::Aborted,
            error: AssistantMessage {
                stop_reason: StopReason::Aborted,
                ..
            },
        }
    ));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn anthropic_stream_retries_retryable_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let attempts = Arc::new(Mutex::new(0_usize));
    let attempts_for_server = attempts.clone();

    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let _ = socket.read(&mut buf).await.unwrap();

            let attempt = {
                let mut attempts = attempts_for_server.lock().unwrap();
                *attempts += 1;
                *attempts
            };
            if attempt == 1 {
                let response = "HTTP/1.1 429 Too Many Requests\r\nretry-after-ms: 1\r\ncontent-length: 0\r\n\r\n";
                socket.write_all(response.as_bytes()).await.unwrap();
            } else {
                let body = anthropic_event(
                    "message_stop",
                    serde_json::json!({ "type": "message_stop" }),
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        }
    });

    let provider = anthropic_provider();
    let model = anthropic_model(format!("http://{}", addr));
    let mut stream = provider.stream(
        &model,
        &test_context(),
        Some(&StreamOptions {
            api_key: Some("test-key".to_string()),
            max_retries: Some(1),
            max_retry_delay_ms: Some(10),
            ..Default::default()
        }),
    );

    let mut final_message = None;
    while let Some(event) = stream.next().await {
        if let AssistantMessageEvent::Done { message, .. } = event {
            final_message = Some(message);
        }
    }
    server.await.unwrap();

    assert_eq!(*attempts.lock().unwrap(), 2);
    assert!(matches!(
        final_message,
        Some(AssistantMessage {
            stop_reason: StopReason::Stop,
            ..
        })
    ));
}

#[tokio::test]
async fn anthropic_cloudflare_gateway_resolves_base_url_and_uses_cf_auth_header() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured_request = Arc::new(Mutex::new(String::new()));
    let captured_for_server = captured_request.clone();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0_u8; 8192];
        let n = socket.read(&mut buf).await.unwrap();
        *captured_for_server.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();

        let body = anthropic_event(
            "message_stop",
            serde_json::json!({ "type": "message_stop" }),
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let provider = anthropic_provider();
    let mut model = anthropic_model(format!(
        "http://127.0.0.1:{}/{{FLOWN_AI_TEST_CLOUDFLARE_ANTHROPIC_PATH}}",
        addr.port()
    ));
    model.provider = Provider::Known(KnownProvider::CloudflareAiGateway);
    model.compat = None;
    let mut stream = {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var("FLOWN_AI_TEST_CLOUDFLARE_ANTHROPIC_PATH").ok();
        unsafe {
            std::env::set_var("FLOWN_AI_TEST_CLOUDFLARE_ANTHROPIC_PATH", "cf-anthropic");
        }
        let stream = provider.stream(
            &model,
            &test_context(),
            Some(&StreamOptions {
                api_key: Some("cf-test-key".to_string()),
                ..Default::default()
            }),
        );
        restore_env("FLOWN_AI_TEST_CLOUDFLARE_ANTHROPIC_PATH", previous);
        stream
    };

    while stream.next().await.is_some() {}
    server.await.unwrap();

    let request = captured_request.lock().unwrap().clone();
    assert!(request.starts_with("POST /cf-anthropic/v1/messages HTTP/1.1"));
    let headers = request
        .split_once("\r\n\r\n")
        .expect("request headers")
        .0
        .to_ascii_lowercase();
    assert!(has_header_line(
        &headers,
        "cf-aig-authorization: bearer cf-test-key"
    ));
    assert!(!has_header_line(&headers, "x-api-key: cf-test-key"));
    assert!(!has_header_line(
        &headers,
        "authorization: bearer cf-test-key"
    ));
}

#[tokio::test]
async fn anthropic_github_copilot_adds_dynamic_headers_for_agent_vision_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured_request = Arc::new(Mutex::new(String::new()));
    let captured_for_server = captured_request.clone();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0_u8; 8192];
        let n = socket.read(&mut buf).await.unwrap();
        *captured_for_server.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();

        let body = anthropic_event(
            "message_stop",
            serde_json::json!({ "type": "message_stop" }),
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let provider = anthropic_provider();
    let mut model = anthropic_model(format!("http://{}", addr));
    model.provider = Provider::Known(KnownProvider::GithubCopilot);
    model.input.push("image".to_string());
    let context = Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                role: "user".to_string(),
                content: MessageContent::Blocks(vec![UserContentBlock::Image(ImageContent {
                    content_type: "image".to_string(),
                    data: "aW1hZ2U=".to_string(),
                    mime_type: "image/png".to_string(),
                })]),
                timestamp: chrono::Utc::now(),
            }),
            Message::Assistant(AssistantMessage {
                role: "assistant".to_string(),
                content: vec![AssistantContent::Text(TextContent {
                    content_type: "text".to_string(),
                    text: "ok".to_string(),
                    text_signature: None,
                })],
                api: Api::Known(KnownApi::AnthropicMessages),
                provider: Provider::Known(KnownProvider::GithubCopilot),
                model: "copilot-test".to_string(),
                response_model: None,
                response_id: None,
                usage: Default::default(),
                stop_reason: StopReason::Stop,
                error_message: None,
                diagnostics: None,
                timestamp: chrono::Utc::now(),
            }),
        ],
        tools: None,
    };

    let mut stream = provider.stream(
        &model,
        &context,
        Some(&StreamOptions {
            api_key: Some("copilot-test-key".to_string()),
            ..Default::default()
        }),
    );

    while stream.next().await.is_some() {}
    server.await.unwrap();

    let request = captured_request.lock().unwrap().clone();
    let headers = request
        .split_once("\r\n\r\n")
        .expect("request headers")
        .0
        .to_ascii_lowercase();
    assert!(headers.contains("x-initiator: agent"));
    assert!(headers.contains("openai-intent: conversation-edits"));
    assert!(headers.contains("copilot-vision-request: true"));
}

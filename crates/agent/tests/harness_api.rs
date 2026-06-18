use async_trait::async_trait;
use flown_agent::{
    AgentHarness, AgentHarnessEvent, AgentHarnessOptions, AgentHarnessStreamOptions, AgentMessage,
    AgentTool, BranchPreparation, BranchSummaryDetails, CollectEntriesResult, CompactionSettings,
    ContextUsageEstimate, ExecOptions, ExecResult, ExecutionEnv, FileError, FileErrorCode,
    FileInfo, InMemorySessionRepo, MemorySessionCreateOptions, NavigateTreeOptions, QueueMode,
    SessionBeforeCompactResult, SessionBeforeTreeResult, SessionMessage, SessionRepo,
    SessionTreeEntry, SessionTreeSummaryResult, SystemPromptConfig, calculate_context_tokens,
    collect_entries_for_branch_summary_result, estimate_tokens, find_turn_start_index,
    generate_branch_summary, get_file_system_result_or_throw, get_last_assistant_usage,
    prepare_branch_entries, uuidv7,
};
use flown_ai::{
    Api, ApiProvider, AssistantContent, AssistantMessage, AssistantMessageEvent,
    AssistantMessageEventStream, Context, MessageContent, Model, ModelCost, Provider,
    ProviderResponse, RawEventStream, SimpleStreamOptions, StopReason, StreamOptions,
    ThinkingLevel, Usage, UserMessage, clear_api_providers, register_api_provider,
};
use parking_lot::Mutex;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone)]
struct TestExecutionEnv {
    cwd: String,
}

impl TestExecutionEnv {
    fn new() -> Self {
        Self {
            cwd: "/tmp/flown-agent-tests".to_string(),
        }
    }
}

#[async_trait]
impl flown_agent::FileSystem for TestExecutionEnv {
    fn cwd(&self) -> &str {
        &self.cwd
    }

    fn absolute_path(&self, path: &str) -> Result<String, FileError> {
        Ok(if path.starts_with('/') {
            path.to_string()
        } else {
            format!("{}/{}", self.cwd, path)
        })
    }

    fn join_path(&self, parts: &[&str]) -> Result<String, FileError> {
        Ok(parts.join("/"))
    }

    async fn read_text_file(&self, path: &str) -> Result<String, FileError> {
        Err(FileError::new(FileErrorCode::NotFound, path))
    }

    async fn read_text_lines(
        &self,
        path: &str,
        _max_lines: Option<usize>,
    ) -> Result<Vec<String>, FileError> {
        Err(FileError::new(FileErrorCode::NotFound, path))
    }

    async fn read_binary_file(&self, path: &str) -> Result<Vec<u8>, FileError> {
        Err(FileError::new(FileErrorCode::NotFound, path))
    }

    async fn write_file(&self, _path: &str, _content: &[u8]) -> Result<(), FileError> {
        Ok(())
    }

    async fn append_file(&self, _path: &str, _content: &[u8]) -> Result<(), FileError> {
        Ok(())
    }

    async fn file_info(&self, path: &str) -> Result<FileInfo, FileError> {
        Err(FileError::new(FileErrorCode::NotFound, path))
    }

    async fn list_dir(&self, _path: &str) -> Result<Vec<FileInfo>, FileError> {
        Ok(Vec::new())
    }

    async fn canonical_path(&self, path: &str) -> Result<String, FileError> {
        self.absolute_path(path)
    }

    async fn exists(&self, _path: &str) -> Result<bool, FileError> {
        Ok(false)
    }

    async fn create_dir(&self, _path: &str, _recursive: bool) -> Result<(), FileError> {
        Ok(())
    }

    async fn remove(&self, _path: &str, _recursive: bool, _force: bool) -> Result<(), FileError> {
        Ok(())
    }

    async fn create_temp_dir(&self, prefix: Option<&str>) -> Result<String, FileError> {
        Ok(format!("{}/{}-dir", self.cwd, prefix.unwrap_or("temp")))
    }

    async fn create_temp_file(
        &self,
        prefix: Option<&str>,
        suffix: Option<&str>,
    ) -> Result<String, FileError> {
        Ok(format!(
            "{}/{}{}",
            self.cwd,
            prefix.unwrap_or("temp"),
            suffix.unwrap_or(".tmp")
        ))
    }

    async fn cleanup(&self) -> Result<(), FileError> {
        Ok(())
    }
}

#[async_trait]
impl flown_agent::Shell for TestExecutionEnv {
    async fn exec(
        &self,
        _command: &str,
        _options: ExecOptions,
    ) -> Result<ExecResult, flown_agent::ExecutionError> {
        Ok(ExecResult {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
        })
    }

    async fn cleanup(&self) -> Result<(), flown_agent::ExecutionError> {
        Ok(())
    }
}

impl ExecutionEnv for TestExecutionEnv {}

#[derive(Default)]
struct TestProviderState {
    payloads: Mutex<Vec<serde_json::Value>>,
    headers: Mutex<Vec<Option<HashMap<String, String>>>>,
    model_ids: Mutex<Vec<String>>,
    reasoning_levels: Mutex<Vec<Option<ThinkingLevel>>>,
    gate_first_call: AtomicBool,
    first_call_started: tokio::sync::Notify,
    release_first_call: tokio::sync::Notify,
}

struct TestApiProvider {
    state: Arc<TestProviderState>,
}

impl TestApiProvider {
    fn new(state: Arc<TestProviderState>) -> Self {
        Self { state }
    }
}

impl ApiProvider for TestApiProvider {
    fn api(&self) -> Api {
        Api::Custom("test-stream-api".to_string())
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
    ) -> AssistantMessageEventStream {
        self.stream_simple(
            model,
            context,
            Some(&SimpleStreamOptions {
                base: options.cloned().unwrap_or_default(),
                reasoning: None,
                thinking_budgets: None,
            }),
        )
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        let model = model.clone();
        let prompt = context
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                flown_ai::Message::User(UserMessage {
                    content: MessageContent::Text(text),
                    ..
                }) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "missing".to_string());
        let state = self.state.clone();
        let on_payload = options
            .as_ref()
            .and_then(|options| options.base.on_payload.clone());
        let on_response = options
            .as_ref()
            .and_then(|options| options.base.on_response.clone());
        let headers = options
            .as_ref()
            .and_then(|options| options.base.headers.clone());
        let reasoning = options
            .as_ref()
            .and_then(|options| options.reasoning.clone());

        AssistantMessageEventStream::from_stream(Box::pin(async_stream::stream! {
            let call_index = {
                let mut model_ids = state.model_ids.lock();
                let index = model_ids.len();
                model_ids.push(model.id.clone());
                state.reasoning_levels.lock().push(reasoning.clone());
                index
            };
            if call_index == 0 && state.gate_first_call.load(Ordering::SeqCst) {
                state.first_call_started.notify_one();
                state.release_first_call.notified().await;
            }

            let mut payload = json!({
                "messages": [{"role": "user", "content": prompt}],
            });

            if let Some(hook) = on_payload {
                if let Some(next_payload) = hook(payload.clone()).await {
                    payload = next_payload;
                }
            }

            state.payloads.lock().push(payload.clone());
            state.headers.lock().push(headers.clone());

            if let Some(hook) = on_response {
                hook(ProviderResponse {
                    status: 207,
                    headers: HashMap::from([
                        ("x-test-response".to_string(), "ok".to_string()),
                        ("content-type".to_string(), "application/json".to_string()),
                    ]),
                }).await;
            }

            let text = payload
                .get("messages")
                .and_then(|messages| messages.get(0))
                .and_then(|message| message.get("content"))
                .and_then(|content| content.as_str())
                .unwrap_or("missing")
                .to_string();

            let assistant = AssistantMessage {
                role: "assistant".to_string(),
                content: vec![AssistantContent::Text(flown_ai::TextContent {
                    content_type: "text".to_string(),
                    text: format!("echo:{text}"),
                    text_signature: None,
                })],
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                response_model: None,
                response_id: Some("resp_test".to_string()),
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                error_message: None,
                diagnostics: None,
                timestamp: chrono::Utc::now(),
            };

            yield AssistantMessageEvent::Start {
                partial: AssistantMessage {
                    content: vec![],
                    ..assistant.clone()
                },
            };
            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: assistant,
            };
        }) as RawEventStream)
    }
}

fn test_model() -> Model {
    Model {
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        api: Api::Custom("test-stream-api".to_string()),
        provider: Provider::Custom("test-provider".to_string()),
        base_url: "https://example.invalid".to_string(),
        reasoning: false,
        thinking_level_map: None,
        input: vec!["text".to_string()],
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 4096,
        max_tokens: 1024,
        headers: None,
        compat: None,
    }
}

async fn test_harness() -> AgentHarness {
    let repo = InMemorySessionRepo::new();
    let session = repo
        .create(MemorySessionCreateOptions {
            id: Some("test-session".to_string()),
        })
        .await
        .expect("in-memory session");
    AgentHarness::new(AgentHarnessOptions {
        env: Arc::new(TestExecutionEnv::new()),
        session,
        tools: Vec::<AgentTool>::new(),
        resources: None,
        system_prompt: SystemPromptConfig::Static("You are a test harness.".to_string()),
        get_api_key_and_headers: Some(Arc::new(|_model| Some(("test-api-key".to_string(), None)))),
        stream_options: Some(AgentHarnessStreamOptions::default()),
        model: test_model(),
        thinking_level: Some(ThinkingLevel::Off),
        active_tool_names: Some(Vec::new()),
        steering_mode: Some(QueueMode::OneAtATime),
        follow_up_mode: Some(QueueMode::OneAtATime),
    })
}

fn user_text(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage {
        role: "user".to_string(),
        content: MessageContent::Text(text.to_string()),
        timestamp: chrono::Utc::now(),
    })
}

fn assistant_text(text: &str, total_tokens: u32) -> AgentMessage {
    AgentMessage::Assistant(AssistantMessage {
        role: "assistant".to_string(),
        content: vec![AssistantContent::Text(flown_ai::TextContent {
            content_type: "text".to_string(),
            text: text.to_string(),
            text_signature: None,
        })],
        api: Api::Custom("history-api".to_string()),
        provider: Provider::Custom("history-provider".to_string()),
        model: "history-model".to_string(),
        response_model: None,
        response_id: None,
        usage: Usage {
            total_tokens,
            ..Usage::default()
        },
        stop_reason: StopReason::Stop,
        error_message: None,
        diagnostics: None,
        timestamp: chrono::Utc::now(),
    })
}

#[test]
fn uuidv7_is_exported_and_looks_like_uuid() {
    let id = uuidv7();
    assert_eq!(id.len(), 36);
    assert_eq!(id.chars().nth(14), Some('7'));
    assert_eq!(id.chars().nth(8), Some('-'));
    assert_eq!(id.chars().nth(13), Some('-'));
    assert_eq!(id.chars().nth(18), Some('-'));
    assert_eq!(id.chars().nth(23), Some('-'));
}

#[test]
fn branch_summary_public_types_are_exported() {
    let details = BranchSummaryDetails {
        read_files: vec!["src/lib.rs".to_string()],
        modified_files: vec!["src/main.rs".to_string()],
    };
    let prep = BranchPreparation {
        messages: vec![],
        read_files: vec![],
        modified_files: vec![],
        total_tokens: 0,
    };
    let collected = CollectEntriesResult {
        entries: vec![],
        common_ancestor_id: None,
    };
    assert_eq!(details.read_files, vec!["src/lib.rs".to_string()]);
    assert_eq!(prep.total_tokens, 0);
    assert!(collected.entries.is_empty());
}

#[test]
fn branch_summary_public_functions_are_exported() {
    let _ = collect_entries_for_branch_summary_result;
    let _ = generate_branch_summary;
    let _ = prepare_branch_entries;
}

#[test]
fn compaction_public_surface_matches_pi_mono_exports() {
    let settings = CompactionSettings::default();
    assert!(settings.enabled);

    let usage_estimate = ContextUsageEstimate {
        tokens: 0,
        usage_tokens: 0,
        trailing_tokens: 0,
        last_usage_index: None,
    };
    assert_eq!(usage_estimate.tokens, 0);

    let _ = calculate_context_tokens;
    let _ = estimate_tokens;
    let _ = find_turn_start_index;
    let _ = get_last_assistant_usage;
}

#[test]
fn session_repo_utils_public_surface_matches_pi_mono_exports() {
    let _ = get_file_system_result_or_throw::<String>;
}

#[tokio::test]
async fn pi_mono_aligned_harness_getters_reflect_live_state() {
    let harness = test_harness().await;

    assert_eq!(harness.get_model().await.id, "test-model");
    assert_eq!(harness.get_thinking_level().await, ThinkingLevel::Off);
    assert_eq!(harness.get_steering_mode().await, QueueMode::OneAtATime);
    assert_eq!(harness.get_follow_up_mode().await, QueueMode::OneAtATime);
    assert!(harness.get_tools().is_empty());
    assert!(harness.get_active_tools().is_empty());
    assert!(harness.get_resources().await.skills.is_empty());
    assert!(harness.get_resources().await.prompt_templates.is_empty());
    let stream_options = harness.get_stream_options().await;
    assert!(stream_options.headers.is_none());
    assert!(stream_options.metadata.is_none());
    assert!(stream_options.transport.is_none());
    assert!(stream_options.timeout_ms.is_none());
    assert!(stream_options.max_retries.is_none());
    assert!(stream_options.max_retry_delay_ms.is_none());
    assert!(stream_options.cache_retention.is_none());

    let resolved_prompt = harness.get_system_prompt().await;
    assert_eq!(resolved_prompt, "You are a test harness.");

    harness.set_steering_mode(QueueMode::All).await;
    harness.set_follow_up_mode(QueueMode::All).await;

    assert_eq!(harness.get_steering_mode().await, QueueMode::All);
    assert_eq!(harness.get_follow_up_mode().await, QueueMode::All);
}

#[tokio::test]
async fn prompt_emits_stream_and_queue_lifecycle_hooks_via_public_api() {
    let _guard = REGISTRY_LOCK.lock();
    clear_api_providers();
    let provider_state = Arc::new(TestProviderState::default());
    register_api_provider(Arc::new(TestApiProvider::new(provider_state.clone())));

    let harness = test_harness().await;
    harness
        .set_stream_options(AgentHarnessStreamOptions {
            headers: Some(HashMap::from([
                ("x-base".to_string(), "base".to_string()),
                ("x-remove".to_string(), "remove-me".to_string()),
            ])),
            metadata: Some(HashMap::from([("base".to_string(), json!(true))])),
            ..AgentHarnessStreamOptions::default()
        })
        .await;

    let queue_updates = Arc::new(Mutex::new(Vec::new()));
    let response_status = Arc::new(Mutex::new(Vec::new()));
    let save_points = Arc::new(Mutex::new(Vec::new()));
    let settled = Arc::new(Mutex::new(Vec::new()));

    let queue_updates_ref = queue_updates.clone();
    let response_status_ref = response_status.clone();
    let save_points_ref = save_points.clone();
    let settled_ref = settled.clone();
    let _unsubscribe = harness.subscribe(move |event, _signal| {
        let queue_updates_ref = queue_updates_ref.clone();
        let response_status_ref = response_status_ref.clone();
        let save_points_ref = save_points_ref.clone();
        let settled_ref = settled_ref.clone();
        Box::pin(async move {
            match event {
                AgentHarnessEvent::QueueUpdate {
                    steer,
                    follow_up,
                    next_turn,
                } => {
                    queue_updates_ref
                        .lock()
                        .push((steer.len(), follow_up.len(), next_turn.len()));
                }
                AgentHarnessEvent::AfterProviderResponse { status, headers } => {
                    response_status_ref
                        .lock()
                        .push((status, headers.get("x-test-response").cloned()));
                }
                AgentHarnessEvent::SavePoint {
                    had_pending_mutations,
                } => {
                    save_points_ref.lock().push(had_pending_mutations);
                }
                AgentHarnessEvent::Settled { next_turn_count } => {
                    settled_ref.lock().push(next_turn_count);
                }
                _ => {}
            }
        })
    });

    let before_request_events = Arc::new(Mutex::new(Vec::new()));
    let before_request_events_ref = before_request_events.clone();
    let _before_request = harness.on("before_provider_request", move |event| {
        let before_request_events_ref = before_request_events_ref.clone();
        Box::pin(async move {
            if let AgentHarnessEvent::BeforeProviderRequest {
                session_id,
                stream_options,
                ..
            } = event
            {
                before_request_events_ref.lock().push((
                    session_id,
                    stream_options.headers.clone(),
                    stream_options.metadata.clone(),
                ));
            }
            Some(json!({
                "streamOptions": {
                    "headers": {
                        "x-added": "hooked",
                        "x-remove": null
                    },
                    "metadata": {
                        "hooked": true
                    }
                }
            }))
        })
    });

    let _before_payload = harness.on("before_provider_payload", move |event| {
        Box::pin(async move {
            if let AgentHarnessEvent::BeforeProviderPayload { mut payload, .. } = event {
                payload["messages"][0]["content"] = json!("hooked prompt");
                return Some(json!({ "payload": payload }));
            }
            None
        })
    });

    harness.next_turn("queued next turn", None).await;
    let result = harness.prompt("original prompt", None).await.unwrap();

    let text = result
        .content
        .iter()
        .find_map(|content| match content {
            AssistantContent::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(text, "echo:hooked prompt");

    let observed_payloads = provider_state.payloads.lock().clone();
    assert_eq!(observed_payloads.len(), 1);
    assert_eq!(
        observed_payloads[0]["messages"][0]["content"],
        json!("hooked prompt")
    );

    let observed_headers = provider_state.headers.lock().clone();
    assert_eq!(observed_headers.len(), 1);
    let headers = observed_headers[0].clone().expect("headers should exist");
    assert_eq!(headers.get("x-base"), Some(&"base".to_string()));
    assert_eq!(headers.get("x-added"), Some(&"hooked".to_string()));
    assert!(!headers.contains_key("x-remove"));

    let before_request_events = before_request_events.lock().clone();
    assert_eq!(before_request_events.len(), 1);
    assert_eq!(before_request_events[0].0, "test-session");
    assert_eq!(
        before_request_events[0]
            .1
            .as_ref()
            .and_then(|headers| headers.get("x-base"))
            .cloned(),
        Some("base".to_string())
    );
    assert_eq!(
        before_request_events[0]
            .2
            .as_ref()
            .and_then(|metadata| metadata.get("base"))
            .cloned(),
        Some(json!(true))
    );

    assert_eq!(
        response_status.lock().clone(),
        vec![(207, Some("ok".to_string()))]
    );
    assert_eq!(save_points.lock().clone(), vec![false]);
    assert_eq!(settled.lock().clone(), vec![0]);
    assert!(
        queue_updates
            .lock()
            .iter()
            .any(|snapshot| *snapshot == (0, 0, 1))
    );

    let session_entries = harness.session().get_entries().await;
    let user_messages: Vec<String> = session_entries
        .iter()
        .filter_map(|entry| match entry {
            SessionTreeEntry::Message {
                message: SessionMessage(AgentMessage::User(msg)),
                ..
            } => match &msg.content {
                MessageContent::Text(text) => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        user_messages,
        vec![
            "queued next turn".to_string(),
            "original prompt".to_string()
        ]
    );
}

#[tokio::test]
async fn compact_uses_session_before_compact_hook_and_emits_hooked_save_entry() {
    println!("test: start compact_uses_session_before_compact_hook_and_emits_hooked_save_entry");
    let harness = test_harness().await;
    harness
        .session()
        .append_message(user_text("older user"))
        .await;
    let first_kept_entry_id = harness
        .session()
        .append_message(assistant_text("older assistant", 64))
        .await;
    harness
        .session()
        .append_message(user_text("latest user that should stay"))
        .await;

    let compact_events = Arc::new(Mutex::new(Vec::new()));
    let compact_events_ref = compact_events.clone();
    let _unsubscribe = harness.subscribe(move |event, _signal| {
        let compact_events_ref = compact_events_ref.clone();
        Box::pin(async move {
            if let AgentHarnessEvent::SessionCompact {
                compaction_entry,
                from_hook,
            } = event
            {
                println!("event: SessionCompact from_hook={from_hook}");
                compact_events_ref
                    .lock()
                    .push((compaction_entry.clone(), from_hook));
            }
        })
    });

    let expected_first_kept_entry_id = first_kept_entry_id.clone();
    let _hook = harness.on("session_before_compact", move |event| {
        let first_kept_entry_id = expected_first_kept_entry_id.clone();
        Box::pin(async move {
            println!("hook: session_before_compact enter");
            if let AgentHarnessEvent::SessionBeforeCompact {
                branch_entries,
                custom_instructions,
                ..
            } = &event
            {
                assert_eq!(branch_entries.len(), 3);
                assert_eq!(custom_instructions.as_deref(), Some("keep recent context"));
            } else {
                panic!("unexpected event type for compaction hook");
            }
            println!("hook: session_before_compact return");
            Some(
                serde_json::to_value(SessionBeforeCompactResult {
                    cancel: None,
                    compaction: Some(flown_agent::CompactResult {
                        summary: "hook summary".to_string(),
                        first_kept_entry_id,
                        tokens_before: 64,
                        details: Some(json!({ "readFiles": ["src/lib.rs"] })),
                    }),
                })
                .unwrap(),
            )
        })
    });

    println!("test: before compact await");
    let result = harness.compact(Some("keep recent context")).await.unwrap();
    println!("test: after compact await");
    assert_eq!(result.summary, "hook summary");
    assert_eq!(result.first_kept_entry_id, first_kept_entry_id);

    println!("test: before event assertions");
    let events = compact_events.lock().clone();
    assert_eq!(events.len(), 1);
    assert!(events[0].1);
    match events[0].0.clone().expect("compaction entry") {
        SessionTreeEntry::Compaction {
            summary,
            first_kept_entry_id,
            from_hook,
            ..
        } => {
            assert_eq!(summary, "hook summary");
            assert_eq!(first_kept_entry_id, result.first_kept_entry_id);
            assert_eq!(from_hook, Some(true));
        }
        other => panic!("expected compaction entry, got {other:?}"),
    }
    println!("test: end");
}

#[tokio::test]
async fn navigate_tree_uses_session_before_tree_hook_and_emits_session_tree() {
    let harness = test_harness().await;
    let first_user_id = harness.session().append_message(user_text("first")).await;
    harness
        .session()
        .append_message(assistant_text("assistant on first branch", 0))
        .await;
    let second_user_id = harness.session().append_message(user_text("second")).await;

    let tree_events = Arc::new(Mutex::new(Vec::new()));
    let tree_events_ref = tree_events.clone();
    let _unsubscribe = harness.subscribe(move |event, _signal| {
        let tree_events_ref = tree_events_ref.clone();
        Box::pin(async move {
            if let AgentHarnessEvent::SessionTree {
                new_leaf_id,
                old_leaf_id,
                summary_entry,
                from_hook,
            } = event
            {
                tree_events_ref.lock().push((
                    new_leaf_id.clone(),
                    old_leaf_id.clone(),
                    summary_entry.clone(),
                    from_hook,
                ));
            }
        })
    });

    let expected_first_user_id = first_user_id.clone();
    let expected_second_user_id = second_user_id.clone();
    let _hook = harness.on("session_before_tree", move |event| {
        let first_user_id = expected_first_user_id.clone();
        let second_user_id = expected_second_user_id.clone();
        Box::pin(async move {
            if let AgentHarnessEvent::SessionBeforeTree { preparation, .. } = &event {
                assert_eq!(preparation.target_id, first_user_id);
                assert_eq!(
                    preparation.old_leaf_id.as_deref(),
                    Some(second_user_id.as_str())
                );
                assert!(preparation.user_wants_summary);
                assert_eq!(
                    preparation.custom_instructions.as_deref(),
                    Some("summarize branch")
                );
            } else {
                panic!("unexpected event type for tree hook");
            }
            Some(
                serde_json::to_value(SessionBeforeTreeResult {
                    cancel: None,
                    summary: Some(SessionTreeSummaryResult {
                        summary: "hook branch summary".to_string(),
                        details: Some(json!({ "label": "Hook label" })),
                    }),
                    custom_instructions: Some("overridden".to_string()),
                    replace_instructions: Some(true),
                    label: Some("Hook label".to_string()),
                })
                .unwrap(),
            )
        })
    });

    let result = harness
        .navigate_tree(
            &first_user_id,
            NavigateTreeOptions {
                summarize: true,
                custom_instructions: Some("summarize branch".to_string()),
                replace_instructions: Some(false),
                label: Some("Original label".to_string()),
            },
        )
        .await
        .unwrap();

    assert!(!result.cancelled);
    match result.summary_entry.clone().expect("summary entry") {
        SessionTreeEntry::BranchSummary {
            summary,
            from_id,
            from_hook,
            ..
        } => {
            assert_eq!(summary, "hook branch summary");
            assert_eq!(from_id, first_user_id);
            assert_eq!(from_hook, Some(true));
        }
        other => panic!("expected branch summary entry, got {other:?}"),
    }

    let events = tree_events.lock().clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0.as_deref(), Some(first_user_id.as_str()));
    assert_eq!(events[0].1.as_deref(), Some(second_user_id.as_str()));
    assert!(events[0].3);
}

#[tokio::test]
async fn wait_for_idle_waits_for_async_subscribers() {
    println!("test: wait_for_idle_waits_for_async_subscribers start");
    let _guard = REGISTRY_LOCK.lock();
    clear_api_providers();
    let provider_state = Arc::new(TestProviderState::default());
    register_api_provider(Arc::new(TestApiProvider::new(provider_state)));

    let harness = test_harness().await;
    let release = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(tokio::sync::Notify::new());

    let release_ref = release.clone();
    let entered_ref = entered.clone();
    let _unsubscribe = harness.subscribe(move |event, _signal| {
        let release_ref = release_ref.clone();
        let entered_ref = entered_ref.clone();
        Box::pin(async move {
            if matches!(event, AgentHarnessEvent::Settled { .. }) {
                println!("test: subscriber received settled");
                entered_ref.notify_one();
                println!("test: subscriber waiting for release");
                release_ref.notified().await;
                println!("test: subscriber released");
            }
        })
    });

    println!("test: prompt future created");
    let prompt_future = harness.prompt("hello", None);
    tokio::pin!(prompt_future);

    println!("test: waiting for entered notification");
    let entered_wait = entered.notified();
    tokio::pin!(entered_wait);
    tokio::select! {
        _ = &mut entered_wait => {
            println!("test: entered notification received");
        }
        result = &mut prompt_future => panic!("prompt completed too early: {result:?}"),
    }

    println!("test: notifying release");
    release.notify_one();

    println!("test: awaiting prompt completion");
    let message = prompt_future.await.unwrap();
    println!("test: prompt completed");
    let text = message
        .content
        .iter()
        .find_map(|content| match content {
            AssistantContent::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(text, "echo:hello");
    println!("test: wait_for_idle_waits_for_async_subscribers end");
}

#[tokio::test]
async fn live_state_updates_apply_to_follow_up_turns() {
    let _guard = REGISTRY_LOCK.lock();
    clear_api_providers();
    let provider_state = Arc::new(TestProviderState::default());
    provider_state.gate_first_call.store(true, Ordering::SeqCst);
    register_api_provider(Arc::new(TestApiProvider::new(provider_state.clone())));

    let harness = Arc::new(test_harness().await);
    let prompt_future = harness.prompt("first prompt", None);
    tokio::pin!(prompt_future);

    let first_call_started = provider_state.first_call_started.notified();
    tokio::pin!(first_call_started);
    tokio::select! {
        _ = &mut first_call_started => {}
        result = &mut prompt_future => panic!("prompt completed too early: {result:?}"),
    }

    harness.follow_up("second prompt", None).await.unwrap();

    let mut second_model = test_model();
    second_model.id = "second-model".to_string();
    second_model.name = "Second Model".to_string();
    harness.set_model(second_model).await;
    harness.set_thinking_level(ThinkingLevel::High).await;

    provider_state.release_first_call.notify_one();

    let final_message = prompt_future.await.unwrap();
    let final_text = final_message
        .content
        .iter()
        .find_map(|content| match content {
            AssistantContent::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .unwrap();

    assert_eq!(final_text, "echo:second prompt");
    assert_eq!(
        provider_state.model_ids.lock().clone(),
        vec!["test-model".to_string(), "second-model".to_string()]
    );
    assert_eq!(
        provider_state.reasoning_levels.lock().clone(),
        vec![None, Some(ThinkingLevel::High)]
    );
}

#[tokio::test]
async fn stream_options_updates_apply_to_follow_up_turns() {
    let _guard = REGISTRY_LOCK.lock();
    clear_api_providers();
    let provider_state = Arc::new(TestProviderState::default());
    provider_state.gate_first_call.store(true, Ordering::SeqCst);
    register_api_provider(Arc::new(TestApiProvider::new(provider_state.clone())));

    let harness = Arc::new(test_harness().await);
    harness
        .set_stream_options(AgentHarnessStreamOptions {
            headers: Some(HashMap::from([(
                "x-phase".to_string(),
                "first".to_string(),
            )])),
            ..AgentHarnessStreamOptions::default()
        })
        .await;

    let run_harness = harness.clone();
    let prompt_future = run_harness.prompt("first prompt", None);
    tokio::pin!(prompt_future);

    let first_call_started = provider_state.first_call_started.notified();
    tokio::pin!(first_call_started);
    tokio::select! {
        _ = &mut first_call_started => {}
        result = &mut prompt_future => panic!("prompt completed too early: {result:?}"),
    }

    harness.follow_up("second prompt", None).await.unwrap();
    harness
        .set_stream_options(AgentHarnessStreamOptions {
            headers: Some(HashMap::from([(
                "x-phase".to_string(),
                "second".to_string(),
            )])),
            ..AgentHarnessStreamOptions::default()
        })
        .await;

    provider_state.release_first_call.notify_one();

    let final_message = prompt_future.await.unwrap();
    let final_text = final_message
        .content
        .iter()
        .find_map(|content| match content {
            AssistantContent::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .unwrap();

    assert_eq!(final_text, "echo:second prompt");
    let observed_headers = provider_state.headers.lock().clone();
    assert_eq!(observed_headers.len(), 2);
    assert_eq!(
        observed_headers[0]
            .as_ref()
            .and_then(|headers| headers.get("x-phase"))
            .cloned(),
        Some("first".to_string())
    );
    assert_eq!(
        observed_headers[1]
            .as_ref()
            .and_then(|headers| headers.get("x-phase"))
            .cloned(),
        Some("second".to_string())
    );
}

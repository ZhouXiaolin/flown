use super::compaction::compaction::CompactionResult;
use super::env::ExecutionEnv;
use super::prompt_templates::format_prompt_template_invocation;
use super::session::Session;
use super::skills::{format_skill_invocation, format_skills_for_system_prompt};
use super::types::*;
use crate::agent_loop::agent_loop;
use crate::types::*;
use flown_ai::{
    AbortSignal, AssistantContent, AssistantMessage, AssistantMessageEvent, Context, ImageContent,
    Message, MessageContent, Model, SimpleStreamOptions, StopReason, TextContent, ThinkingLevel,
    Tool, ToolResultContent, Usage, UserContentBlock, UserMessage,
};
use futures::future::BoxFuture;
use futures::stream::StreamExt;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

type EventHandler =
    Arc<dyn Fn(HarnessEvent, Option<AbortSignal>) -> BoxFuture<'static, ()> + Send + Sync>;
type HookHandler = Arc<
    dyn Fn(HarnessEvent) -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send>>
        + Send
        + Sync,
>;
pub type GetApiKeyAndHeadersFn =
    Arc<dyn Fn(&Model) -> Option<(String, Option<HashMap<String, String>>)> + Send + Sync>;

/// Subscriber entry with unique ID
struct SubscriberEntry {
    id: usize,
    handler: EventHandler,
}

/// Hook entry with unique ID
struct HookEntry {
    id: usize,
    handler: HookHandler,
}

/// Create a user message with optional images
fn create_user_message(text: &str, images: Option<Vec<ImageContent>>) -> AgentMessage {
    let content = if let Some(images) = images {
        let mut blocks = vec![UserContentBlock::Text(TextContent {
            content_type: "text".to_string(),
            text: text.to_string(),
            text_signature: None,
        })];
        for image in images {
            blocks.push(UserContentBlock::Image(image));
        }
        MessageContent::Blocks(blocks)
    } else {
        MessageContent::Text(text.to_string())
    };

    AgentMessage::User(UserMessage {
        role: "user".to_string(),
        content,
        timestamp: chrono::Utc::now(),
    })
}

fn validate_tool_names(
    names: &[String],
    tools: &HashMap<String, AgentTool>,
) -> Result<(), HarnessError> {
    let mut seen = std::collections::HashSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(HarnessError::InvalidArgument(format!(
                "Duplicate active tool name: {}",
                name
            )));
        }
        if !tools.contains_key(name) {
            return Err(HarnessError::InvalidArgument(format!(
                "Unknown active tool: {}",
                name
            )));
        }
    }
    Ok(())
}

fn apply_stream_options_patch(
    base: &HarnessStreamOptions,
    patch: &serde_json::Value,
) -> HarnessStreamOptions {
    let mut result = base.clone();
    let Some(patch) = patch.as_object() else {
        return result;
    };

    if let Some(value) = patch.get("transport") {
        result.transport = serde_json::from_value(value.clone()).ok();
    }
    if let Some(value) = patch.get("timeoutMs").or_else(|| patch.get("timeout_ms")) {
        result.timeout_ms = value.as_u64();
    }
    if let Some(value) = patch.get("maxRetries").or_else(|| patch.get("max_retries")) {
        result.max_retries = value.as_u64().and_then(|value| u32::try_from(value).ok());
    }
    if let Some(value) = patch
        .get("maxRetryDelayMs")
        .or_else(|| patch.get("max_retry_delay_ms"))
    {
        result.max_retry_delay_ms = value.as_u64();
    }
    if let Some(value) = patch
        .get("cacheRetention")
        .or_else(|| patch.get("cache_retention"))
    {
        result.cache_retention = serde_json::from_value(value.clone()).ok();
    }

    if patch.contains_key("headers") {
        result.headers = apply_headers_patch(result.headers, patch.get("headers"));
    }
    if patch.contains_key("metadata") {
        result.metadata = apply_metadata_patch(result.metadata, patch.get("metadata"));
    }

    result
}

fn apply_headers_patch(
    base: Option<HashMap<String, String>>,
    patch: Option<&serde_json::Value>,
) -> Option<HashMap<String, String>> {
    let Some(patch) = patch else {
        return base;
    };
    if patch.is_null() {
        return None;
    }

    let Some(patch) = patch.as_object() else {
        return base;
    };
    let mut headers = base.unwrap_or_default();
    for (key, value) in patch {
        if value.is_null() {
            headers.remove(key);
        } else if let Some(value) = value.as_str() {
            headers.insert(key.clone(), value.to_string());
        }
    }
    (!headers.is_empty()).then_some(headers)
}

fn apply_metadata_patch(
    base: Option<HashMap<String, serde_json::Value>>,
    patch: Option<&serde_json::Value>,
) -> Option<HashMap<String, serde_json::Value>> {
    let Some(patch) = patch else {
        return base;
    };
    if patch.is_null() {
        return None;
    }

    let Some(patch) = patch.as_object() else {
        return base;
    };
    let mut metadata = base.unwrap_or_default();
    for (key, value) in patch {
        if value.is_null() {
            metadata.remove(key);
        } else {
            metadata.insert(key.clone(), value.clone());
        }
    }
    (!metadata.is_empty()).then_some(metadata)
}

fn merge_headers(
    first: Option<HashMap<String, String>>,
    second: Option<HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    let mut merged = HashMap::new();
    if let Some(headers) = first {
        merged.extend(headers);
    }
    if let Some(headers) = second {
        merged.extend(headers);
    }
    (!merged.is_empty()).then_some(merged)
}

/// Agent harness - high-level orchestration layer
pub struct AgentHarness {
    // Core state
    env: Arc<dyn ExecutionEnv>,
    session: Arc<Session>,
    phase: Arc<RwLock<AgentHarnessPhase>>,
    idle_tx: flume::Sender<()>,
    idle_rx: flume::Receiver<()>,
    run_abort: Arc<RwLock<Option<AbortSignal>>>,

    // Configuration
    model: Arc<RwLock<Model>>,
    thinking_level: Arc<RwLock<ThinkingLevel>>,
    tools: Arc<RwLock<HashMap<String, AgentTool>>>,
    active_tool_names: Arc<RwLock<Vec<String>>>,
    resources: Arc<RwLock<HarnessResources>>,
    stream_options: Arc<RwLock<HarnessStreamOptions>>,
    system_prompt: Arc<RwLock<SystemPromptConfig>>,

    // Queues
    steer_queue: Arc<RwLock<Vec<AgentMessage>>>,
    follow_up_queue: Arc<RwLock<Vec<AgentMessage>>>,
    next_turn_queue: Arc<RwLock<Vec<AgentMessage>>>,

    // Queue modes
    steering_mode: Arc<RwLock<QueueMode>>,
    follow_up_mode: Arc<RwLock<QueueMode>>,

    // Pending session writes
    pending_writes: Arc<RwLock<Vec<PendingSessionWrite>>>,

    // Event handlers
    subscribers: Arc<RwLock<Vec<SubscriberEntry>>>,
    hooks: Arc<RwLock<HashMap<String, Vec<HookEntry>>>>,
    next_subscriber_id: Arc<Mutex<usize>>,
    next_hook_id: Arc<Mutex<usize>>,

    // API key provider
    get_api_key_and_headers: Option<GetApiKeyAndHeadersFn>,
}

/// System prompt configuration
#[derive(Clone)]
pub enum SystemPromptConfig {
    Static(String),
    Dynamic(Arc<dyn Fn(&SystemPromptContext) -> String + Send + Sync>),
    AsyncDynamic(
        Arc<
            dyn Fn(&SystemPromptContext) -> Pin<Box<dyn Future<Output = String> + Send>>
                + Send
                + Sync,
        >,
    ),
}

/// Context for dynamic system prompt generation
pub struct SystemPromptContext {
    pub env: Arc<dyn ExecutionEnv>,
    pub session: Arc<Session>,
    pub model: Model,
    pub thinking_level: ThinkingLevel,
    pub active_tools: Vec<AgentTool>,
    pub resources: HarnessResources,
}

/// Pending session write (without id/parentId/timestamp)
#[derive(Debug, Clone)]
pub enum PendingSessionWrite {
    Message(AgentMessage),
    ModelChange {
        provider: String,
        model_id: String,
    },
    ThinkingLevelChange {
        level: String,
    },
    ActiveToolsChange {
        active_tool_names: Vec<String>,
    },
    Label {
        target_id: String,
        label: Option<String>,
    },
    SessionInfo {
        name: Option<String>,
    },
    Custom {
        custom_type: String,
        data: serde_json::Value,
    },
    CustomMessage {
        custom_type: String,
        content: String,
        display: Option<String>,
        details: Option<serde_json::Value>,
    },
    Leaf {
        target_id: String,
    },
}

/// Harness constructor options
pub struct AgentHarnessOptions {
    pub env: Arc<dyn ExecutionEnv>,
    pub session: Session,
    pub tools: Vec<AgentTool>,
    pub resources: Option<HarnessResources>,
    pub system_prompt: SystemPromptConfig,
    pub get_api_key_and_headers: Option<GetApiKeyAndHeadersFn>,
    pub stream_options: Option<HarnessStreamOptions>,
    pub model: Model,
    pub thinking_level: Option<ThinkingLevel>,
    pub active_tool_names: Option<Vec<String>>,
    pub steering_mode: Option<QueueMode>,
    pub follow_up_mode: Option<QueueMode>,
}

/// Turn state snapshot
struct TurnState {
    messages: Vec<AgentMessage>,
    resources: HarnessResources,
    session_id: String,
    system_prompt: String,
    model: Model,
    thinking_level: ThinkingLevel,
    active_tools: Vec<AgentTool>,
}

impl AgentHarness {
    /// Create a new agent harness
    pub fn new(options: AgentHarnessOptions) -> Self {
        let tools_map: HashMap<String, AgentTool> = options
            .tools
            .into_iter()
            .map(|t| (t.name.clone(), t))
            .collect();

        let active_names = options
            .active_tool_names
            .unwrap_or_else(|| tools_map.keys().cloned().collect());

        let (idle_tx, idle_rx) = flume::unbounded();

        Self {
            env: options.env,
            session: Arc::new(options.session),
            phase: Arc::new(RwLock::new(AgentHarnessPhase::Idle)),
            idle_tx,
            idle_rx,
            run_abort: Arc::new(RwLock::new(None)),
            model: Arc::new(RwLock::new(options.model)),
            thinking_level: Arc::new(RwLock::new(
                options.thinking_level.unwrap_or(ThinkingLevel::Off),
            )),
            tools: Arc::new(RwLock::new(tools_map)),
            active_tool_names: Arc::new(RwLock::new(active_names)),
            resources: Arc::new(RwLock::new(options.resources.unwrap_or_default())),
            stream_options: Arc::new(RwLock::new(options.stream_options.unwrap_or_default())),
            system_prompt: Arc::new(RwLock::new(options.system_prompt)),
            steer_queue: Arc::new(RwLock::new(Vec::new())),
            follow_up_queue: Arc::new(RwLock::new(Vec::new())),
            next_turn_queue: Arc::new(RwLock::new(Vec::new())),
            steering_mode: Arc::new(RwLock::new(
                options.steering_mode.unwrap_or(QueueMode::OneAtATime),
            )),
            follow_up_mode: Arc::new(RwLock::new(
                options.follow_up_mode.unwrap_or(QueueMode::OneAtATime),
            )),
            pending_writes: Arc::new(RwLock::new(Vec::new())),
            subscribers: Arc::new(RwLock::new(Vec::new())),
            hooks: Arc::new(RwLock::new(HashMap::new())),
            next_subscriber_id: Arc::new(Mutex::new(0)),
            next_hook_id: Arc::new(Mutex::new(0)),
            get_api_key_and_headers: options.get_api_key_and_headers,
        }
    }

    // ── Public API ──────────────────────────────────────────────────

    fn set_phase(&self, phase: AgentHarnessPhase) {
        tracing::debug!(target: "flown::harness", phase = ?phase, "set_phase");
        *self.phase.write() = phase.clone();
        if phase == AgentHarnessPhase::Idle {
            tracing::debug!(target: "flown::harness", "set_phase sending idle signal");
            let _ = self.idle_tx.send(());
        }
    }

    fn is_idle(&self) -> bool {
        *self.phase.read() == AgentHarnessPhase::Idle
    }

    /// Get current phase
    pub async fn phase(&self) -> AgentHarnessPhase {
        self.phase.read().clone()
    }

    /// Get execution environment
    pub fn env(&self) -> &dyn ExecutionEnv {
        self.env.as_ref()
    }

    /// Get current model
    pub async fn model(&self) -> Model {
        self.model.read().clone()
    }

    /// Pi-mono-aligned alias for `model()`.
    pub async fn get_model(&self) -> Model {
        self.model().await
    }

    /// Get current thinking level
    pub async fn thinking_level(&self) -> ThinkingLevel {
        self.thinking_level.read().clone()
    }

    /// Pi-mono-aligned alias for `thinking_level()`.
    pub async fn get_thinking_level(&self) -> ThinkingLevel {
        self.thinking_level().await
    }

    /// Get current steering mode
    pub async fn steering_mode(&self) -> QueueMode {
        self.steering_mode.read().clone()
    }

    /// Pi-mono-aligned alias for `steering_mode()`.
    pub async fn get_steering_mode(&self) -> QueueMode {
        self.steering_mode().await
    }

    /// Set steering mode
    pub async fn set_steering_mode(&self, mode: QueueMode) {
        *self.steering_mode.write() = mode;
    }

    /// Get current follow-up mode
    pub async fn follow_up_mode(&self) -> QueueMode {
        self.follow_up_mode.read().clone()
    }

    /// Pi-mono-aligned alias for `follow_up_mode()`.
    pub async fn get_follow_up_mode(&self) -> QueueMode {
        self.follow_up_mode().await
    }

    /// Count pending user messages queued via steer/follow-up.
    pub async fn pending_message_count(&self) -> usize {
        self.steer_queue.read().len() + self.follow_up_queue.read().len()
    }

    /// Set follow-up mode
    pub async fn set_follow_up_mode(&self, mode: QueueMode) {
        *self.follow_up_mode.write() = mode;
    }

    /// Get current resources
    pub async fn resources(&self) -> HarnessResources {
        self.resources.read().clone()
    }

    /// Pi-mono-aligned alias for `resources()`.
    pub async fn get_resources(&self) -> HarnessResources {
        self.resources().await
    }

    /// Get current stream options
    pub async fn stream_options(&self) -> HarnessStreamOptions {
        self.stream_options.read().clone()
    }

    /// Pi-mono-aligned alias for `stream_options()`.
    pub async fn get_stream_options(&self) -> HarnessStreamOptions {
        self.stream_options().await
    }

    /// Get the resolved system prompt for the current harness state.
    pub async fn system_prompt(&self) -> String {
        self.create_turn_state().await.system_prompt
    }

    /// Pi-mono-aligned alias for `system_prompt()`.
    pub async fn get_system_prompt(&self) -> String {
        self.system_prompt().await
    }

    /// Wait for the current run to complete
    pub async fn wait_for_idle(&self) {
        tracing::debug!(target: "flown::harness", "wait_for_idle start");
        while !self.is_idle() {
            tracing::debug!(target: "flown::harness", "wait_for_idle awaiting idle signal");
            let _ = self.idle_rx.recv_async().await;
            tracing::debug!(target: "flown::harness", "wait_for_idle received idle signal");
        }
        tracing::debug!(target: "flown::harness", "wait_for_idle end");
    }

    /// Set model
    pub async fn set_model(&self, model: Model) {
        let previous = {
            let mut current = self.model.write();
            let previous = current.clone();
            *current = model.clone();
            previous
        };

        if self.is_idle() {
            let _ = self
                .session
                .append_model_change(&model.provider.to_string(), &model.id)
                .await;
        } else {
            self.pending_writes
                .write()
                .push(PendingSessionWrite::ModelChange {
                    provider: model.provider.to_string(),
                    model_id: model.id.clone(),
                });
        }

        self.emit(
            HarnessEvent::ModelUpdate {
                model,
                previous_model: Some(previous),
                source: ModelUpdateSource::Set,
            },
            None,
        )
        .await;
    }

    /// Set thinking level
    pub async fn set_thinking_level(&self, level: ThinkingLevel) {
        let previous = {
            let mut current = self.thinking_level.write();
            let previous = current.clone();
            *current = level.clone();
            previous
        };

        if self.is_idle() {
            let _ = self
                .session
                .append_thinking_level_change(&format!("{:?}", level))
                .await;
        } else {
            self.pending_writes
                .write()
                .push(PendingSessionWrite::ThinkingLevelChange {
                    level: format!("{:?}", level),
                });
        }

        self.emit(
            HarnessEvent::ThinkingLevelUpdate {
                level,
                previous_level: previous,
            },
            None,
        )
        .await;
    }

    /// Get all registered tools.
    /// Aligned with pi-mono `AgentHarness.getTools()`.
    pub fn tools(&self) -> Vec<AgentTool> {
        self.tools.read().values().cloned().collect()
    }

    /// Pi-mono-aligned alias for `tools()`.
    pub fn get_tools(&self) -> Vec<AgentTool> {
        self.tools()
    }

    /// Get currently active tool names.
    /// Aligned with pi-mono `AgentHarness.activeToolNames` (read).
    pub fn active_tool_names(&self) -> Vec<String> {
        self.active_tool_names.read().clone()
    }

    /// Get currently active tools.
    /// Aligned with pi-mono `AgentHarness.getActiveTools()`.
    pub fn active_tools(&self) -> Vec<AgentTool> {
        let names = self.active_tool_names.read();
        let tools = self.tools.read();
        names
            .iter()
            .filter_map(|name| tools.get(name).cloned())
            .collect()
    }

    /// Pi-mono-aligned alias for `active_tools()`.
    pub fn get_active_tools(&self) -> Vec<AgentTool> {
        self.active_tools()
    }

    /// Set tools with optional active names
    pub async fn set_tools(
        &self,
        tools: Vec<AgentTool>,
        active_names: Option<Vec<String>>,
    ) -> Result<(), HarnessError> {
        let previous_tool_names: Vec<String> = self.tools.read().keys().cloned().collect();
        let previous_active_tool_names = self.active_tool_names.read().clone();
        let mut tools_map = HashMap::new();
        for tool in tools {
            if tools_map.insert(tool.name.clone(), tool).is_some() {
                return Err(HarnessError::InvalidArgument(
                    "duplicate tool name".to_string(),
                ));
            }
        }

        let next_active_names =
            active_names.unwrap_or_else(|| self.active_tool_names.read().clone());
        validate_tool_names(&next_active_names, &tools_map)?;

        *self.tools.write() = tools_map;
        *self.active_tool_names.write() = next_active_names.clone();
        self.persist_active_tools_change(next_active_names).await;
        let tool_names: Vec<String> = self.tools.read().keys().cloned().collect();
        let active_tool_names = self.active_tool_names.read().clone();
        self.emit(
            HarnessEvent::ToolsUpdate {
                tool_names,
                previous_tool_names,
                active_tool_names,
                previous_active_tool_names,
                source: ToolUpdateSource::Set,
            },
            None,
        )
        .await;
        Ok(())
    }

    /// Set active tool names
    pub async fn set_active_tools(&self, names: Vec<String>) -> Result<(), HarnessError> {
        let previous_tool_names: Vec<String> = self.tools.read().keys().cloned().collect();
        let previous_active_tool_names = self.active_tool_names.read().clone();
        let tools_map = self.tools.read();
        validate_tool_names(&names, &tools_map)?;
        drop(tools_map);
        *self.active_tool_names.write() = names.clone();
        self.persist_active_tools_change(names).await;
        let tool_names: Vec<String> = self.tools.read().keys().cloned().collect();
        let active_tool_names = self.active_tool_names.read().clone();
        self.emit(
            HarnessEvent::ToolsUpdate {
                tool_names,
                previous_tool_names,
                active_tool_names,
                previous_active_tool_names,
                source: ToolUpdateSource::Set,
            },
            None,
        )
        .await;
        Ok(())
    }

    async fn persist_active_tools_change(&self, names: Vec<String>) {
        if self.is_idle() {
            self.session.append_active_tools_change(names).await;
        } else {
            self.pending_writes
                .write()
                .push(PendingSessionWrite::ActiveToolsChange {
                    active_tool_names: names,
                });
        }
    }

    /// Set resources
    pub async fn set_resources(&self, resources: HarnessResources) {
        let mut current = self.resources.write();
        let previous = current.clone();
        *current = resources.clone();

        self.emit(
            HarnessEvent::ResourcesUpdate {
                resources,
                previous_resources: previous,
            },
            None,
        )
        .await;
    }

    /// Set stream options
    pub async fn set_stream_options(&self, options: HarnessStreamOptions) {
        *self.stream_options.write() = options;
    }

    /// Get session reference
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Prompt the agent with text
    pub async fn prompt(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
    ) -> Result<AssistantMessage, HarnessError> {
        let phase = self.phase.read().clone();
        tracing::info!(
            target: "flown::harness",
            phase = ?phase,
            text_len = text.len(),
            has_images = images.as_ref().map(|images| !images.is_empty()).unwrap_or(false),
            "harness prompt requested"
        );
        if phase != AgentHarnessPhase::Idle {
            tracing::warn!(
                target: "flown::harness",
                phase = ?phase,
                "harness prompt rejected: busy"
            );
            return Err(HarnessError::Busy(phase));
        }

        tracing::debug!(target: "flown::harness", text_len = text.len(), "prompt start");
        self.set_phase(AgentHarnessPhase::Turn);
        tracing::info!(target: "flown::harness", "harness prompt phase set to turn");

        let result = self.execute_turn(text, images).await;
        match &result {
            Ok(message) => {
                tracing::info!(
                    target: "flown::harness",
                    stop_reason = ?message.stop_reason,
                    "harness prompt execute_turn returned"
                );
            }
            Err(error) => {
                tracing::warn!(
                    target: "flown::harness",
                    error = ?error,
                    "harness prompt execute_turn returned error"
                );
            }
        }
        self.set_phase(AgentHarnessPhase::Idle);
        tracing::debug!(target: "flown::harness", "harness prompt phase set to idle");
        tracing::debug!(target: "flown::harness", "prompt end");

        result
    }

    /// Invoke a skill
    pub async fn skill(
        &self,
        name: &str,
        additional_instructions: Option<&str>,
    ) -> Result<AssistantMessage, HarnessError> {
        self.assert_idle()?;

        let invocation = {
            let resources = self.resources.read();
            let skill = resources
                .skills
                .iter()
                .find(|s| s.name == name)
                .ok_or_else(|| HarnessError::InvalidArgument(format!("Unknown skill: {}", name)))?;
            format_skill_invocation(skill, additional_instructions)
        };

        self.set_phase(AgentHarnessPhase::Turn);
        let result = self.execute_turn(&invocation, None).await;
        self.set_phase(AgentHarnessPhase::Idle);

        result
    }

    /// Prompt from a template
    pub async fn prompt_from_template(
        &self,
        name: &str,
        args: &[&str],
    ) -> Result<AssistantMessage, HarnessError> {
        self.assert_idle()?;

        let invocation = {
            let resources = self.resources.read();
            let template = resources
                .prompt_templates
                .iter()
                .find(|t| t.name == name)
                .ok_or_else(|| {
                    HarnessError::InvalidArgument(format!("Unknown prompt template: {}", name))
                })?;
            format_prompt_template_invocation(template, args)
        };

        self.set_phase(AgentHarnessPhase::Turn);
        let result = self.execute_turn(&invocation, None).await;
        self.set_phase(AgentHarnessPhase::Idle);

        result
    }

    /// Steer the agent with a message (injected between turns)
    pub async fn steer(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
    ) -> Result<(), HarnessError> {
        if self.is_idle() {
            return Err(HarnessError::InvalidState(
                "cannot steer when idle".to_string(),
            ));
        }

        let message = create_user_message(text, images);
        self.steer_queue.write().push(message);
        self.emit_queue_update().await;
        Ok(())
    }

    /// Queue a follow-up message (injected when agent would stop)
    pub async fn follow_up(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
    ) -> Result<(), HarnessError> {
        if self.is_idle() {
            return Err(HarnessError::InvalidState(
                "cannot follow_up when idle".to_string(),
            ));
        }

        let message = create_user_message(text, images);
        self.follow_up_queue.write().push(message);
        self.emit_queue_update().await;
        Ok(())
    }

    /// Queue a message for the next turn
    pub async fn next_turn(&self, text: &str, images: Option<Vec<ImageContent>>) {
        let message = create_user_message(text, images);
        self.next_turn_queue.write().push(message);
        self.emit_queue_update().await;
    }

    /// Abort the current run
    pub async fn abort(&self) -> Result<AbortResult, HarnessError> {
        let cleared_steer = {
            let mut queue = self.steer_queue.write();
            std::mem::take(&mut *queue)
        };
        let cleared_follow_up = {
            let mut queue = self.follow_up_queue.write();
            std::mem::take(&mut *queue)
        };

        if let Some(abort) = self.run_abort.write().as_ref() {
            abort.cancel();
        }

        self.emit_queue_update().await;

        // Wait for the run to actually complete
        self.wait_for_idle().await;

        self.emit(
            HarnessEvent::Abort {
                cleared_steer: cleared_steer.clone(),
                cleared_follow_up: cleared_follow_up.clone(),
            },
            None,
        )
        .await;

        Ok(AbortResult {
            cleared_steer,
            cleared_follow_up,
        })
    }

    /// Append a message directly to the session
    pub async fn append_message(&self, message: AgentMessage) {
        if self.is_idle() {
            self.session.append_message(message).await;
        } else {
            self.pending_writes
                .write()
                .push(PendingSessionWrite::Message(message));
        }
    }

    pub async fn append_custom_message_entry(
        &self,
        custom_type: &str,
        content: &str,
        display: Option<&str>,
        details: Option<&serde_json::Value>,
    ) {
        if self.is_idle() {
            self.session
                .append_custom_message_entry(custom_type, content, display, details)
                .await;
        } else {
            self.pending_writes
                .write()
                .push(PendingSessionWrite::CustomMessage {
                    custom_type: custom_type.to_string(),
                    content: content.to_string(),
                    display: display.map(ToOwned::to_owned),
                    details: details.cloned(),
                });
        }
    }

    /// Compact the conversation context
    pub async fn compact(
        &self,
        custom_instructions: Option<&str>,
    ) -> Result<CompactionResult, HarnessError> {
        if !self.is_idle() {
            let _ = self.abort().await?;
        }
        self.set_phase(AgentHarnessPhase::Compaction);
        let abort_signal = AbortSignal::new();
        *self.run_abort.write() = Some(abort_signal.clone());

        let result = self
            .execute_compaction(custom_instructions, Some(abort_signal))
            .await;
        self.run_abort.write().take();
        self.set_phase(AgentHarnessPhase::Idle);

        result
    }

    async fn execute_compaction(
        &self,
        custom_instructions: Option<&str>,
        signal: Option<AbortSignal>,
    ) -> Result<CompactionResult, HarnessError> {
        let model = self.model.read().clone();
        let thinking_level = self.thinking_level.read().clone();

        // Match pi-mono ordering: auth must be available before any compaction work starts.
        let (api_key, headers) = self
            .get_api_key_and_headers
            .as_ref()
            .and_then(|f| f(&model))
            .ok_or_else(|| HarnessError::Auth("No API key available for compaction".to_string()))?;

        // Get branch entries
        let entries = self.session.get_branch(None).await;

        // Prepare compaction
        let settings = super::compaction::compaction::CompactionSettings::default();
        let preparation = super::compaction::compaction::prepare_compaction(&entries, &settings)
            .map_err(HarnessError::Compaction)?;

        let preparation = match preparation {
            Some(preparation) => preparation,
            None => {
                return Err(HarnessError::Compaction(CompactionError::new(
                    CompactionErrorCode::Unknown,
                    "No compaction needed or possible",
                )));
            }
        };

        // Emit before_compact hook
        let hook_event = HarnessEvent::SessionBeforeCompact {
            preparation: preparation.clone(),
            branch_entries: entries.clone(),
            custom_instructions: custom_instructions.map(|s| s.to_string()),
            signal: signal.clone().unwrap_or_else(AbortSignal::new),
        };
        if let Some(result) = self.emit_hook("session_before_compact", &hook_event).await {
            if let Ok(parsed) = serde_json::from_value::<SessionBeforeCompactResult>(result) {
                if parsed.cancel == Some(true) {
                    return Err(HarnessError::Compaction(CompactionError::new(
                        CompactionErrorCode::Aborted,
                        "Compaction cancelled by hook",
                    )));
                }
                if let Some(compaction) = parsed.compaction {
                    let result: CompactionResult = compaction.into();
                    self.finish_compaction(&result, Some(true)).await;
                    return Ok(result);
                }
            }
        }

        // Generate summary using LLM
        let summary = super::compaction::compaction::compact_with_llm(
            &preparation,
            &model,
            &api_key,
            headers.as_ref(),
            custom_instructions,
            preparation.previous_summary.as_deref(),
            Some(&thinking_level),
            signal,
        )
        .await
        .map_err(HarnessError::Compaction)?;

        self.finish_compaction(&summary, None).await;

        Ok(summary)
    }

    async fn finish_compaction(&self, result: &CompactionResult, from_hook: Option<bool>) {
        let entry_id = self
            .session
            .append_compaction(
                &result.summary,
                &result.first_kept_entry_id,
                result.tokens_before,
                result.details.as_ref(),
                from_hook,
            )
            .await;
        let _session_context = self.session.build_context().await;
        let compaction_entry = self.session.get_entry(&entry_id).await;

        self.emit(
            HarnessEvent::SessionCompact {
                compaction_entry,
                from_hook: from_hook == Some(true),
            },
            None,
        )
        .await;
    }

    /// Navigate to a different point in the session tree
    pub async fn navigate_tree(
        &self,
        target_id: &str,
        options: NavigateTreeOptions,
    ) -> Result<NavigateTreeResult, HarnessError> {
        self.assert_idle()?;
        self.set_phase(AgentHarnessPhase::BranchSummary);
        let abort_signal = AbortSignal::new();
        *self.run_abort.write() = Some(abort_signal.clone());

        let result = self
            .execute_navigate_tree(target_id, options, abort_signal)
            .await;
        self.run_abort.write().take();
        self.set_phase(AgentHarnessPhase::Idle);

        result
    }

    async fn execute_navigate_tree(
        &self,
        target_id: &str,
        options: NavigateTreeOptions,
        navigation_signal: AbortSignal,
    ) -> Result<NavigateTreeResult, HarnessError> {
        let old_leaf_id = self.session.get_leaf_id().await;

        // If already at target, return
        if old_leaf_id.as_deref() == Some(target_id) {
            return Ok(NavigateTreeResult {
                cancelled: false,
                editor_text: None,
                summary_entry: None,
            });
        }

        let old_leaf = old_leaf_id.clone().unwrap_or_default();
        let all_entries = self.session.get_entries().await;
        let entries = super::compaction::branch_summary::collect_entries_for_branch_summary(
            &all_entries,
            &old_leaf,
            target_id,
        );

        // Emit before_tree hook
        let hook_event = HarnessEvent::SessionBeforeTree {
            preparation: TreeNavigationPreparation {
                target_id: target_id.to_string(),
                old_leaf_id: old_leaf_id.clone(),
                common_ancestor_id: None,
                entries_to_summarize: entries.clone(),
                user_wants_summary: options.summarize,
                custom_instructions: options.custom_instructions.clone(),
                replace_instructions: options.replace_instructions,
                label: options.label.clone(),
            },
            signal: navigation_signal.clone(),
        };
        let mut hook_summary: Option<(String, Option<serde_json::Value>, Option<bool>)> = None;
        let mut summary_instructions = options.custom_instructions.clone();
        let mut replace_instructions = options.replace_instructions.unwrap_or(false);
        let mut summary_label = options.label.clone();
        if let Some(result) = self.emit_hook("session_before_tree", &hook_event).await {
            if let Ok(parsed) = serde_json::from_value::<SessionBeforeTreeResult>(result) {
                if parsed.cancel == Some(true) {
                    return Ok(NavigateTreeResult {
                        cancelled: true,
                        editor_text: None,
                        summary_entry: None,
                    });
                }
                if let Some(custom_instructions) = parsed.custom_instructions {
                    summary_instructions = Some(custom_instructions);
                }
                if let Some(replace) = parsed.replace_instructions {
                    replace_instructions = replace;
                }
                if let Some(label) = parsed.label {
                    summary_label = Some(label);
                }
                if let Some(summary) = parsed.summary {
                    hook_summary = Some((summary.summary, summary.details, Some(true)));
                }
            }
        }

        // Generate summary if requested
        let from_hook_summary = hook_summary.is_some();
        let summary = if from_hook_summary {
            hook_summary.clone()
        } else if options.summarize && !entries.is_empty() {
            let (api_key, headers) = self
                .get_api_key_and_headers
                .as_ref()
                .and_then(|f| f(&self.model.read()))
                .ok_or_else(|| {
                    HarnessError::BranchSummary(BranchSummaryError::new(
                        BranchSummaryErrorCode::SummarizationFailed,
                        "No auth available for branch summary",
                    ))
                })?;
            let result = super::compaction::branch_summary::generate_branch_summary_with_llm(
                &entries,
                &super::compaction::branch_summary::GenerateBranchSummaryOptions {
                    model: self.model.read().clone(),
                    api_key,
                    headers,
                    signal: Some(navigation_signal.clone()),
                    custom_instructions: summary_instructions.clone(),
                    replace_instructions,
                    reserve_tokens: 16384,
                },
            )
            .await
            .map_err(HarnessError::BranchSummary)?;
            Some((
                result.summary,
                Some(serde_json::json!({
                    "readFiles": result.read_files,
                    "modifiedFiles": result.modified_files,
                    "label": summary_label,
                })),
                Some(false),
            ))
        } else {
            None
        };

        // Determine new leaf ID
        // If target is a user or custom_message, navigate to its parent so user can re-send
        let entry = self.session.get_entry(target_id).await;
        let new_leaf_id = if let Some(entry) = &entry {
            match entry {
                super::session::SessionTreeEntry::Message {
                    message: super::session::SessionMessage(AgentMessage::User(_)),
                    ..
                } => entry.parent_id().unwrap_or(target_id).to_string(),
                _ => target_id.to_string(),
            }
        } else {
            target_id.to_string()
        };

        // Move to the new position
        let summary_entry_id = self
            .session
            .move_to(
                Some(&new_leaf_id),
                summary.as_ref().map(|(summary, _, _)| summary.clone()),
                summary.as_ref().and_then(|(_, details, _)| details.clone()),
                summary.as_ref().and_then(|(_, _, from_hook)| *from_hook),
            )
            .await
            .map_err(|err| HarnessError::Session(err.to_string()))?;

        // Fetch summary entry if created
        let summary_entry = if let Some(ref sid) = summary_entry_id {
            self.session.get_entry(sid).await
        } else {
            None
        };

        self.emit(
            HarnessEvent::SessionTree {
                new_leaf_id: Some(new_leaf_id.clone()),
                old_leaf_id: old_leaf_id.clone(),
                summary_entry: summary_entry.clone(),
                from_hook: from_hook_summary,
            },
            None,
        )
        .await;

        // Get editor text from the target entry if it's a user message
        let editor_text = if let Some(entry) = &entry {
            match entry {
                super::session::SessionTreeEntry::Message {
                    message: super::session::SessionMessage(AgentMessage::User(msg)),
                    ..
                } => match &msg.content {
                    MessageContent::Text(t) => Some(t.clone()),
                    _ => None,
                },
                _ => None,
            }
        } else {
            None
        };

        Ok(NavigateTreeResult {
            cancelled: false,
            editor_text,
            summary_entry,
        })
    }

    // ── Event System ────────────────────────────────────────────────

    /// Subscribe to all events, returns an unsubscribe function
    pub fn subscribe(
        &self,
        handler: impl Fn(HarnessEvent, Option<AbortSignal>) -> BoxFuture<'static, ()>
        + Send
        + Sync
        + 'static,
    ) -> Box<dyn Fn() + Send + Sync> {
        let mut subscribers = self.subscribers.write();
        let mut next_id = self.next_subscriber_id.lock();
        let id = *next_id;
        *next_id += 1;
        subscribers.push(SubscriberEntry {
            id,
            handler: Arc::new(handler),
        });
        let subscribers_ref = self.subscribers.clone();
        Box::new(move || {
            let mut subscribers = subscribers_ref.write();
            subscribers.retain(|entry| entry.id != id);
        })
    }

    /// Register a hook for a specific event type, returns an unsubscribe function
    pub fn on(
        &self,
        event_type: &str,
        handler: impl Fn(
            HarnessEvent,
        ) -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send>>
        + Send
        + Sync
        + 'static,
    ) -> impl Fn() {
        let mut hooks = self.hooks.write();
        let entry = hooks.entry(event_type.to_string()).or_default();
        let mut next_id = self.next_hook_id.lock();
        let id = *next_id;
        *next_id += 1;
        entry.push(HookEntry {
            id,
            handler: Arc::new(handler),
        });
        let hooks_ref = self.hooks.clone();
        let event_type_owned = event_type.to_string();
        move || {
            let mut hooks = hooks_ref.write();
            if let Some(handlers) = hooks.get_mut(&event_type_owned) {
                handlers.retain(|entry| entry.id != id);
            }
        }
    }

    async fn emit(&self, event: HarnessEvent, signal: Option<AbortSignal>) {
        let handlers: Vec<EventHandler> = self
            .subscribers
            .read()
            .iter()
            .map(|entry| Arc::clone(&entry.handler))
            .collect();
        tracing::debug!(
            target: "flown::harness",
            event = ?event,
            subscribers = handlers.len(),
            "emit start"
        );
        for (index, handler) in handlers.into_iter().enumerate() {
            tracing::debug!(target: "flown::harness", index, "emit calling subscriber");
            handler(event.clone(), signal.clone()).await;
            tracing::debug!(target: "flown::harness", index, "emit subscriber completed");
        }
        tracing::debug!(target: "flown::harness", event = ?event, "emit end");
    }

    async fn emit_any(&self, event: HarnessEvent, signal: Option<AbortSignal>) {
        self.emit(event, signal).await;
    }

    async fn emit_hook(&self, event_type: &str, event: &HarnessEvent) -> Option<serde_json::Value> {
        let handlers: Vec<HookHandler> = {
            let hooks = self.hooks.read();
            match hooks.get(event_type) {
                Some(entries) => entries.iter().map(|e| e.handler.clone()).collect(),
                None => return None,
            }
        };
        let mut result = None;
        for handler in handlers {
            if let Some(r) = handler(event.clone()).await {
                result = Some(r);
            }
        }
        result
    }

    async fn emit_queue_update(&self) {
        let steer = self.steer_queue.read().clone();
        let follow_up = self.follow_up_queue.read().clone();
        let next_turn = self.next_turn_queue.read().clone();
        let signal = self.run_abort.read().clone();
        self.emit(
            HarnessEvent::QueueUpdate {
                steer,
                follow_up,
                next_turn,
            },
            signal,
        )
        .await;
    }

    // ── Internal ────────────────────────────────────────────────────

    fn assert_idle(&self) -> Result<(), HarnessError> {
        let phase = self.phase.read();
        if *phase != AgentHarnessPhase::Idle {
            return Err(HarnessError::Busy(phase.clone()));
        }
        Ok(())
    }

    async fn create_turn_state(&self) -> TurnState {
        let session_context = self.session.build_context().await;
        let resources = self.resources.read().clone();
        let metadata = self.session.get_metadata().await;
        let model = self.model.read().clone();
        let thinking_level = self.thinking_level.read().clone();
        let all_tools: Vec<AgentTool> = self.tools.read().values().cloned().collect();
        let active_names = self.active_tool_names.read().clone();
        let active_tools: Vec<AgentTool> = all_tools
            .iter()
            .filter(|t| active_names.contains(&t.name))
            .cloned()
            .collect();

        // Resolve system prompt
        let system_prompt_config = self.system_prompt.read().clone();
        let system_prompt = match system_prompt_config {
            SystemPromptConfig::Static(s) => s,
            SystemPromptConfig::Dynamic(f) => {
                let ctx = SystemPromptContext {
                    env: self.env.clone(),
                    session: self.session.clone(),
                    model: model.clone(),
                    thinking_level: thinking_level.clone(),
                    active_tools: active_tools.clone(),
                    resources: resources.clone(),
                };
                f(&ctx)
            }
            SystemPromptConfig::AsyncDynamic(f) => {
                let ctx = SystemPromptContext {
                    env: self.env.clone(),
                    session: self.session.clone(),
                    model: model.clone(),
                    thinking_level: thinking_level.clone(),
                    active_tools: active_tools.clone(),
                    resources: resources.clone(),
                };
                f(&ctx).await
            }
        };

        // Inject skills into system prompt
        let skills_text = format_skills_for_system_prompt(&resources.skills);
        let full_system_prompt = if skills_text.is_empty() {
            system_prompt
        } else if system_prompt.contains("<available_skills>") {
            system_prompt
        } else {
            format!("{}\n\n{}", system_prompt, skills_text)
        };

        TurnState {
            messages: session_context.messages,
            resources,
            session_id: metadata.id.clone(),
            system_prompt: full_system_prompt,
            model,
            thinking_level,
            active_tools,
        }
    }

    async fn flush_pending_writes(&self) -> Result<(), HarnessError> {
        let writes: Vec<PendingSessionWrite> = {
            let mut pending = self.pending_writes.write();
            std::mem::take(&mut *pending)
        };

        for write in writes {
            match write {
                PendingSessionWrite::Message(msg) => {
                    self.session.append_message(msg).await;
                }
                PendingSessionWrite::ModelChange { provider, model_id } => {
                    self.session.append_model_change(&provider, &model_id).await;
                }
                PendingSessionWrite::ThinkingLevelChange { level } => {
                    self.session.append_thinking_level_change(&level).await;
                }
                PendingSessionWrite::ActiveToolsChange { active_tool_names } => {
                    self.session
                        .append_active_tools_change(active_tool_names)
                        .await;
                }
                PendingSessionWrite::Label { target_id, label } => {
                    self.session
                        .append_label(&target_id, label.as_deref())
                        .await;
                }
                PendingSessionWrite::SessionInfo { name } => {
                    if let Some(n) = name {
                        self.session.append_session_name(&n).await;
                    }
                }
                PendingSessionWrite::Custom { custom_type, data } => {
                    self.session.append_custom_entry(&custom_type, &data).await;
                }
                PendingSessionWrite::CustomMessage {
                    custom_type,
                    content,
                    display,
                    details,
                } => {
                    self.session
                        .append_custom_message_entry(
                            &custom_type,
                            &content,
                            display.as_deref(),
                            details.as_ref(),
                        )
                        .await;
                }
                PendingSessionWrite::Leaf { target_id } => {
                    self.session
                        .set_leaf_id(&target_id)
                        .await
                        .map_err(|err| HarnessError::Session(err.to_string()))?;
                }
            }
        }
        Ok(())
    }

    async fn execute_turn(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
    ) -> Result<AssistantMessage, HarnessError> {
        tracing::debug!(target: "flown::harness", text_len = text.len(), "execute_turn start");
        let mut turn_state = self.create_turn_state().await;
        let abort_signal = AbortSignal::new();
        *self.run_abort.write() = Some(abort_signal.clone());

        // Drain next-turn queue
        let next_turn_msgs: Vec<AgentMessage> = {
            let mut queue = self.next_turn_queue.write();
            std::mem::take(&mut *queue)
        };

        // Build initial messages
        let mut initial_messages = next_turn_msgs;
        let user_message = create_user_message(text, images.clone());
        initial_messages.push(user_message);

        // Emit before_agent_start hook
        let hook_event = HarnessEvent::BeforeAgentStart {
            prompt: text.to_string(),
            images,
            system_prompt: turn_state.system_prompt.clone(),
            resources: turn_state.resources.clone(),
        };
        if let Some(result) = self.emit_hook("before_agent_start", &hook_event).await {
            if let Ok(parsed) = serde_json::from_value::<BeforeAgentStartResult>(result) {
                if let Some(injected) = parsed.messages {
                    initial_messages.extend(injected);
                }
                if let Some(sp) = parsed.system_prompt {
                    turn_state.system_prompt = sp;
                }
            }
        }

        // Create agent context
        let context = AgentContext {
            system_prompt: turn_state.system_prompt.clone(),
            messages: turn_state.messages.clone(),
            tools: Some(turn_state.active_tools.clone()),
        };

        // Create loop config
        let harness = self.clone_refs();

        let get_steering = {
            let h = harness.clone();
            Arc::new(move || {
                let h = h.clone();
                Box::pin(async move { h.drain_steer_queue() })
                    as Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>>
            })
        };

        let get_follow_up = {
            let h = harness.clone();
            Arc::new(move || {
                let h = h.clone();
                Box::pin(async move { h.drain_follow_up_queue() })
                    as Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>>
            })
        };

        let prepare_next_turn = {
            let h = harness.clone();
            Arc::new(
                move |_ctx: PrepareNextTurnContext, _signal: Option<AbortSignal>| {
                    let h = h.clone();
                    Box::pin(async move {
                        h.flush_pending_writes().await.ok()?;
                        let new_state = h.create_turn_state().await;
                        Some(AgentLoopTurnUpdate {
                            context: Some(AgentContext {
                                system_prompt: new_state.system_prompt.clone(),
                                messages: new_state.messages.clone(),
                                tools: Some(new_state.active_tools.clone()),
                            }),
                            model: Some(new_state.model.clone()),
                            thinking_level: Some(new_state.thinking_level.clone()),
                        })
                    })
                        as Pin<Box<dyn Future<Output = Option<AgentLoopTurnUpdate>> + Send>>
                },
            )
        };

        let before_tool_call = {
            let h = harness.clone();
            Arc::new(
                move |ctx: BeforeToolCallContext, _signal: Option<AbortSignal>| {
                    let h = h.clone();
                    Box::pin(async move {
                        let event = HarnessEvent::ToolCall {
                            tool_call_id: ctx.tool_call.id.clone(),
                            tool_name: ctx.tool_call.name.clone(),
                            input: ctx.args.clone(),
                        };
                        if let Some(result) = h.emit_hook("tool_call", &event).await {
                            if let Ok(parsed) = serde_json::from_value::<ToolCallResult>(result) {
                                return Some(BeforeToolCallResult {
                                    block: parsed.block,
                                    reason: parsed.reason,
                                });
                            }
                        }
                        None
                    })
                        as Pin<Box<dyn Future<Output = Option<BeforeToolCallResult>> + Send>>
                },
            )
        };

        let after_tool_call = {
            let h = harness.clone();
            Arc::new(
                move |ctx: AfterToolCallContext, _signal: Option<AbortSignal>| {
                    let h = h.clone();
                    Box::pin(async move {
                        if !ctx.is_error && ctx.tool_call.name == "run_workflow" {
                            if let (Some(workflow_name), Some(result_path)) = (
                                ctx.result
                                    .details
                                    .get("workflow")
                                    .and_then(|value| value.as_str()),
                                ctx.result
                                    .details
                                    .get("resultPath")
                                    .and_then(|value| value.as_str()),
                            ) {
                                let details = serde_json::json!({
                                    "toolCallId": ctx.tool_call.id,
                                    "workflowName": workflow_name,
                                    "resultPath": result_path,
                                });
                                h.append_custom_message_entry(
                                    "workflow_result",
                                    &format!(
                                        "Workflow `{workflow_name}` completed. Continue from this result JSON path:\n{result_path}"
                                    ),
                                    Some("workflow_result"),
                                    Some(&details),
                                )
                                .await;
                                h.follow_up(
                                    &format!(
                                        "Workflow '{}' completed. Result saved to {}. Read the result file thoroughly. Do not merely summarize — instead, synthesize the findings and deliver a deep, critical analysis. Identify underlying patterns, non-obvious connections, implications, and actionable insights. Think about what the data reveals beyond the surface. Present your analysis in clear, human-readable prose — never dump raw JSON.",
                                        workflow_name,
                                        result_path,
                                    ),
                                    None,
                                )
                                .await;
                            }
                        }
                        let event = HarnessEvent::ToolResult {
                            tool_call_id: ctx.tool_call.id.clone(),
                            tool_name: ctx.tool_call.name.clone(),
                            input: ctx.args.clone(),
                            content: ctx.result.content.clone(),
                            details: ctx.result.details.clone(),
                            is_error: ctx.is_error,
                        };
                        if let Some(result) = h.emit_hook("tool_result", &event).await {
                            if let Ok(patch) = serde_json::from_value::<ToolResultPatch>(result) {
                                return Some(AfterToolCallResult {
                                    content: patch.content,
                                    details: patch.details,
                                    is_error: patch.is_error,
                                    terminate: patch.terminate,
                                });
                            }
                        }
                        None
                    })
                        as Pin<Box<dyn Future<Output = Option<AfterToolCallResult>> + Send>>
                },
            )
        };

        let reasoning = match turn_state.thinking_level {
            ThinkingLevel::Off => None,
            level => Some(level),
        };

        let get_api_key = {
            let api_key_fn = self.get_api_key_and_headers.clone();
            let model = turn_state.model.clone();
            Arc::new(move |provider: String| {
                let api_key_fn = api_key_fn.clone();
                let model = model.clone();
                Box::pin(async move {
                    if model.provider.to_string() != provider {
                        return None;
                    }
                    api_key_fn
                        .as_ref()
                        .and_then(|f| f(&model).map(|(key, _headers)| key))
                }) as Pin<Box<dyn Future<Output = Option<String>> + Send>>
            })
        };

        let transform_context = {
            let h = harness.clone();
            Some(Arc::new(
                move |msgs: Vec<AgentMessage>, _signal: Option<AbortSignal>| {
                    let h = h.clone();
                    Box::pin(async move {
                        let event = HarnessEvent::Context {
                            messages: msgs.clone(),
                        };
                        if let Some(result) = h.emit_hook("context", &event).await {
                            if let Ok(parsed) = serde_json::from_value::<ContextResult>(result) {
                                if let Some(transformed) = parsed.messages {
                                    return transformed;
                                }
                            }
                        }
                        msgs
                    })
                        as Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>>
                },
            )
                as Arc<
                    dyn Fn(
                            Vec<AgentMessage>,
                            Option<AbortSignal>,
                        )
                            -> Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>>
                        + Send
                        + Sync,
                >)
        };

        // Create stream function with before_provider_request and after_provider_response hooks.
        let stream_fn = {
            let h = harness.clone();
            let session_id = turn_state.session_id.clone();
            Arc::new(
                move |model: Model, context: Context, options: Option<SimpleStreamOptions>| {
                    let h = h.clone();
                    let session_id = session_id.clone();
                    flown_ai::AssistantMessageEventStream::from_stream(Box::pin(
                        async_stream::stream! {
                            let mut options = options.unwrap_or_default();

                            // Get API key and headers
                            let api_key_fn = h.get_api_key_and_headers.clone();
                            let auth = api_key_fn.as_ref().and_then(|f| f(&model));
                            if let Some((api_key, _headers)) = &auth {
                                options.base.api_key = Some(api_key.clone());
                            }

                            // Emit before_provider_request hook
                            let mut snapshot_options = h.stream_options.read().clone();
                            snapshot_options.headers = merge_headers(
                                snapshot_options.headers,
                                auth.and_then(|(_api_key, headers)| headers),
                            );
                            let updated_options = h.emit_before_provider_request(&model, &session_id, &snapshot_options).await;

                            // Apply updated options
                            if let Some(headers) = updated_options.headers {
                                options.base.headers = Some(headers);
                            }
                            if let Some(transport) = updated_options.transport {
                                options.base.transport = Some(transport);
                            }
                            if let Some(timeout) = updated_options.timeout_ms {
                                options.base.timeout_ms = Some(timeout);
                            }
                            if let Some(retries) = updated_options.max_retries {
                                options.base.max_retries = Some(retries);
                            }
                            if let Some(delay) = updated_options.max_retry_delay_ms {
                                options.base.max_retry_delay_ms = Some(delay);
                            }
                            if let Some(retention) = updated_options.cache_retention {
                                options.base.cache_retention = Some(retention);
                            }
                            if let Some(metadata) = updated_options.metadata {
                                options.base.metadata = Some(metadata);
                            }

                            // Wire up on_payload callback (before_provider_payload hook)
                            let h_payload = h.clone();
                            let model_payload = model.clone();
                            options.base.on_payload = Some(Arc::new(move |payload| {
                                let h = h_payload.clone();
                                let model = model_payload.clone();
                                Box::pin(async move {
                                    Some(h.emit_before_provider_payload(&model, payload).await)
                                })
                            }));

                            let h_response = h.clone();
                            options.base.on_response = Some(Arc::new(move |response| {
                                let h = h_response.clone();
                                Box::pin(async move {
                                    h.emit(HarnessEvent::AfterProviderResponse {
                                        status: response.status,
                                        headers: response.headers,
                                    }, None)
                                    .await;
                                })
                            }));

                            let mut stream = match flown_ai::stream_simple(&model, &context, Some(&options)) {
                                Ok(s) => s,
                                Err(error) => {
                                    yield AssistantMessageEvent::Error {
                                        reason: StopReason::Error,
                                        error: AssistantMessage {
                                            role: "assistant".to_string(),
                                            content: vec![],
                                            api: model.api.clone(),
                                            provider: model.provider.clone(),
                                            model: model.id.clone(),
                                            response_model: None,
                                            response_id: None,
                                            usage: Usage::default(),
                                            stop_reason: StopReason::Error,
                                            error_message: Some(error.to_string()),
                                            diagnostics: None,
                                            timestamp: chrono::Utc::now(),
                                        },
                                    };
                                    return;
                                }
                            };

                            while let Some(event) = stream.next().await {
                                yield event;
                            }
                        },
                    ))
                },
            )
        };

        let config = AgentLoopConfig {
            model: turn_state.model.clone(),
            reasoning,
            session_id: Some(turn_state.session_id.clone()),
            thinking_budgets: None,
            transport: None,
            max_retry_delay_ms: None,
            on_payload: None,
            on_response: None,
            convert_to_llm: Arc::new(|msgs| {
                // Convert AgentMessage to LLM Message
                // Custom types are converted to user messages (matching pi-mono behavior)
                msgs.iter()
                    .map(|m| match m {
                        AgentMessage::User(u) => Message::User(u.clone()),
                        AgentMessage::Assistant(a) => Message::Assistant(a.clone()),
                        AgentMessage::ToolResult(t) => Message::ToolResult(t.clone()),
                        AgentMessage::Custom(c) => {
                            let text = match c.custom_type.as_str() {
                                "branch_summary" | "compaction_summary" => {
                                    format!("<summary>\n{}\n</summary>", c.content)
                                }
                                _ => c.content.clone(),
                            };
                            Message::User(UserMessage {
                                role: "user".to_string(),
                                content: MessageContent::Text(text),
                                timestamp: c.timestamp,
                            })
                        }
                    })
                    .collect()
            }),
            transform_context,
            get_api_key: Some(get_api_key),
            stream_fn: Some(stream_fn),
            should_stop_after_turn: None,
            prepare_next_turn: Some(prepare_next_turn),
            get_steering_messages: Some(get_steering),
            get_follow_up_messages: Some(get_follow_up),
            tool_execution: ToolExecutionMode::Parallel,
            before_tool_call: Some(before_tool_call),
            after_tool_call: Some(after_tool_call),
        };

        // Run the agent loop
        let mut stream = agent_loop(initial_messages, context, config, Some(abort_signal), None);
        let mut last_message = None;

        while let Some(event) = stream.next().await {
            tracing::debug!(target: "flown::harness", event = ?event, "execute_turn event");
            // Convert AgentEvent to HarnessEvent and forward to subscribers
            let harness_event = HarnessEvent::from(&event);

            // Forward event to subscribers
            self.emit_any(harness_event, None).await;

            // Handle specific events
            match &event {
                AgentEvent::MessageEnd {
                    message: AgentMessage::User(msg),
                } => {
                    self.session
                        .append_message(AgentMessage::User(msg.clone()))
                        .await;
                }
                AgentEvent::MessageEnd {
                    message: AgentMessage::Custom(msg),
                } => {
                    self.session
                        .append_message(AgentMessage::Custom(msg.clone()))
                        .await;
                }
                AgentEvent::MessageEnd {
                    message: AgentMessage::Assistant(msg),
                } => {
                    last_message = Some(msg.clone());
                    // Append to session
                    self.session
                        .append_message(AgentMessage::Assistant(msg.clone()))
                        .await;
                }
                AgentEvent::TurnEnd { tool_results, .. } => {
                    tracing::debug!(
                        target: "flown::harness",
                        tool_results_count = tool_results.len(),
                        "execute_turn handling TurnEnd"
                    );
                    // Append tool results to session
                    for result in tool_results {
                        self.session
                            .append_message(AgentMessage::ToolResult(result.clone()))
                            .await;
                    }

                    // Check for pending mutations before flush
                    let had_pending_mutations = !self.pending_writes.read().is_empty();

                    // Flush pending writes
                    self.flush_pending_writes().await?;

                    // Emit save point
                    self.emit(
                        HarnessEvent::SavePoint {
                            had_pending_mutations,
                        },
                        None,
                    )
                    .await;
                    tracing::debug!(target: "flown::harness", "execute_turn save point emitted");
                }
                AgentEvent::AgentEnd { .. } => {
                    tracing::debug!(target: "flown::harness", "execute_turn handling AgentEnd");
                    self.flush_pending_writes().await?;
                    tracing::debug!(target: "flown::harness", "execute_turn pending writes flushed after AgentEnd");
                    let next_turn_count = self.next_turn_queue.read().len();
                    self.emit(HarnessEvent::Settled { next_turn_count }, None)
                        .await;
                    tracing::debug!(target: "flown::harness", "execute_turn settled emitted");
                }
                _ => {}
            }
        }

        self.run_abort.write().take();
        tracing::debug!(
            target: "flown::harness",
            has_last_message = last_message.is_some(),
            "execute_turn stream ended"
        );
        last_message.ok_or_else(|| HarnessError::InvalidState("no assistant response".to_string()))
    }

    fn clone_refs(&self) -> HarnessRefs {
        HarnessRefs {
            env: self.env.clone(),
            session: self.session.clone(),
            model: self.model.clone(),
            thinking_level: self.thinking_level.clone(),
            tools: self.tools.clone(),
            active_tool_names: self.active_tool_names.clone(),
            resources: self.resources.clone(),
            stream_options: self.stream_options.clone(),
            system_prompt: self.system_prompt.clone(),
            steer_queue: self.steer_queue.clone(),
            follow_up_queue: self.follow_up_queue.clone(),
            steering_mode: self.steering_mode.clone(),
            follow_up_mode: self.follow_up_mode.clone(),
            pending_writes: self.pending_writes.clone(),
            subscribers: self.subscribers.clone(),
            hooks: self.hooks.clone(),
            get_api_key_and_headers: self.get_api_key_and_headers.clone(),
        }
    }
}

/// Cloned references for use in closures
#[derive(Clone)]
struct HarnessRefs {
    env: Arc<dyn ExecutionEnv>,
    session: Arc<Session>,
    model: Arc<RwLock<Model>>,
    thinking_level: Arc<RwLock<ThinkingLevel>>,
    tools: Arc<RwLock<HashMap<String, AgentTool>>>,
    active_tool_names: Arc<RwLock<Vec<String>>>,
    resources: Arc<RwLock<HarnessResources>>,
    stream_options: Arc<RwLock<HarnessStreamOptions>>,
    system_prompt: Arc<RwLock<SystemPromptConfig>>,
    steer_queue: Arc<RwLock<Vec<AgentMessage>>>,
    follow_up_queue: Arc<RwLock<Vec<AgentMessage>>>,
    steering_mode: Arc<RwLock<QueueMode>>,
    follow_up_mode: Arc<RwLock<QueueMode>>,
    pending_writes: Arc<RwLock<Vec<PendingSessionWrite>>>,
    subscribers: Arc<RwLock<Vec<SubscriberEntry>>>,
    hooks: Arc<RwLock<HashMap<String, Vec<HookEntry>>>>,
    get_api_key_and_headers: Option<GetApiKeyAndHeadersFn>,
}

impl HarnessRefs {
    async fn append_custom_message_entry(
        &self,
        custom_type: &str,
        content: &str,
        display: Option<&str>,
        details: Option<&serde_json::Value>,
    ) {
        self.pending_writes
            .write()
            .push(PendingSessionWrite::CustomMessage {
                custom_type: custom_type.to_string(),
                content: content.to_string(),
                display: display.map(ToOwned::to_owned),
                details: details.cloned(),
            });
    }

    async fn follow_up(&self, text: &str, images: Option<Vec<ImageContent>>) {
        let message = create_user_message(text, images);
        self.follow_up_queue.write().push(message);
    }

    fn drain_steer_queue(&self) -> Vec<AgentMessage> {
        let mode = self.steering_mode.read().clone();
        let mut queue = self.steer_queue.write();
        match mode {
            QueueMode::All => std::mem::take(&mut *queue),
            QueueMode::OneAtATime => {
                if queue.is_empty() {
                    Vec::new()
                } else {
                    vec![queue.remove(0)]
                }
            }
        }
    }

    fn drain_follow_up_queue(&self) -> Vec<AgentMessage> {
        let mode = self.follow_up_mode.read().clone();
        let mut queue = self.follow_up_queue.write();
        match mode {
            QueueMode::All => std::mem::take(&mut *queue),
            QueueMode::OneAtATime => {
                if queue.is_empty() {
                    Vec::new()
                } else {
                    vec![queue.remove(0)]
                }
            }
        }
    }

    async fn flush_pending_writes(&self) -> Result<(), HarnessError> {
        let writes: Vec<PendingSessionWrite> = {
            let mut pending = self.pending_writes.write();
            std::mem::take(&mut *pending)
        };

        for write in writes {
            match write {
                PendingSessionWrite::Message(msg) => {
                    self.session.append_message(msg).await;
                }
                PendingSessionWrite::ModelChange { provider, model_id } => {
                    self.session.append_model_change(&provider, &model_id).await;
                }
                PendingSessionWrite::ThinkingLevelChange { level } => {
                    self.session.append_thinking_level_change(&level).await;
                }
                PendingSessionWrite::ActiveToolsChange { active_tool_names } => {
                    self.session
                        .append_active_tools_change(active_tool_names)
                        .await;
                }
                PendingSessionWrite::Label { target_id, label } => {
                    self.session
                        .append_label(&target_id, label.as_deref())
                        .await;
                }
                PendingSessionWrite::SessionInfo { name } => {
                    if let Some(n) = name {
                        self.session.append_session_name(&n).await;
                    }
                }
                PendingSessionWrite::Custom { custom_type, data } => {
                    self.session.append_custom_entry(&custom_type, &data).await;
                }
                PendingSessionWrite::CustomMessage {
                    custom_type,
                    content,
                    display,
                    details,
                } => {
                    self.session
                        .append_custom_message_entry(
                            &custom_type,
                            &content,
                            display.as_deref(),
                            details.as_ref(),
                        )
                        .await;
                }
                PendingSessionWrite::Leaf { target_id } => {
                    self.session
                        .set_leaf_id(&target_id)
                        .await
                        .map_err(|err| HarnessError::Session(err.to_string()))?;
                }
            }
        }
        Ok(())
    }

    async fn create_turn_state(&self) -> TurnState {
        let session_context = self.session.build_context().await;
        let resources = self.resources.read().clone();
        let metadata = self.session.get_metadata().await;
        let model = self.model.read().clone();
        let thinking_level = self.thinking_level.read().clone();
        let all_tools: Vec<AgentTool> = self.tools.read().values().cloned().collect();
        let active_names = self.active_tool_names.read().clone();
        let active_tools: Vec<AgentTool> = all_tools
            .iter()
            .filter(|t| active_names.contains(&t.name))
            .cloned()
            .collect();

        let system_prompt_config = self.system_prompt.read().clone();
        let system_prompt = match system_prompt_config {
            SystemPromptConfig::Static(s) => s,
            SystemPromptConfig::Dynamic(f) => {
                let ctx = SystemPromptContext {
                    env: self.env.clone(),
                    session: self.session.clone(),
                    model: model.clone(),
                    thinking_level: thinking_level.clone(),
                    active_tools: active_tools.clone(),
                    resources: resources.clone(),
                };
                f(&ctx)
            }
            SystemPromptConfig::AsyncDynamic(f) => {
                let ctx = SystemPromptContext {
                    env: self.env.clone(),
                    session: self.session.clone(),
                    model: model.clone(),
                    thinking_level: thinking_level.clone(),
                    active_tools: active_tools.clone(),
                    resources: resources.clone(),
                };
                f(&ctx).await
            }
        };

        let skills_text = format_skills_for_system_prompt(&resources.skills);
        let full_system_prompt = if skills_text.is_empty() {
            system_prompt
        } else {
            format!("{}\n\n{}", system_prompt, skills_text)
        };

        TurnState {
            messages: session_context.messages,
            resources,
            session_id: metadata.id.clone(),
            system_prompt: full_system_prompt,
            model,
            thinking_level,
            active_tools,
        }
    }

    async fn emit_hook(&self, event_type: &str, event: &HarnessEvent) -> Option<serde_json::Value> {
        let handlers: Vec<HookHandler> = {
            let hooks = self.hooks.read();
            match hooks.get(event_type) {
                Some(entries) => entries.iter().map(|e| e.handler.clone()).collect(),
                None => return None,
            }
        };
        let mut result = None;
        for handler in handlers {
            if let Some(r) = handler(event.clone()).await {
                result = Some(r);
            }
        }
        result
    }

    async fn emit_before_provider_request(
        &self,
        model: &Model,
        session_id: &str,
        stream_options: &HarnessStreamOptions,
    ) -> HarnessStreamOptions {
        let handlers: Vec<HookHandler> = {
            let hooks = self.hooks.read();
            match hooks.get("before_provider_request") {
                Some(entries) => entries.iter().map(|entry| entry.handler.clone()).collect(),
                None => return stream_options.clone(),
            }
        };
        let mut current = stream_options.clone();
        for handler in handlers {
            let event = HarnessEvent::BeforeProviderRequest {
                model: model.clone(),
                session_id: session_id.to_string(),
                stream_options: current.clone(),
            };
            if let Some(result) = handler(event).await {
                if let Some(patch) = result
                    .get("streamOptions")
                    .or_else(|| result.get("stream_options"))
                {
                    current = apply_stream_options_patch(&current, patch);
                }
            }
        }
        current
    }

    async fn emit_before_provider_payload(
        &self,
        model: &Model,
        payload: serde_json::Value,
    ) -> serde_json::Value {
        let event = HarnessEvent::BeforeProviderPayload {
            model: model.clone(),
            payload: payload.clone(),
        };
        if let Some(result) = self.emit_hook("before_provider_payload", &event).await {
            if let Ok(parsed) = serde_json::from_value::<BeforeProviderPayloadResult>(result) {
                if let Some(new_payload) = parsed.payload {
                    return new_payload;
                }
            }
        }
        payload
    }

    async fn emit(&self, event: HarnessEvent, signal: Option<AbortSignal>) {
        let handlers: Vec<EventHandler> = self
            .subscribers
            .read()
            .iter()
            .map(|entry| Arc::clone(&entry.handler))
            .collect();
        for handler in handlers {
            handler(event.clone(), signal.clone()).await;
        }
    }
}

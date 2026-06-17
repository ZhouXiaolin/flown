use crate::agent_loop::{agent_loop, agent_loop_continue};
use crate::types::*;
use flown_ai::types::*;
use futures::stream::Stream;
use futures::StreamExt;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// Agent execution phase
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AgentPhase {
    Idle = 0,
    Turn = 1,
}

/// Agent options for construction
pub struct AgentOptions {
    pub initial_state: Option<AgentState>,
    pub convert_to_llm: Option<Arc<dyn Fn(Vec<AgentMessage>) -> Vec<Message> + Send + Sync>>,
    pub transform_context: Option<
        Arc<
            dyn Fn(
                    Vec<AgentMessage>,
                    Option<AbortSignal>,
                ) -> Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>>
                + Send
                + Sync,
        >,
    >,
    pub stream_fn: Option<StreamFn>,
    pub get_api_key: Option<
        Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>,
    >,
    pub on_payload: Option<OnPayloadFn>,
    pub on_response: Option<OnResponseFn>,
    pub before_tool_call: Option<
        Arc<
            dyn Fn(
                    BeforeToolCallContext,
                    Option<AbortSignal>,
                )
                    -> Pin<Box<dyn Future<Output = Option<BeforeToolCallResult>> + Send>>
                + Send
                + Sync,
        >,
    >,
    pub after_tool_call: Option<
        Arc<
            dyn Fn(
                    AfterToolCallContext,
                    Option<AbortSignal>,
                )
                    -> Pin<Box<dyn Future<Output = Option<AfterToolCallResult>> + Send>>
                + Send
                + Sync,
        >,
    >,
    pub prepare_next_turn: Option<
        Arc<
            dyn Fn(
                    Option<AbortSignal>,
                )
                    -> Pin<Box<dyn Future<Output = Option<AgentLoopTurnUpdate>> + Send>>
                + Send
                + Sync,
        >,
    >,
    pub steering_mode: Option<QueueMode>,
    pub follow_up_mode: Option<QueueMode>,
    pub session_id: Option<String>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub transport: Option<Transport>,
    pub max_retry_delay_ms: Option<u64>,
    pub tool_execution: Option<ToolExecutionMode>,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            initial_state: None,
            convert_to_llm: None,
            transform_context: None,
            stream_fn: None,
            get_api_key: None,
            on_payload: None,
            on_response: None,
            before_tool_call: None,
            after_tool_call: None,
            prepare_next_turn: None,
            steering_mode: None,
            follow_up_mode: None,
            session_id: None,
            thinking_budgets: None,
            transport: None,
            max_retry_delay_ms: None,
            tool_execution: None,
        }
    }
}

/// Pending message queue (steer / follow-up).
/// Written from the TUI thread, drained by the agent loop.
struct MessageQueue {
    messages: Vec<AgentMessage>,
    mode: QueueMode,
}

impl MessageQueue {
    fn new(mode: QueueMode) -> Self {
        Self {
            messages: Vec::new(),
            mode,
        }
    }

    fn drain(&mut self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::All => std::mem::take(&mut self.messages),
            QueueMode::OneAtATime => {
                if self.messages.is_empty() {
                    Vec::new()
                } else {
                    vec![self.messages.remove(0)]
                }
            }
        }
    }
}

/// Stateful agent.
///
/// Interior mutability summary:
/// - `state` (RwLock) — the only true shared-mutable lock; protects model,
///   thinking_level, system_prompt, and the message history.
/// - `tools` (RwLock) — set at startup, updated by MCP reconcile at runtime.
/// - `steering_queue` / `follow_up_queue` (RwLock) — brief lock; TUI writes,
///   agent loop drains.
/// - `phase` (AtomicU8) — lock-free Idle/Turn flag.
/// - `run_abort` (RwLock<Option<AbortSignal>>) — set per-turn by `run()`,
///   cancelled by `abort()`. RwLock because it's Option-wrapped; AbortSignal
///   itself is internally atomic.
pub struct Agent {
    state: Arc<RwLock<AgentState>>,
    tools: Arc<RwLock<Vec<AgentTool>>>,
    steering_queue: Arc<RwLock<MessageQueue>>,
    follow_up_queue: Arc<RwLock<MessageQueue>>,
    phase: AtomicU8,
    run_abort: Arc<RwLock<Option<AbortSignal>>>,
    convert_to_llm: Arc<dyn Fn(Vec<AgentMessage>) -> Vec<Message> + Send + Sync>,
    transform_context: Option<
        Arc<
            dyn Fn(
                    Vec<AgentMessage>,
                    Option<AbortSignal>,
                ) -> Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>>
                + Send
                + Sync,
        >,
    >,
    stream_fn: Option<StreamFn>,
    get_api_key: Option<
        Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>,
    >,
    on_payload: Option<OnPayloadFn>,
    on_response: Option<OnResponseFn>,
    before_tool_call: Option<
        Arc<
            dyn Fn(
                    BeforeToolCallContext,
                    Option<AbortSignal>,
                )
                    -> Pin<Box<dyn Future<Output = Option<BeforeToolCallResult>> + Send>>
                + Send
                + Sync,
        >,
    >,
    after_tool_call: Option<
        Arc<
            dyn Fn(
                    AfterToolCallContext,
                    Option<AbortSignal>,
                )
                    -> Pin<Box<dyn Future<Output = Option<AfterToolCallResult>> + Send>>
                + Send
                + Sync,
        >,
    >,
    prepare_next_turn: Option<
        Arc<
            dyn Fn(
                    Option<AbortSignal>,
                )
                    -> Pin<Box<dyn Future<Output = Option<AgentLoopTurnUpdate>> + Send>>
                + Send
                + Sync,
        >,
    >,
    session_id: Option<String>,
    thinking_budgets: Option<ThinkingBudgets>,
    transport: Option<Transport>,
    max_retry_delay_ms: Option<u64>,
    tool_execution: ToolExecutionMode,
}

impl Agent {
    pub fn new(options: AgentOptions) -> Self {
        flown_ai::register_built_in_api_providers();
        let initial_state = options.initial_state.unwrap_or_else(|| AgentState {
            system_prompt: String::new(),
            model: flown_ai::models::get_model("deepseek", "deepseek-v4-flash")
                .expect("Default model not found"),
            thinking_level: ThinkingLevel::Off,
            messages: Vec::new(),
            is_streaming: false,
            streaming_message: None,
            pending_tool_calls: HashSet::new(),
            error_message: None,
        });

        Self {
            state: Arc::new(RwLock::new(initial_state)),
            tools: Arc::new(RwLock::new(Vec::new())),
            steering_queue: Arc::new(RwLock::new(MessageQueue::new(
                options.steering_mode.unwrap_or(QueueMode::OneAtATime),
            ))),
            follow_up_queue: Arc::new(RwLock::new(MessageQueue::new(
                options.follow_up_mode.unwrap_or(QueueMode::OneAtATime),
            ))),
            phase: AtomicU8::new(AgentPhase::Idle as u8),
            run_abort: Arc::new(RwLock::new(None)),
            convert_to_llm: options.convert_to_llm.unwrap_or_else(|| {
                Arc::new(|messages| {
                    messages
                        .into_iter()
                        .map(|m| match m {
                            AgentMessage::User(u) => Message::User(u),
                            AgentMessage::Assistant(a) => Message::Assistant(a),
                            AgentMessage::ToolResult(t) => Message::ToolResult(t),
                            AgentMessage::Custom(c) => Message::User(UserMessage {
                                role: "user".to_string(),
                                content: MessageContent::Text(c.content),
                                timestamp: c.timestamp,
                            }),
                        })
                        .collect()
                })
            }),
            transform_context: options.transform_context,
            stream_fn: options.stream_fn,
            get_api_key: options.get_api_key,
            on_payload: options.on_payload,
            on_response: options.on_response,
            before_tool_call: options.before_tool_call,
            after_tool_call: options.after_tool_call,
            prepare_next_turn: options.prepare_next_turn,
            session_id: options.session_id,
            thinking_budgets: options.thinking_budgets,
            transport: options.transport,
            max_retry_delay_ms: options.max_retry_delay_ms,
            tool_execution: options
                .tool_execution
                .unwrap_or(ToolExecutionMode::Parallel),
        }
    }

    // ── Phase ───────────────────────────────────────────────────────

    /// Current execution phase.
    pub fn phase(&self) -> AgentPhase {
        match self.phase.load(Ordering::Acquire) {
            0 => AgentPhase::Idle,
            _ => AgentPhase::Turn,
        }
    }

    /// Whether the agent is idle (not running a turn).
    pub fn is_idle(&self) -> bool {
        self.phase() == AgentPhase::Idle
    }

    fn set_phase(&self, p: AgentPhase) {
        self.phase.store(p as u8, Ordering::Release);
    }

    // ── State getters ───────────────────────────────────────────────

    /// Current agent state snapshot.
    pub fn state(&self) -> AgentState {
        self.state.read().clone()
    }

    /// Current model.
    pub fn model(&self) -> Model {
        self.state.read().model.clone()
    }

    /// Current thinking level.
    pub fn thinking_level(&self) -> ThinkingLevel {
        self.state.read().thinking_level.clone()
    }

    /// Current system prompt.
    pub fn system_prompt(&self) -> String {
        self.state.read().system_prompt.clone()
    }

    // ── State setters ───────────────────────────────────────────────

    /// Set model.
    pub fn set_model(&self, model: Model) {
        self.state.write().model = model;
    }

    /// Set thinking level.
    pub fn set_thinking_level(&self, level: ThinkingLevel) {
        self.state.write().thinking_level = level;
    }

    /// Set system prompt.
    pub fn set_system_prompt(&self, prompt: String) {
        self.state.write().system_prompt = prompt;
    }

    // ── Tools ───────────────────────────────────────────────────────

    /// Set tools (full replace).
    pub fn set_tools(&self, tools: Vec<AgentTool>) {
        *self.tools.write() = tools;
    }

    /// Get current tools.
    pub fn tools(&self) -> Vec<AgentTool> {
        self.tools.read().clone()
    }

    // ── Message queues ──────────────────────────────────────────────

    /// Queue a steering message (injected before the next LLM call).
    pub fn steer(&self, message: AgentMessage) {
        self.steering_queue.write().messages.push(message);
    }

    /// Queue a follow-up message (injected when the agent would stop).
    pub fn follow_up(&self, message: AgentMessage) {
        self.follow_up_queue.write().messages.push(message);
    }

    /// Clear all queued messages.
    pub fn clear_all_queues(&self) {
        self.steering_queue.write().messages.clear();
        self.follow_up_queue.write().messages.clear();
    }

    /// Set steering queue mode.
    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.steering_queue.write().mode = mode;
    }

    /// Set follow-up queue mode.
    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.follow_up_queue.write().mode = mode;
    }

    // ── Abort ───────────────────────────────────────────────────────

    /// Abort the current run. Clears queues and cancels the in-flight
    /// stream via its AbortSignal.
    pub fn abort(&self) {
        self.clear_all_queues();
        if let Some(signal) = self.run_abort.write().take() {
            signal.cancel();
        }
    }

    // ── Run to completion (primary API) ─────────────────────────────

    /// Run the agent with text and optional images. Consumes the event
    /// stream internally and returns the final assistant message.
    ///
    /// Creates a fresh AbortSignal per turn (stored on `run_abort`) so
    /// `abort()` can interrupt it. Manages phase transitions
    /// (Idle → Turn → Idle). Returns `AgentError::Busy` if already running.
    pub async fn run(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
    ) -> Result<AssistantMessage, AgentError> {
        self.ensure_idle()?;

        let signal = AbortSignal::new();
        *self.run_abort.write() = Some(signal.clone());

        let result = self.execute_turn(text, images, signal).await;

        self.run_abort.write().take();
        self.set_phase(AgentPhase::Idle);

        result
    }

    /// Run with pre-built messages (for steer/follow-up/next-turn patterns).
    pub async fn run_messages(
        &self,
        messages: Vec<AgentMessage>,
    ) -> Result<AssistantMessage, AgentError> {
        self.ensure_idle()?;

        let signal = AbortSignal::new();
        *self.run_abort.write() = Some(signal.clone());

        let result = self.execute_turn_messages(messages, signal).await;

        self.run_abort.write().take();
        self.set_phase(AgentPhase::Idle);

        result
    }

    // ── Stream-based API ────────────────────────────────────────────

    /// Start a prompt, returning a stream. Creates and stores an
    /// AbortSignal so `abort()` works. Caller must manage phase if
    /// needed — this method does NOT set phase (use `run()` for that).
    pub fn prompt_messages(
        &self,
        messages: Vec<AgentMessage>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
        let context = self.create_context_snapshot();
        let config = self.create_loop_config();
        let signal = AbortSignal::new();
        *self.run_abort.write() = Some(signal.clone());
        agent_loop(messages, context, config, Some(signal), None)
    }

    /// Start a prompt with text.
    pub fn prompt(&self, input: String) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
        let messages = vec![AgentMessage::User(UserMessage {
            role: "user".to_string(),
            content: MessageContent::Text(input),
            timestamp: chrono::Utc::now(),
        })];
        self.prompt_messages(messages)
    }

    /// Continue from current transcript (drains steer/follow-up queues).
    pub fn continue_loop(&self) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
        let last_is_assistant = self
            .state
            .read()
            .messages
            .last()
            .is_some_and(|m| matches!(m, AgentMessage::Assistant(_)));

        if last_is_assistant {
            let steering = self.steering_queue.write().drain();
            if !steering.is_empty() {
                return self.prompt_messages(steering);
            }
            let follow_up = self.follow_up_queue.write().drain();
            if !follow_up.is_empty() {
                return self.prompt_messages(follow_up);
            }
            panic!("Cannot continue from message role: assistant");
        }

        let context = self.create_context_snapshot();
        let config = self.create_loop_config();
        let signal = AbortSignal::new();
        *self.run_abort.write() = Some(signal.clone());
        agent_loop_continue(context, config, Some(signal), None)
    }

    /// Reset agent state.
    pub fn reset(&self) {
        let mut state = self.state.write();
        state.messages.clear();
        state.is_streaming = false;
        state.streaming_message = None;
        state.pending_tool_calls.clear();
        state.error_message = None;
        drop(state);
        self.clear_all_queues();
    }

    // ── Internal ────────────────────────────────────────────────────

    fn ensure_idle(&self) -> Result<(), AgentError> {
        if self.phase.compare_exchange(
            AgentPhase::Idle as u8,
            AgentPhase::Turn as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) != Ok(AgentPhase::Idle as u8)
        {
            return Err(AgentError::Busy);
        }
        Ok(())
    }

    fn create_context_snapshot(&self) -> AgentContext {
        let state = self.state.read();
        let tools = self.tools.read();
        AgentContext {
            system_prompt: state.system_prompt.clone(),
            messages: state.messages.clone(),
            tools: tools.clone(),
        }
    }

    fn create_loop_config(&self) -> AgentLoopConfig {
        let state = self.state.read();
        let convert_to_llm = self.convert_to_llm.clone();
        let transform_context = self.transform_context.clone();
        let stream_fn = self.stream_fn.clone();
        let get_api_key = self.get_api_key.clone();
        let on_payload = self.on_payload.clone();
        let on_response = self.on_response.clone();
        let before_tool_call = self.before_tool_call.clone();
        let after_tool_call = self.after_tool_call.clone();
        let steering_queue = self.steering_queue.clone();
        let follow_up_queue = self.follow_up_queue.clone();

        AgentLoopConfig {
            model: state.model.clone(),
            reasoning: if state.thinking_level == ThinkingLevel::Off {
                None
            } else {
                Some(state.thinking_level.clone())
            },
            session_id: self.session_id.clone(),
            thinking_budgets: self.thinking_budgets.clone(),
            transport: self.transport.clone(),
            max_retry_delay_ms: self.max_retry_delay_ms,
            on_payload,
            on_response,
            convert_to_llm,
            transform_context,
            get_api_key,
            stream_fn,
            should_stop_after_turn: None,
            // Task 6 rewrites Agent; until then the PrepareNextTurnContext-taking
            // signature has no compatible source, so disable the hook here.
            prepare_next_turn: None,
            get_steering_messages: Some(Arc::new(move || {
                let msgs = steering_queue.write().drain();
                Box::pin(async move { msgs })
                    as Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>>
            })),
            get_follow_up_messages: Some(Arc::new(move || {
                let msgs = follow_up_queue.write().drain();
                Box::pin(async move { msgs })
                    as Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>>
            })),
            tool_execution: self.tool_execution.clone(),
            before_tool_call,
            after_tool_call,
        }
    }

    async fn execute_turn(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        signal: AbortSignal,
    ) -> Result<AssistantMessage, AgentError> {
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

        let user_message = AgentMessage::User(UserMessage {
            role: "user".to_string(),
            content,
            timestamp: chrono::Utc::now(),
        });

        self.execute_turn_messages(vec![user_message], signal).await
    }

    async fn execute_turn_messages(
        &self,
        messages: Vec<AgentMessage>,
        signal: AbortSignal,
    ) -> Result<AssistantMessage, AgentError> {
        let context = self.create_context_snapshot();
        let config = self.create_loop_config();

        let mut stream = agent_loop(messages, context, config, Some(signal), None);
        let mut last_message = None;

        while let Some(event) = stream.next().await {
            match &event {
                AgentEvent::MessageEnd {
                    message: AgentMessage::Assistant(msg),
                } => {
                    last_message = Some(msg.clone());
                    self.state
                        .write()
                        .messages
                        .push(AgentMessage::Assistant(msg.clone()));
                }
                AgentEvent::MessageEnd {
                    message: AgentMessage::User(msg),
                } => {
                    self.state
                        .write()
                        .messages
                        .push(AgentMessage::User(msg.clone()));
                }
                AgentEvent::MessageEnd {
                    message: AgentMessage::Custom(msg),
                } => {
                    self.state
                        .write()
                        .messages
                        .push(AgentMessage::Custom(msg.clone()));
                }
                AgentEvent::TurnEnd { tool_results, .. } => {
                    for result in tool_results {
                        self.state
                            .write()
                            .messages
                            .push(AgentMessage::ToolResult(result.clone()));
                    }
                }
                _ => {}
            }
        }

        last_message.ok_or(AgentError::NoResponse)
    }
}

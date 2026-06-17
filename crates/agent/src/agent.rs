use crate::agent_loop::{run_agent_loop, run_agent_loop_continue, AgentEventSink};
use crate::types::*;
use flown_ai::types::*;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Agent options for construction.
pub struct AgentOptions {
    pub initial_state: Option<AgentState>,
    pub convert_to_llm: Option<Arc<dyn Fn(Vec<AgentMessage>) -> Vec<Message> + Send + Sync>>,
    pub transform_context: Option<
        Arc<
            dyn Fn(
                    Vec<AgentMessage>,
                    Option<AbortSignal>,
                )
                    -> Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>>
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
                    PrepareNextTurnContext,
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

/// Stateful wrapper around the low-level agent loop (pi-mono callback model).
///
/// Owns the transcript, emits lifecycle events to subscribed listeners, executes
/// tools, and exposes queueing APIs for steering/follow-up messages. A run is
/// driven on a single tokio task; listeners are awaited in subscription order
/// and are part of the run's settlement (the agent is not idle until all
/// `agent_end` listeners finish).
#[derive(Clone)]
pub struct Agent {
    state: Arc<RwLock<AgentState>>,
    tools: Arc<RwLock<Vec<AgentTool>>>,
    steering_queue: Arc<RwLock<MessageQueue>>,
    follow_up_queue: Arc<RwLock<MessageQueue>>,
    listeners: Arc<RwLock<Vec<AgentListener>>>,
    // Per-run handle: abort signal + completion notifier + "active" flag.
    run: Arc<RwLock<Option<RunHandle>>>,
    convert_to_llm: Arc<dyn Fn(Vec<AgentMessage>) -> Vec<Message> + Send + Sync>,
    transform_context: Option<
        Arc<
            dyn Fn(
                    Vec<AgentMessage>,
                    Option<AbortSignal>,
                )
                    -> Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>>
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
                    PrepareNextTurnContext,
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

struct RunHandle {
    signal: AbortSignal,
    idle: Arc<tokio::sync::Notify>,
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
            listeners: Arc::new(RwLock::new(Vec::new())),
            run: Arc::new(RwLock::new(None)),
            convert_to_llm: options
                .convert_to_llm
                .unwrap_or_else(|| Arc::new(default_convert_to_llm)),
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
            tool_execution: options.tool_execution.unwrap_or(ToolExecutionMode::Parallel),
        }
    }

    // ── Subscription ───────────────────────────────────────────────

    /// Subscribe to lifecycle events. Returns a guard whose `Drop`/`unsubscribe`
    /// removes the listener. Listeners are awaited in subscription order.
    pub fn subscribe(&self, listener: AgentListener) -> Subscription {
        self.listeners.write().push(listener);
        let listeners = self.listeners.clone();
        let idx = self.listeners.read().len() - 1;
        Subscription::new(Box::new(move || {
            if idx < listeners.read().len() {
                listeners.write().remove(idx);
            }
        }))
    }

    // ── State snapshot + setters (JS `state.x = y` mapping) ────────

    pub fn state(&self) -> AgentState {
        self.state.read().clone()
    }
    pub fn set_model(&self, model: Model) {
        self.state.write().model = model;
    }
    pub fn set_thinking_level(&self, level: ThinkingLevel) {
        self.state.write().thinking_level = level;
    }
    pub fn set_system_prompt(&self, prompt: String) {
        self.state.write().system_prompt = prompt;
    }
    pub fn set_tools(&self, tools: Vec<AgentTool>) {
        *self.tools.write() = tools;
    }
    pub fn set_messages(&self, messages: Vec<AgentMessage>) {
        self.state.write().messages = messages;
    }

    // ── Queue modes ────────────────────────────────────────────────

    pub fn steering_mode(&self) -> QueueMode {
        self.steering_queue.read().mode.clone()
    }
    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.steering_queue.write().mode = mode;
    }
    pub fn follow_up_mode(&self) -> QueueMode {
        self.follow_up_queue.read().mode.clone()
    }
    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.follow_up_queue.write().mode = mode;
    }

    // ── Queues ─────────────────────────────────────────────────────

    pub fn steer(&self, message: AgentMessage) {
        self.steering_queue.write().messages.push(message);
    }
    pub fn follow_up(&self, message: AgentMessage) {
        self.follow_up_queue.write().messages.push(message);
    }
    pub fn clear_steering_queue(&self) {
        self.steering_queue.write().messages.clear();
    }
    pub fn clear_follow_up_queue(&self) {
        self.follow_up_queue.write().messages.clear();
    }
    pub fn clear_all_queues(&self) {
        self.clear_steering_queue();
        self.clear_follow_up_queue();
    }
    pub fn has_queued_messages(&self) -> bool {
        !self.steering_queue.read().messages.is_empty()
            || !self.follow_up_queue.read().messages.is_empty()
    }

    // ── Run control ────────────────────────────────────────────────

    /// Active run's abort signal, if any.
    pub fn signal(&self) -> Option<AbortSignal> {
        self.run.read().as_ref().map(|h| h.signal.clone())
    }

    /// Abort the current run (cancels its abort signal + clears queues).
    pub fn abort(&self) {
        self.clear_all_queues();
        if let Some(handle) = self.run.write().take() {
            handle.signal.cancel();
            handle.idle.notify_waiters();
        }
    }

    /// Resolve once the current run (and all awaited listeners) have settled.
    pub async fn wait_for_idle(&self) {
        let notify = self.run.read().as_ref().map(|h| h.idle.clone());
        if let Some(notify) = notify {
            notify.notified().await;
        }
    }

    /// Clear transcript + runtime + queued messages.
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

    // ── Main API ───────────────────────────────────────────────────

    /// Start a new prompt. Errors (provider/runtime) surface as an error
    /// assistant message event sequence, not as `Err` — `Err` is reserved for
    /// re-entrancy guards (`AlreadyProcessing`).
    pub async fn prompt(&self, input: PromptInput) -> Result<(), AgentError> {
        if self.run.read().is_some() {
            return Err(AgentError::AlreadyProcessing);
        }
        let messages = self.normalize_prompt_input(input);
        self.run_prompt_messages(messages, false).await;
        Ok(())
    }

    /// Continue from the current transcript. Drains steer/follow-up queues
    /// when the last message is an assistant message.
    pub async fn continue_run(&self) -> Result<(), AgentError> {
        if self.run.read().is_some() {
            return Err(AgentError::AlreadyProcessing);
        }
        let last_is_assistant = self
            .state
            .read()
            .messages
            .last()
            .is_some_and(|m| matches!(m, AgentMessage::Assistant(_)));

        if self.state.read().messages.is_empty() {
            return Err(AgentError::NoMessages);
        }

        if last_is_assistant {
            let steering = self.steering_queue.write().drain();
            if !steering.is_empty() {
                self.run_prompt_messages(steering, true).await; // skip initial steering poll
                return Ok(());
            }
            let follow_up = self.follow_up_queue.write().drain();
            if !follow_up.is_empty() {
                self.run_prompt_messages(follow_up, false).await;
                return Ok(());
            }
            return Err(AgentError::CannotContinueFromAssistant);
        }

        self.run_continuation().await;
        Ok(())
    }

    // ── Internal ───────────────────────────────────────────────────

    fn normalize_prompt_input(&self, input: PromptInput) -> Vec<AgentMessage> {
        match input {
            PromptInput::Text(text) => vec![AgentMessage::User(UserMessage {
                role: "user".to_string(),
                content: MessageContent::Text(text),
                timestamp: chrono::Utc::now(),
            })],
            PromptInput::TextWithImages { text, images } => {
                let mut blocks = vec![UserContentBlock::Text(TextContent {
                    content_type: "text".to_string(),
                    text,
                    text_signature: None,
                })];
                for image in images {
                    blocks.push(UserContentBlock::Image(image));
                }
                vec![AgentMessage::User(UserMessage {
                    role: "user".to_string(),
                    content: MessageContent::Blocks(blocks),
                    timestamp: chrono::Utc::now(),
                })]
            }
            PromptInput::Messages(messages) => messages,
        }
    }

    async fn run_prompt_messages(&self, messages: Vec<AgentMessage>, skip_initial_steering: bool) {
        let context = self.create_context_snapshot();
        let config = self.create_loop_config(skip_initial_steering);
        let signal = AbortSignal::new();
        let idle = Arc::new(tokio::sync::Notify::new());
        *self.run.write() = Some(RunHandle {
            signal: signal.clone(),
            idle: idle.clone(),
        });

        self.state.write().is_streaming = true;
        self.state.write().streaming_message = None;
        self.state.write().error_message = None;

        let sink = self.make_event_sink();
        self.drive_loop(
            async move {
                let _ = run_agent_loop(
                    messages,
                    context,
                    config,
                    sink,
                    Some(signal),
                    self.stream_fn.clone(),
                )
                .await;
            },
            idle,
        )
        .await;
    }

    async fn run_continuation(&self) {
        let context = self.create_context_snapshot();
        let config = self.create_loop_config(false);
        let signal = AbortSignal::new();
        let idle = Arc::new(tokio::sync::Notify::new());
        *self.run.write() = Some(RunHandle {
            signal: signal.clone(),
            idle: idle.clone(),
        });

        self.state.write().is_streaming = true;

        let sink = self.make_event_sink();
        self.drive_loop(
            async move {
                let _ = run_agent_loop_continue(
                    context,
                    config,
                    sink,
                    Some(signal),
                    self.stream_fn.clone(),
                )
                .await;
            },
            idle,
        )
        .await;
    }

    /// Build a sink that reduces each event into `state`, then awaits all
    /// listeners in subscription order.
    fn make_event_sink(&self) -> AgentEventSink {
        let state = self.state.clone();
        let listeners = self.listeners.clone();
        let signal_slot = self.run.clone();
        Arc::new(move |event| {
            let state = state.clone();
            let listeners = listeners.clone();
            let signal_slot = signal_slot.clone();
            Box::pin(async move {
                // Reduce into state (pi-mono processEvents).
                match &event {
                    AgentEvent::MessageStart { message } => {
                        state.write().streaming_message = Some(message.clone());
                    }
                    AgentEvent::MessageUpdate { message, .. } => {
                        state.write().streaming_message = Some(message.clone());
                    }
                    AgentEvent::MessageEnd { message } => {
                        state.write().streaming_message = None;
                        state.write().messages.push(message.clone());
                    }
                    AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                        state.write().pending_tool_calls.insert(tool_call_id.clone());
                    }
                    AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                        state.write().pending_tool_calls.remove(tool_call_id);
                    }
                    AgentEvent::TurnEnd { message, .. } => {
                        if let AgentMessage::Assistant(a) = message {
                            if a.error_message.is_some() {
                                state.write().error_message = a.error_message.clone();
                            }
                        }
                    }
                    AgentEvent::AgentEnd { .. } => {
                        state.write().streaming_message = None;
                    }
                    _ => {}
                }
                // Await listeners in order with the active signal. Clone the
                // listener Arcs out of the guard first so the guard is dropped
                // before the await (RwLockReadGuard is not Send).
                let signal = signal_slot.read().as_ref().map(|h| h.signal.clone());
                let snapshot: Vec<AgentListener> = listeners.read().clone();
                for listener in snapshot {
                    listener(event.clone(), signal.clone()).await;
                }
            })
        })
    }

    /// Run the loop future to completion, then finish the run (clear handle,
    /// reset streaming flags, notify waiters). Failure inside the loop is
    /// converted to an error assistant message event sequence before settling.
    async fn drive_loop<F>(&self, loop_future: F, idle: Arc<tokio::sync::Notify>)
    where
        F: std::future::Future<Output = ()> + Send,
    {
        // The loop itself never panics on provider errors — those are encoded
        // as error events by run_loop. Await it, then settle.
        loop_future.await;

        {
            let mut state = self.state.write();
            state.is_streaming = false;
            state.streaming_message = None;
            state.pending_tool_calls.clear();
        }
        self.run.write().take();
        idle.notify_waiters();
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

    fn create_loop_config(&self, skip_initial_steering: bool) -> AgentLoopConfig {
        let state = self.state.read();
        let steering_queue = self.steering_queue.clone();
        let follow_up_queue = self.follow_up_queue.clone();
        let skip = Arc::new(AtomicBool::new(skip_initial_steering));
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
            on_payload: self.on_payload.clone(),
            on_response: self.on_response.clone(),
            convert_to_llm: self.convert_to_llm.clone(),
            transform_context: self.transform_context.clone(),
            get_api_key: self.get_api_key.clone(),
            stream_fn: self.stream_fn.clone(),
            should_stop_after_turn: None,
            prepare_next_turn: self.prepare_next_turn.clone(),
            get_steering_messages: Some(Arc::new(move || {
                if skip.swap(false, Ordering::SeqCst) {
                    let msgs = Vec::new();
                    Box::pin(async move { msgs })
                        as Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>>
                } else {
                    let msgs = steering_queue.write().drain();
                    Box::pin(async move { msgs })
                }
            })),
            get_follow_up_messages: Some(Arc::new(move || {
                let msgs = follow_up_queue.write().drain();
                Box::pin(async move { msgs })
            })),
            tool_execution: self.tool_execution.clone(),
            before_tool_call: self.before_tool_call.clone(),
            after_tool_call: self.after_tool_call.clone(),
        }
    }
}

fn default_convert_to_llm(messages: Vec<AgentMessage>) -> Vec<Message> {
    messages
        .into_iter()
        .filter_map(|m| match m {
            AgentMessage::User(u) => Some(Message::User(u)),
            AgentMessage::Assistant(a) => Some(Message::Assistant(a)),
            AgentMessage::ToolResult(t) => Some(Message::ToolResult(t)),
            AgentMessage::Custom(_) => None,
        })
        .collect()
}

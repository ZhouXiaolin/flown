use crate::agent_loop::{agent_loop, agent_loop_continue};
use crate::types::*;
use flown_ai::types::*;
use futures::stream::Stream;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

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
            dyn Fn(Option<AbortSignal>)
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

/// Pending message queue
struct PendingMessageQueue {
    messages: Vec<AgentMessage>,
    mode: QueueMode,
}

impl PendingMessageQueue {
    fn new(mode: QueueMode) -> Self {
        Self {
            messages: Vec::new(),
            mode,
        }
    }

    fn enqueue(&mut self, message: AgentMessage) {
        self.messages.push(message);
    }

    fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    fn drain(&mut self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::All => {
                let drained = self.messages.clone();
                self.messages.clear();
                drained
            }
            QueueMode::OneAtATime => {
                if self.messages.is_empty() {
                    Vec::new()
                } else {
                    vec![self.messages.remove(0)]
                }
            }
        }
    }

    fn clear(&mut self) {
        self.messages.clear();
    }
}

/// Stateful agent
pub struct Agent {
    state: Arc<RwLock<AgentState>>,
    tools: Arc<RwLock<Vec<AgentTool>>>,
    steering_queue: Arc<RwLock<PendingMessageQueue>>,
    follow_up_queue: Arc<RwLock<PendingMessageQueue>>,
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
            dyn Fn(Option<AbortSignal>)
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
        flown_ai::init();
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
            steering_queue: Arc::new(RwLock::new(PendingMessageQueue::new(
                options.steering_mode.unwrap_or(QueueMode::OneAtATime),
            ))),
            follow_up_queue: Arc::new(RwLock::new(PendingMessageQueue::new(
                options.follow_up_mode.unwrap_or(QueueMode::OneAtATime),
            ))),
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

    /// Get current agent state
    pub fn state(&self) -> AgentState {
        self.state.read().unwrap().clone()
    }

    /// Set tools for the agent
    pub fn set_tools(&self, tools: Vec<AgentTool>) {
        *self.tools.write().unwrap() = tools;
    }

    /// Get current tools
    pub fn tools(&self) -> Vec<AgentTool> {
        self.tools.read().unwrap().clone()
    }

    /// Set steering mode
    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.steering_queue.write().unwrap().mode = mode;
    }

    /// Set follow-up mode
    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.follow_up_queue.write().unwrap().mode = mode;
    }

    /// Queue a steering message
    pub fn steer(&self, message: AgentMessage) {
        self.steering_queue.write().unwrap().enqueue(message);
    }

    /// Queue a follow-up message
    pub fn follow_up(&self, message: AgentMessage) {
        self.follow_up_queue.write().unwrap().enqueue(message);
    }

    /// Clear steering queue
    pub fn clear_steering_queue(&self) {
        self.steering_queue.write().unwrap().clear();
    }

    /// Clear follow-up queue
    pub fn clear_follow_up_queue(&self) {
        self.follow_up_queue.write().unwrap().clear();
    }

    /// Clear all queues
    pub fn clear_all_queues(&self) {
        self.clear_steering_queue();
        self.clear_follow_up_queue();
    }

    /// Check if queues have pending messages
    pub fn has_queued_messages(&self) -> bool {
        self.steering_queue.read().unwrap().has_items()
            || self.follow_up_queue.read().unwrap().has_items()
    }

    /// Start a new prompt
    pub fn prompt(&self, input: String) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
        let messages = vec![AgentMessage::User(UserMessage {
            role: "user".to_string(),
            content: MessageContent::Text(input),
            timestamp: chrono::Utc::now(),
        })];
        self.prompt_messages(messages)
    }

    /// Start a new prompt with messages
    pub fn prompt_messages(
        &self,
        messages: Vec<AgentMessage>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
        let context = self.create_context_snapshot();
        let config = self.create_loop_config();

        agent_loop(messages, context, config, None)
    }

    /// Continue from current transcript
    pub fn continue_loop(&self) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
        let state = self.state.read().unwrap();
        let last_message = state.messages.last();

        if let Some(AgentMessage::Assistant(_)) = last_message {
            // Try steering queue first
            let steering = self.steering_queue.write().unwrap().drain();
            if !steering.is_empty() {
                return self.prompt_messages(steering);
            }

            // Try follow-up queue
            let follow_up = self.follow_up_queue.write().unwrap().drain();
            if !follow_up.is_empty() {
                return self.prompt_messages(follow_up);
            }

            panic!("Cannot continue from message role: assistant");
        }

        let context = self.create_context_snapshot();
        let config = self.create_loop_config();

        agent_loop_continue(context, config, None)
    }

    /// Reset agent state
    pub fn reset(&self) {
        let mut state = self.state.write().unwrap();
        state.messages.clear();
        state.is_streaming = false;
        state.streaming_message = None;
        state.pending_tool_calls.clear();
        state.error_message = None;
        self.clear_all_queues();
    }

    fn create_context_snapshot(&self) -> AgentContext {
        let state = self.state.read().unwrap();
        let tools = self.tools.read().unwrap();
        AgentContext {
            system_prompt: state.system_prompt.clone(),
            messages: state.messages.clone(),
            tools: tools.clone(),
        }
    }

    fn create_loop_config(&self) -> AgentLoopConfig {
        let state = self.state.read().unwrap();
        let convert_to_llm = self.convert_to_llm.clone();
        let transform_context = self.transform_context.clone();
        let stream_fn = self.stream_fn.clone();
        let get_api_key = self.get_api_key.clone();
        let on_payload = self.on_payload.clone();
        let on_response = self.on_response.clone();
        let before_tool_call = self.before_tool_call.clone();
        let after_tool_call = self.after_tool_call.clone();
        let prepare_next_turn = self.prepare_next_turn.clone();
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
            convert_to_llm: convert_to_llm,
            transform_context: transform_context,
            get_api_key: get_api_key,
            stream_fn,
            should_stop_after_turn: None,
            prepare_next_turn,
            get_steering_messages: Some(Arc::new(move || {
                let msgs = steering_queue.write().unwrap().drain();
                Box::pin(async move { msgs })
                    as Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>>
            })),
            get_follow_up_messages: Some(Arc::new(move || {
                let msgs = follow_up_queue.write().unwrap().drain();
                Box::pin(async move { msgs })
                    as Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>>
            })),
            tool_execution: self.tool_execution.clone(),
            before_tool_call: before_tool_call,
            after_tool_call: after_tool_call,
        }
    }
}

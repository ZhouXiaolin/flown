pub use flown_ai::types::AbortSignal;
use flown_ai::types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Stream function used by the agent loop
pub type StreamFn = Arc<
    dyn Fn(
            Model,
            Context,
            Option<SimpleStreamOptions>,
        ) -> flown_ai::api_registry::AssistantMessageEventStream
        + Send
        + Sync,
>;

/// Tool execution mode
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode {
    Sequential,
    Parallel,
}

/// Queue mode for steering/follow-up messages
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    All,
    OneAtATime,
}

/// Result from beforeToolCall hook
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeforeToolCallResult {
    pub block: Option<bool>,
    pub reason: Option<String>,
}

/// Result from afterToolCall hook
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AfterToolCallResult {
    pub content: Option<Vec<ToolResultContent>>,
    pub details: Option<serde_json::Value>,
    pub is_error: Option<bool>,
    pub terminate: Option<bool>,
}

/// Context passed to beforeToolCall
#[derive(Debug, Clone)]
pub struct BeforeToolCallContext {
    pub assistant_message: AssistantMessage,
    pub tool_call: ToolCall,
    pub args: serde_json::Value,
    pub context: AgentContext,
}

/// Context passed to afterToolCall
#[derive(Debug, Clone)]
pub struct AfterToolCallContext {
    pub assistant_message: AssistantMessage,
    pub tool_call: ToolCall,
    pub args: serde_json::Value,
    pub result: AgentToolResult,
    pub is_error: bool,
    pub context: AgentContext,
}

/// Context passed to shouldStopAfterTurn
#[derive(Debug, Clone)]
pub struct ShouldStopAfterTurnContext {
    pub message: AssistantMessage,
    pub tool_results: Vec<ToolResultMessage>,
    pub context: AgentContext,
    pub new_messages: Vec<AgentMessage>,
}

/// Replacement runtime state for next turn
#[derive(Debug, Clone)]
pub struct AgentLoopTurnUpdate {
    pub context: Option<AgentContext>,
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
}

/// Agent loop configuration
pub struct AgentLoopConfig {
    pub model: Model,
    pub reasoning: Option<ThinkingLevel>,
    pub session_id: Option<String>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub transport: Option<Transport>,
    pub max_retry_delay_ms: Option<u64>,
    pub on_payload: Option<OnPayloadFn>,
    pub on_response: Option<OnResponseFn>,
    pub convert_to_llm: Arc<dyn Fn(Vec<AgentMessage>) -> Vec<Message> + Send + Sync>,
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
    pub get_api_key: Option<
        Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>,
    >,
    pub stream_fn: Option<StreamFn>,
    pub should_stop_after_turn: Option<
        Arc<
            dyn Fn(ShouldStopAfterTurnContext) -> Pin<Box<dyn Future<Output = bool> + Send>>
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
    pub get_steering_messages: Option<
        Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>> + Send + Sync>,
    >,
    pub get_follow_up_messages: Option<
        Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>> + Send + Sync>,
    >,
    pub tool_execution: ToolExecutionMode,
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
}

/// Agent message - extends LLM messages with custom types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum AgentMessage {
    #[serde(rename = "user")]
    User(UserMessage),
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),
    #[serde(rename = "toolResult")]
    ToolResult(ToolResultMessage),
    #[serde(rename = "custom")]
    Custom(CustomAgentMessage),
}

/// Custom agent message with arbitrary type and content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomAgentMessage {
    pub custom_type: String,
    pub content: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Agent tool result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentToolResult {
    pub content: Vec<ToolResultContent>,
    pub details: serde_json::Value,
    pub terminate: Option<bool>,
}

/// Error returned by agent tools.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct AgentToolError {
    message: String,
}

impl AgentToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub type ToolUpdateFn =
    Arc<dyn Fn(serde_json::Value) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

pub type AgentToolFuture =
    Pin<Box<dyn Future<Output = Result<AgentToolResult, AgentToolError>> + Send>>;

/// Agent tool definition
pub struct AgentTool {
    pub name: String,
    pub label: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub execute: Arc<
        dyn Fn(
                String,
                serde_json::Value,
                Option<AbortSignal>,
                Option<ToolUpdateFn>,
            ) -> AgentToolFuture
            + Send
            + Sync,
    >,
    pub prepare_arguments:
        Option<Arc<dyn Fn(serde_json::Value) -> serde_json::Value + Send + Sync>>,
    pub execution_mode: Option<ToolExecutionMode>,
}

impl std::fmt::Debug for AgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTool")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("description", &self.description)
            .field("parameters", &self.parameters)
            .field("execution_mode", &self.execution_mode)
            .finish()
    }
}

impl Clone for AgentTool {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            label: self.label.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
            execute: self.execute.clone(),
            prepare_arguments: self.prepare_arguments.clone(),
            execution_mode: self.execution_mode.clone(),
        }
    }
}

/// Agent context snapshot
#[derive(Debug, Clone)]
pub struct AgentContext {
    pub system_prompt: String,
    pub messages: Vec<AgentMessage>,
    pub tools: Vec<AgentTool>,
}

/// Agent state
#[derive(Debug, Clone)]
pub struct AgentState {
    pub system_prompt: String,
    pub model: Model,
    pub thinking_level: ThinkingLevel,
    pub messages: Vec<AgentMessage>,
    pub is_streaming: bool,
    pub streaming_message: Option<AgentMessage>,
    pub pending_tool_calls: HashSet<String>,
    pub error_message: Option<String>,
}

/// Agent event for UI updates
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    #[serde(rename = "agent_start")]
    AgentStart,
    #[serde(rename = "agent_end")]
    AgentEnd { messages: Vec<AgentMessage> },
    #[serde(rename = "turn_start")]
    TurnStart,
    #[serde(rename = "turn_end")]
    TurnEnd {
        message: AgentMessage,
        tool_results: Vec<ToolResultMessage>,
    },
    #[serde(rename = "message_start")]
    MessageStart { message: AgentMessage },
    #[serde(rename = "message_update")]
    MessageUpdate {
        message: AgentMessage,
        assistant_message_event: AssistantMessageEvent,
    },
    #[serde(rename = "message_end")]
    MessageEnd { message: AgentMessage },
    #[serde(rename = "tool_execution_start")]
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    #[serde(rename = "tool_execution_update")]
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: serde_json::Value,
        partial_result: serde_json::Value,
    },
    #[serde(rename = "tool_execution_end")]
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: serde_json::Value,
        is_error: bool,
    },
}

/// Error returned by [`crate::Agent`] operations.
///
/// Mirrors pi-mono's `agent.ts` thrown errors: re-entrant `prompt`/`continue`
/// throws, and the "cannot continue from assistant" guard. `Busy` and
/// `NoResponse` are retained only until Task 6 rewrites `Agent` and drops them.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AgentError {
    #[error("Agent is already processing a prompt. Use steer() or follow_up() to queue messages, or wait for completion.")]
    AlreadyProcessing,
    #[error("No messages to continue from")]
    NoMessages,
    #[error("Cannot continue from message role: assistant")]
    CannotContinueFromAssistant,
    #[error("{0}")]
    Other(String),
    // Legacy variants — removed in Task 6 when `Agent` is rewritten:
    #[error("agent is busy")]
    Busy,
    #[error("no assistant response")]
    NoResponse,
}

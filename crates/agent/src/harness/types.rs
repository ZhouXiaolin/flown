use super::prompt_templates::PromptTemplate;
use super::session::types::SessionTreeEntry;
use super::skills::Skill;
use crate::types::*;
use flown_ai::types::*;
use serde::{Deserialize, Serialize};

/// Agent harness phase
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentHarnessPhase {
    Idle,
    Turn,
    Compaction,
    BranchSummary,
    Retry,
}

/// Model update source
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelUpdateSource {
    Set,
    Restore,
}

/// Tool update source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolUpdateSource {
    Set,
    Restore,
}

/// Harness resources
#[derive(Debug, Clone, Default)]
pub struct HarnessResources {
    pub skills: Vec<Skill>,
    pub prompt_templates: Vec<PromptTemplate>,
}

/// Stream options for the harness
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HarnessStreamOptions {
    pub transport: Option<Transport>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    pub headers: Option<std::collections::HashMap<String, String>>,
    pub metadata: Option<std::collections::HashMap<String, serde_json::Value>>,
    pub cache_retention: Option<CacheRetention>,
}

/// Per-request stream option patch returned by `before_provider_request`.
///
/// `headers` and `metadata` use open JSON so hook boundaries can represent
/// pi-mono's deletion semantics: a null field clears the whole map, while null
/// values inside a map delete individual keys.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HarnessStreamOptionsPatch {
    pub transport: Option<Transport>,
    #[serde(rename = "timeoutMs", alias = "timeout_ms")]
    pub timeout_ms: Option<u64>,
    #[serde(rename = "maxRetries", alias = "max_retries")]
    pub max_retries: Option<u32>,
    #[serde(rename = "maxRetryDelayMs", alias = "max_retry_delay_ms")]
    pub max_retry_delay_ms: Option<u64>,
    pub headers: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
    #[serde(rename = "cacheRetention", alias = "cache_retention")]
    pub cache_retention: Option<CacheRetention>,
}

/// All harness events
#[derive(Debug, Clone)]
pub enum HarnessEvent {
    // Queue events
    QueueUpdate {
        steer: Vec<AgentMessage>,
        follow_up: Vec<AgentMessage>,
        next_turn: Vec<AgentMessage>,
    },
    SavePoint {
        had_pending_mutations: bool,
    },
    Abort {
        cleared_steer: Vec<AgentMessage>,
        cleared_follow_up: Vec<AgentMessage>,
    },
    Settled {
        next_turn_count: usize,
    },

    // Hook events
    BeforeAgentStart {
        prompt: String,
        images: Option<Vec<ImageContent>>,
        system_prompt: String,
        resources: HarnessResources,
    },
    Context {
        messages: Vec<AgentMessage>,
    },
    BeforeProviderRequest {
        model: Model,
        session_id: String,
        stream_options: HarnessStreamOptions,
    },
    BeforeProviderPayload {
        model: Model,
        payload: serde_json::Value,
    },
    AfterProviderResponse {
        status: u16,
        headers: std::collections::HashMap<String, String>,
    },
    ToolCall {
        tool_call_id: String,
        tool_name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        input: serde_json::Value,
        content: Vec<ToolResultContent>,
        details: serde_json::Value,
        is_error: bool,
    },

    // Agent loop events (forwarded from AgentEvent)
    AgentStart,
    TurnStart,
    MessageStart {
        message: AgentMessage,
    },
    MessageUpdate {
        message: AgentMessage,
        assistant_message_event: AssistantMessageEvent,
    },
    MessageEnd {
        message: AgentMessage,
    },
    TurnEnd {
        message: AgentMessage,
        tool_results: Vec<ToolResultMessage>,
    },
    AgentEnd {
        messages: Vec<AgentMessage>,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: serde_json::Value,
        partial_result: serde_json::Value,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: serde_json::Value,
        is_error: bool,
    },

    // Session events
    SessionBeforeCompact {
        preparation: super::compaction::compaction::CompactionPreparation,
        branch_entries: Vec<SessionTreeEntry>,
        custom_instructions: Option<String>,
        signal: AbortSignal,
    },
    SessionCompact {
        compaction_entry: Option<SessionTreeEntry>,
        from_hook: bool,
    },
    SessionBeforeTree {
        preparation: TreeNavigationPreparation,
        signal: AbortSignal,
    },
    SessionTree {
        new_leaf_id: Option<String>,
        old_leaf_id: Option<String>,
        summary_entry: Option<SessionTreeEntry>,
        from_hook: bool,
    },

    // Config events
    ModelUpdate {
        model: Model,
        previous_model: Option<Model>,
        source: ModelUpdateSource,
    },
    ThinkingLevelUpdate {
        level: ThinkingLevel,
        previous_level: ThinkingLevel,
    },
    ResourcesUpdate {
        resources: HarnessResources,
        previous_resources: HarnessResources,
    },
    ToolsUpdate {
        tool_names: Vec<String>,
        previous_tool_names: Vec<String>,
        active_tool_names: Vec<String>,
        previous_active_tool_names: Vec<String>,
        source: ToolUpdateSource,
    },
}

impl From<&AgentEvent> for HarnessEvent {
    fn from(event: &AgentEvent) -> Self {
        match event {
            AgentEvent::AgentStart => HarnessEvent::AgentStart,
            AgentEvent::TurnStart => HarnessEvent::TurnStart,
            AgentEvent::MessageStart { message } => HarnessEvent::MessageStart {
                message: message.clone(),
            },
            AgentEvent::MessageUpdate {
                message,
                assistant_message_event,
            } => HarnessEvent::MessageUpdate {
                message: message.clone(),
                assistant_message_event: assistant_message_event.clone(),
            },
            AgentEvent::MessageEnd { message } => HarnessEvent::MessageEnd {
                message: message.clone(),
            },
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => HarnessEvent::TurnEnd {
                message: message.clone(),
                tool_results: tool_results.clone(),
            },
            AgentEvent::AgentEnd { messages } => HarnessEvent::AgentEnd {
                messages: messages.clone(),
            },
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => HarnessEvent::ToolExecutionStart {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                args: args.clone(),
            },
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
            } => HarnessEvent::ToolExecutionUpdate {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                args: args.clone(),
                partial_result: partial_result.clone(),
            },
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            } => HarnessEvent::ToolExecutionEnd {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                result: result.clone(),
                is_error: *is_error,
            },
        }
    }
}

/// Hook result for before_agent_start
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BeforeAgentStartResult {
    #[serde(alias = "inject_messages")]
    pub messages: Option<Vec<AgentMessage>>,
    #[serde(rename = "systemPrompt", alias = "system_prompt")]
    pub system_prompt: Option<String>,
}

/// Hook result for context transformation
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextResult {
    pub messages: Option<Vec<AgentMessage>>,
}

/// Hook result for before_provider_request
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BeforeProviderRequestResult {
    #[serde(rename = "streamOptions", alias = "stream_options")]
    pub stream_options: Option<serde_json::Value>,
}

/// Hook result for before_provider_payload
///
/// Hook patches intentionally use `serde_json::Value` at the boundary. Harness
/// hooks are application extension points whose payloads can contain provider-
/// specific and app-specific fields; each result struct documents the stable
/// fields the harness will read while preserving that open JSON surface.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BeforeProviderPayloadResult {
    pub payload: Option<serde_json::Value>,
}

/// Hook result for tool_call
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub block: Option<bool>,
    pub reason: Option<String>,
}

/// Hook result for tool_result
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolResultPatch {
    pub content: Option<Vec<ToolResultContent>>,
    pub details: Option<serde_json::Value>,
    pub is_error: Option<bool>,
    pub terminate: Option<bool>,
}

/// Structured compaction result returned by hooks and compaction helpers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactResult {
    pub summary: String,
    #[serde(rename = "firstKeptEntryId", alias = "first_kept_entry_id")]
    pub first_kept_entry_id: String,
    #[serde(rename = "tokensBefore", alias = "tokens_before")]
    pub tokens_before: u64,
    pub details: Option<serde_json::Value>,
}

/// Hook result for session_before_compact
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionBeforeCompactResult {
    pub cancel: Option<bool>,
    pub compaction: Option<CompactResult>,
}

/// Summary payload returned by `session_before_tree`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionTreeSummaryResult {
    pub summary: String,
    pub details: Option<serde_json::Value>,
}

/// Hook result for session_before_tree
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionBeforeTreeResult {
    pub cancel: Option<bool>,
    pub summary: Option<SessionTreeSummaryResult>,
    #[serde(rename = "customInstructions", alias = "custom_instructions")]
    pub custom_instructions: Option<String>,
    #[serde(rename = "replaceInstructions", alias = "replace_instructions")]
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
}

/// Structured compaction error codes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionErrorCode {
    Aborted,
    SummarizationFailed,
    InvalidSession,
    Unknown,
}

/// Structured branch summary error codes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchSummaryErrorCode {
    Aborted,
    SummarizationFailed,
    InvalidSession,
}

/// Public compaction error.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[error("{message}")]
pub struct CompactionError {
    pub code: CompactionErrorCode,
    pub message: String,
}

impl CompactionError {
    pub fn new(code: CompactionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Public branch summary error.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[error("{message}")]
pub struct BranchSummaryError {
    pub code: BranchSummaryErrorCode,
    pub message: String,
}

impl BranchSummaryError {
    pub fn new(code: BranchSummaryErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Tree navigation preparation info for events
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TreeNavigationPreparation {
    #[serde(rename = "targetId")]
    pub target_id: String,
    #[serde(rename = "oldLeafId")]
    pub old_leaf_id: Option<String>,
    #[serde(rename = "commonAncestorId")]
    pub common_ancestor_id: Option<String>,
    #[serde(rename = "entriesToSummarize")]
    pub entries_to_summarize: Vec<SessionTreeEntry>,
    #[serde(rename = "userWantsSummary")]
    pub user_wants_summary: bool,
    #[serde(rename = "customInstructions")]
    pub custom_instructions: Option<String>,
    #[serde(rename = "replaceInstructions")]
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
}

/// Harness error
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessErrorCode {
    Busy,
    InvalidState,
    InvalidArgument,
    Session,
    Hook,
    Auth,
    Compaction,
    BranchSummary,
    Unknown,
}

/// Public harness error with a stable top-level classification matching pi-mono.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("harness is busy (phase: {0:?})")]
    Busy(AgentHarnessPhase),
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("session error: {0}")]
    Session(String),
    #[error("hook error: {0}")]
    Hook(String),
    #[error("auth error: {0}")]
    Auth(String),
    #[error("compaction error: {0}")]
    Compaction(CompactionError),
    #[error("branch summary error: {0}")]
    BranchSummary(BranchSummaryError),
    #[error("unknown error: {0}")]
    Unknown(String),
}

impl HarnessError {
    pub fn code(&self) -> HarnessErrorCode {
        match self {
            HarnessError::Busy(_) => HarnessErrorCode::Busy,
            HarnessError::InvalidState(_) => HarnessErrorCode::InvalidState,
            HarnessError::InvalidArgument(_) => HarnessErrorCode::InvalidArgument,
            HarnessError::Session(_) => HarnessErrorCode::Session,
            HarnessError::Hook(_) => HarnessErrorCode::Hook,
            HarnessError::Auth(_) => HarnessErrorCode::Auth,
            HarnessError::Compaction(_) => HarnessErrorCode::Compaction,
            HarnessError::BranchSummary(_) => HarnessErrorCode::BranchSummary,
            HarnessError::Unknown(_) => HarnessErrorCode::Unknown,
        }
    }
}

/// Abort result
#[derive(Debug)]
pub struct AbortResult {
    pub cleared_steer: Vec<AgentMessage>,
    pub cleared_follow_up: Vec<AgentMessage>,
}

/// Navigate tree options
#[derive(Debug, Clone, Default)]
pub struct NavigateTreeOptions {
    pub summarize: bool,
    pub custom_instructions: Option<String>,
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
}

/// Navigate tree result
#[derive(Debug)]
pub struct NavigateTreeResult {
    pub cancelled: bool,
    pub editor_text: Option<String>,
    pub summary_entry: Option<SessionTreeEntry>,
}

impl From<CompactResult> for super::compaction::compaction::CompactionResult {
    fn from(value: CompactResult) -> Self {
        let (read_files, modified_files) = value
            .details
            .as_ref()
            .map(|details| {
                (
                    details
                        .get("readFiles")
                        .and_then(|value| value.as_array())
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                    details
                        .get("modifiedFiles")
                        .and_then(|value| value.as_array())
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                )
            })
            .unwrap_or_default();

        super::compaction::compaction::CompactionResult {
            summary: value.summary,
            first_kept_entry_id: value.first_kept_entry_id,
            tokens_before: value.tokens_before,
            details: value.details,
            read_files,
            modified_files,
        }
    }
}

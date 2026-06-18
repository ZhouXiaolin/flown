mod compaction;
mod env;
mod harness;
mod messages;
mod prompt_templates;
mod session;
mod skills;
mod system_prompt;
mod types;
mod utils;

pub use compaction::branch_summary::{
    BranchPreparation, BranchSummaryDetails, BranchSummaryResult, CollectEntriesResult,
    GenerateBranchSummaryOptions, collect_entries_for_branch_summary,
    collect_entries_for_branch_summary_result, generate_branch_summary, prepare_branch_entries,
};
pub use compaction::compaction::{
    CompactionPreparation, CompactionResult, CompactionSettings, ContextUsageEstimate, CutPoint,
    DEFAULT_COMPACTION_SETTINGS, build_file_ops_tag, calculate_context_tokens, compact,
    compact_with_llm, estimate_context_tokens, estimate_message_tokens, estimate_tokens,
    extract_file_ops, find_cut_point, find_turn_start_index, generate_summary,
    generate_turn_prefix_summary, get_last_assistant_usage, prepare_compaction,
    serialize_conversation, should_compact,
};
pub use env::{
    AbortSignal, ExecOptions, ExecResult, ExecutionEnv, ExecutionError, ExecutionErrorCode,
    FileError, FileErrorCode, FileInfo, FileKind, FileSystem, Shell, ShellOutputUpdateFn,
};
pub use harness::{
    AgentHarness, AgentHarnessOptions, GetApiKeyAndHeadersFn, SystemPromptConfig,
    SystemPromptContext,
};
pub use messages::{
    BashExecutionMessage, BranchSummaryMessage, ContentBlock, CustomMessage, CustomMessageContent,
    HarnessMessage, bash_execution_to_text, convert_to_llm, create_branch_summary_message,
    create_compaction_summary_message,
};
pub use prompt_templates::{
    LoadPromptTemplatesResult, LoadSourcedPromptTemplatesResult, PromptTemplate,
    PromptTemplateDiagnostic, PromptTemplateDiagnosticCode, SourcedPromptTemplate,
    SourcedPromptTemplateDiagnostic, SourcedPromptTemplateInput, format_prompt_template_invocation,
    load_prompt_templates, load_prompt_templates_with_diagnostics, load_sourced_prompt_templates,
    parse_command_args,
};
pub use session::*;
pub use session::{
    InMemorySessionRepo, JsonlSessionCreateOptions, JsonlSessionForkOptions,
    JsonlSessionListOptions, JsonlSessionRepo, MemorySessionCreateOptions,
    MemorySessionForkOptions, Session, SessionRepo, SessionStorage, build_session_context,
    create_session_id, create_timestamp, get_entries_to_fork, get_file_system_result_or_throw,
    to_session, uuidv7,
};
pub use skills::{
    LoadSkillsResult, LoadSourcedSkillsResult, Skill, SkillDiagnostic, SkillDiagnosticCode,
    SourcedSkill, SourcedSkillDiagnostic, SourcedSkillInput, format_skill_invocation,
    format_skills_for_system_prompt, load_skills, load_skills_with_diagnostics,
    load_sourced_skills,
};
pub use system_prompt::format_skills_for_system_prompt as format_system_prompt_skills;
pub use types::{
    AgentHarnessError, AgentHarnessErrorCode, AgentHarnessEvent, AgentHarnessPhase,
    AgentHarnessResources, AgentHarnessStreamOptions, AgentHarnessStreamOptionsPatch,
    BeforeAgentStartResult, BeforeProviderPayloadResult, BeforeProviderRequestResult,
    BranchSummaryError, BranchSummaryErrorCode, CompactResult, CompactionError,
    CompactionErrorCode, ContextResult, ModelUpdateSource, NavigateTreeOptions,
    NavigateTreeResult, SessionBeforeCompactResult, SessionBeforeTreeResult,
    SessionTreeSummaryResult, ToolCallResult, ToolResultPatch, TreeNavigationPreparation,
    ToolUpdateSource,
};
pub use utils::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, GREP_MAX_LINE_LENGTH, LineTruncationResult,
    ShellCaptureOptions, ShellCaptureResult, TruncationLimit, TruncationOptions, TruncationResult,
    execute_shell_with_capture, format_size, sanitize_binary_output, truncate_head, truncate_line,
    truncate_tail,
};

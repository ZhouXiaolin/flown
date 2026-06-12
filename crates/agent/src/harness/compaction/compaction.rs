use crate::harness::session::types::*;
use crate::harness::types::{CompactionError, CompactionErrorCode};
use crate::types::AgentMessage;
use flown_ai::types::*;

/// Compaction settings.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            reserve_tokens: 16384,
            keep_recent_tokens: 20000,
        }
    }
}

pub const DEFAULT_COMPACTION_SETTINGS: CompactionSettings = CompactionSettings {
    enabled: true,
    reserve_tokens: 16384,
    keep_recent_tokens: 20000,
};

/// File operations tracked during compaction preparation.
#[derive(Debug, Clone, Default)]
pub struct FileOperations {
    pub read: std::collections::HashSet<String>,
    pub written: std::collections::HashSet<String>,
    pub edited: std::collections::HashSet<String>,
}

/// Estimated context-token usage breakdown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    pub tokens: u64,
    pub usage_tokens: u64,
    pub trailing_tokens: u64,
    pub last_usage_index: Option<usize>,
}

/// Compaction preparation result.
#[derive(Debug, Clone)]
pub struct CompactionPreparation {
    pub first_kept_entry_id: String,
    pub messages_to_summarize: Vec<SessionTreeEntry>,
    pub turn_prefix_messages: Vec<SessionTreeEntry>,
    pub is_split_turn: bool,
    pub tokens_before: u64,
    pub previous_summary: Option<String>,
    pub file_ops: FileOperations,
    pub settings: CompactionSettings,
    pub first_kept_entry_index: usize,
}

/// Compaction result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompactionResult {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: u64,
    pub details: Option<serde_json::Value>,
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

/// Cut point selected for compaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutPoint {
    pub first_kept_entry_index: usize,
    pub first_kept_entry_id: String,
    pub turn_start_index: usize,
    pub is_split_turn: bool,
}

pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens > 0 {
        usage.total_tokens as u64
    } else {
        (usage.input + usage.output + usage.cache_read + usage.cache_write) as u64
    }
}

pub fn get_last_assistant_usage(entries: &[SessionTreeEntry]) -> Option<Usage> {
    entries.iter().rev().find_map(|entry| match entry {
        SessionTreeEntry::Message {
            message: SessionMessage(AgentMessage::Assistant(msg)),
            ..
        } if msg.stop_reason != StopReason::Aborted && msg.stop_reason != StopReason::Error => {
            Some(msg.usage.clone())
        }
        _ => None,
    })
}

pub fn estimate_tokens(message: &AgentMessage) -> u64 {
    match message {
        AgentMessage::User(msg) => match &msg.content {
            MessageContent::Text(t) => ((t.len() as u64) + 3) / 4,
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .map(|block| match block {
                    UserContentBlock::Text(t) => ((t.text.len() as u64) + 3) / 4,
                    UserContentBlock::Image(_) => 4800,
                })
                .sum(),
        },
        AgentMessage::Assistant(msg) => msg
            .content
            .iter()
            .map(|content| match content {
                AssistantContent::Text(t) => ((t.text.len() as u64) + 3) / 4,
                AssistantContent::Thinking(t) => ((t.thinking.len() as u64) + 3) / 4,
                AssistantContent::ToolCall(tc) => {
                    ((tc.name.len() as u64) + 3) / 4
                        + ((tc.arguments.to_string().len() as u64) + 3) / 4
                }
            })
            .sum(),
        AgentMessage::ToolResult(msg) => msg
            .content
            .iter()
            .map(|content| match content {
                ToolResultContent::Text(t) => ((t.text.len() as u64) + 3) / 4,
                ToolResultContent::Image(_) => 4800,
            })
            .sum(),
        AgentMessage::Custom(msg) => ((msg.content.len() as u64) + 3) / 4,
    }
}

/// Estimate tokens for one session entry.
pub fn estimate_message_tokens(entry: &SessionTreeEntry) -> u64 {
    match entry {
        SessionTreeEntry::Message { message, .. } => estimate_tokens(&message.0),
        SessionTreeEntry::BranchSummary { summary, .. } => ((summary.len() as u64) + 3) / 4,
        SessionTreeEntry::CustomMessage { content, .. } => match content {
            CustomMessageContent::Text(t) => ((t.len() as u64) + 3) / 4,
            CustomMessageContent::Blocks(blocks) => blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text(t) => ((t.text.len() as u64) + 3) / 4,
                    ContentBlock::Image(_) => 4800,
                })
                .sum(),
        },
        _ => 0,
    }
}

pub fn estimate_context_usage(entries: &[SessionTreeEntry]) -> ContextUsageEstimate {
    let mut last_usage_index = None;
    let mut usage_tokens = 0;

    for (index, entry) in entries.iter().enumerate().rev() {
        if let SessionTreeEntry::Message {
            message: SessionMessage(AgentMessage::Assistant(msg)),
            ..
        } = entry
        {
            if msg.stop_reason != StopReason::Aborted
                && msg.stop_reason != StopReason::Error
                && calculate_context_tokens(&msg.usage) > 0
            {
                last_usage_index = Some(index);
                usage_tokens = calculate_context_tokens(&msg.usage);
                break;
            }
        }
    }

    if let Some(index) = last_usage_index {
        let trailing_tokens = entries[index + 1..]
            .iter()
            .map(estimate_message_tokens)
            .sum();
        ContextUsageEstimate {
            tokens: usage_tokens + trailing_tokens,
            usage_tokens,
            trailing_tokens,
            last_usage_index,
        }
    } else {
        let trailing_tokens = entries.iter().map(estimate_message_tokens).sum();
        ContextUsageEstimate {
            tokens: trailing_tokens,
            usage_tokens: 0,
            trailing_tokens,
            last_usage_index: None,
        }
    }
}

/// Estimate total context tokens.
pub fn estimate_context_tokens(entries: &[SessionTreeEntry]) -> u64 {
    estimate_context_usage(entries).tokens
}

pub fn should_compact(
    context_tokens: u64,
    context_window: u64,
    settings: &CompactionSettings,
) -> bool {
    settings.enabled && context_tokens > context_window.saturating_sub(settings.reserve_tokens)
}

pub fn find_turn_start_index(
    entries: &[SessionTreeEntry],
    entry_index: usize,
    start_index: usize,
) -> isize {
    for index in (start_index..=entry_index).rev() {
        match &entries[index] {
            SessionTreeEntry::BranchSummary { .. } | SessionTreeEntry::CustomMessage { .. } => {
                return index as isize;
            }
            SessionTreeEntry::Message {
                message: SessionMessage(AgentMessage::User(_)),
                ..
            } => return index as isize,
            _ => {}
        }
    }
    -1
}

pub fn find_cut_point(
    entries: &[SessionTreeEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPoint {
    let valid_cut_points: Vec<usize> = entries[start_index..end_index]
        .iter()
        .enumerate()
        .filter_map(|(offset, entry)| {
            let index = start_index + offset;
            match entry {
                SessionTreeEntry::Message {
                    message: SessionMessage(AgentMessage::User(_)),
                    ..
                }
                | SessionTreeEntry::Message {
                    message: SessionMessage(AgentMessage::Assistant(_)),
                    ..
                }
                | SessionTreeEntry::BranchSummary { .. }
                | SessionTreeEntry::CustomMessage { .. } => Some(index),
                _ => None,
            }
        })
        .collect();

    if valid_cut_points.is_empty() {
        return CutPoint {
            first_kept_entry_index: start_index,
            first_kept_entry_id: entries[start_index].id().to_string(),
            turn_start_index: start_index,
            is_split_turn: false,
        };
    }

    let mut accumulated = 0u64;
    let mut cut_index = valid_cut_points[0];
    for index in (start_index..end_index).rev() {
        if !matches!(entries[index], SessionTreeEntry::Message { .. }) {
            continue;
        }
        accumulated += estimate_message_tokens(&entries[index]);
        if accumulated >= keep_recent_tokens {
            if let Some(valid) = valid_cut_points
                .iter()
                .copied()
                .find(|point| *point >= index)
            {
                cut_index = valid;
            }
            break;
        }
    }

    while cut_index > start_index {
        match &entries[cut_index - 1] {
            SessionTreeEntry::Compaction { .. } => break,
            SessionTreeEntry::Message { .. } => break,
            _ => cut_index -= 1,
        }
    }

    let is_user_message = matches!(
        &entries[cut_index],
        SessionTreeEntry::Message {
            message: SessionMessage(AgentMessage::User(_)),
            ..
        }
    );
    let turn_start_index = if is_user_message {
        cut_index
    } else {
        let index = find_turn_start_index(entries, cut_index, start_index);
        if index >= 0 {
            index as usize
        } else {
            cut_index
        }
    };

    CutPoint {
        first_kept_entry_index: cut_index,
        first_kept_entry_id: entries[cut_index].id().to_string(),
        turn_start_index,
        is_split_turn: !is_user_message && turn_start_index != cut_index,
    }
}

pub fn prepare_compaction(
    entries: &[SessionTreeEntry],
    settings: &CompactionSettings,
) -> Result<Option<CompactionPreparation>, CompactionError> {
    if entries.is_empty() || matches!(entries.last(), Some(SessionTreeEntry::Compaction { .. })) {
        return Ok(None);
    }

    let mut previous_summary = None;
    let mut boundary_start = 0usize;
    let mut file_ops = FileOperations::default();

    if let Some(prev_index) = entries
        .iter()
        .rposition(|entry| matches!(entry, SessionTreeEntry::Compaction { .. }))
    {
        if let SessionTreeEntry::Compaction {
            summary,
            first_kept_entry_id,
            details,
            from_hook,
            ..
        } = &entries[prev_index]
        {
            previous_summary = Some(summary.clone());
            boundary_start = entries
                .iter()
                .position(|entry| entry.id() == first_kept_entry_id)
                .unwrap_or(prev_index + 1);
            if from_hook != &Some(true) {
                if let Some(details) = details {
                    if let Some(read_files) =
                        details.get("readFiles").and_then(|value| value.as_array())
                    {
                        for file in read_files {
                            if let Some(file) = file.as_str() {
                                file_ops.read.insert(file.to_string());
                            }
                        }
                    }
                    if let Some(modified_files) = details
                        .get("modifiedFiles")
                        .and_then(|value| value.as_array())
                    {
                        for file in modified_files {
                            if let Some(file) = file.as_str() {
                                file_ops.edited.insert(file.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    let cut = find_cut_point(
        entries,
        boundary_start,
        entries.len(),
        settings.keep_recent_tokens,
    );
    let first_kept_entry = entries.get(cut.first_kept_entry_index).ok_or_else(|| {
        CompactionError::new(
            CompactionErrorCode::InvalidSession,
            "First kept entry index is out of bounds",
        )
    })?;

    if first_kept_entry.id().is_empty() {
        return Err(CompactionError::new(
            CompactionErrorCode::InvalidSession,
            "First kept entry has no UUID - session may need migration",
        ));
    }

    let history_end = if cut.is_split_turn {
        cut.turn_start_index
    } else {
        cut.first_kept_entry_index
    };
    let messages_to_summarize = entries[boundary_start..history_end].to_vec();
    let turn_prefix_messages = if cut.is_split_turn {
        entries[cut.turn_start_index..cut.first_kept_entry_index].to_vec()
    } else {
        Vec::new()
    };

    let extracted_file_ops = extract_file_operations(&messages_to_summarize);
    file_ops.read.extend(extracted_file_ops.read);
    file_ops.written.extend(extracted_file_ops.written);
    file_ops.edited.extend(extracted_file_ops.edited);

    for entry in &turn_prefix_messages {
        merge_file_ops_from_entry(entry, &mut file_ops);
    }

    Ok(Some(CompactionPreparation {
        first_kept_entry_id: cut.first_kept_entry_id.clone(),
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn: cut.is_split_turn,
        tokens_before: estimate_context_tokens(entries),
        previous_summary,
        file_ops,
        settings: settings.clone(),
        first_kept_entry_index: cut.first_kept_entry_index,
    }))
}

pub fn serialize_conversation(entries: &[SessionTreeEntry]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for entry in entries {
        match entry {
            SessionTreeEntry::Message { message, .. } => match &message.0 {
                AgentMessage::User(msg) => {
                    let text = match &msg.content {
                        MessageContent::Text(t) => t.clone(),
                        MessageContent::Blocks(blocks) => blocks
                            .iter()
                            .filter_map(|block| match block {
                                UserContentBlock::Text(t) => Some(t.text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(""),
                    };
                    if !text.is_empty() {
                        parts.push(format!("[User]: {text}"));
                    }
                }
                AgentMessage::Assistant(msg) => {
                    // Aligned with pi-mono: collect thinking, text, and toolCalls separately
                    let mut thinking_parts: Vec<String> = Vec::new();
                    let mut text_parts: Vec<String> = Vec::new();
                    let mut tool_calls: Vec<String> = Vec::new();

                    for content in &msg.content {
                        match content {
                            AssistantContent::Text(t) => {
                                text_parts.push(t.text.clone());
                            }
                            AssistantContent::Thinking(t) => {
                                thinking_parts.push(t.thinking.clone());
                            }
                            AssistantContent::ToolCall(tc) => {
                                let args_str = tc
                                    .arguments
                                    .as_object()
                                    .map(|obj| {
                                        obj.iter()
                                            .map(|(k, v)| format!("{}={}", k, safe_json_stringify(v)))
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    })
                                    .unwrap_or_else(|| tc.arguments.to_string());
                                tool_calls.push(format!("{}({})", tc.name, args_str));
                            }
                        }
                    }

                    if !thinking_parts.is_empty() {
                        parts.push(format!("[Assistant thinking]: {}", thinking_parts.join("\n")));
                    }
                    if !text_parts.is_empty() {
                        parts.push(format!("[Assistant]: {}", text_parts.join("\n")));
                    }
                    if !tool_calls.is_empty() {
                        parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                    }
                }
                AgentMessage::ToolResult(msg) => {
                    let text = msg
                        .content
                        .iter()
                        .filter_map(|content| match content {
                            ToolResultContent::Text(t) => Some(t.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    if !text.is_empty() {
                        parts.push(format!("[Tool result]: {}", truncate_for_summary(&text, 2000)));
                    }
                }
                AgentMessage::Custom(msg) => {
                    parts.push(format!("[{}]: {}", msg.custom_type, msg.content));
                }
            },
            SessionTreeEntry::BranchSummary { summary, .. } => {
                parts.push(format!("[Branch Summary]: {summary}"));
            }
            SessionTreeEntry::CustomMessage { content, .. } => {
                let text = match content {
                    CustomMessageContent::Text(t) => t.clone(),
                    CustomMessageContent::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text(t) => Some(t.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(""),
                };
                if !text.is_empty() {
                    parts.push(format!("[Custom]: {text}"));
                }
            }
            _ => {}
        }
    }
    parts.join("\n\n")
}

fn safe_json_stringify(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[unserializable]".to_string())
}

fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let truncated_chars = text.len() - max_chars;
    format!(
        "{}\n\n[... {} more characters truncated]",
        &text[..max_chars],
        truncated_chars
    )
}

pub fn extract_file_ops(entries: &[SessionTreeEntry]) -> (Vec<String>, Vec<String>) {
    let file_ops = extract_file_operations(entries);
    compute_file_lists(&file_ops)
}

pub fn build_file_ops_tag(read_files: &[String], modified_files: &[String]) -> String {
    if read_files.is_empty() && modified_files.is_empty() {
        return String::new();
    }

    let mut sections: Vec<String> = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        return String::new();
    }
    format!("\n\n{}", sections.join("\n\n"))
}

const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";
const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.\n\nUse this EXACT format:\n\n## Goal\n[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]\n\n## Constraints & Preferences\n- [Any constraints, preferences, or requirements mentioned by user]\n- [Or \"(none)\" if none were mentioned]\n\n## Progress\n### Done\n- [x] [Completed tasks/changes]\n\n### In Progress\n- [ ] [Current work]\n\n### Blocked\n- [Issues preventing progress, if any]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale]\n\n## Next Steps\n1. [Ordered list of what should happen next]\n\n## Critical Context\n- [Any data, examples, or references needed to continue]\n- [Or \"(none)\" if not applicable]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";
const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.\n\nUpdate the existing structured summary with new information. RULES:\n- PRESERVE all existing information from the previous summary\n- ADD new progress, decisions, and context from the new messages\n- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed\n- UPDATE \"Next Steps\" based on what was accomplished\n- PRESERVE exact file paths, function names, and error messages\n- If something is no longer relevant, you may remove it\n\nUse this EXACT format:\n\n## Goal\n[Preserve existing goals, add new ones if the task expanded]\n\n## Constraints & Preferences\n- [Preserve existing, add new ones discovered]\n\n## Progress\n### Done\n- [x] [Include previously done items AND newly completed items]\n\n### In Progress\n- [ ] [Current work - update based on progress]\n\n### Blocked\n- [Current blockers - remove if resolved]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale] (preserve all previous, add new)\n\n## Next Steps\n1. [Update based on current state]\n\n## Critical Context\n- [Preserve important context, add new if needed]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";
const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = "This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.\n\nSummarize the prefix to provide context for the retained suffix:\n\n## Original Request\n[What did the user ask for in this turn?]\n\n## Early Progress\n- [Key decisions and work done in the prefix]\n\n## Context for Suffix\n- [Information needed to understand the retained recent work]\n\nBe concise. Focus on what's needed to understand the kept suffix.";

pub async fn generate_summary(
    messages_to_summarize: &[SessionTreeEntry],
    model: &Model,
    api_key: &str,
    headers: Option<&std::collections::HashMap<String, String>>,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    thinking_level: Option<&ThinkingLevel>,
    signal: Option<AbortSignal>,
    reserve_tokens: u64,
) -> Result<String, CompactionError> {
    use flown_ai::complete_simple;

    // Aligned with pi-mono: maxTokens = min(0.8 * reserveTokens, model.maxTokens)
    let max_tokens = (reserve_tokens as f64 * 0.8).min(model.max_tokens as f64) as u32;

    let conversation = serialize_conversation(messages_to_summarize);
    let (read_files, modified_files) = extract_file_ops(messages_to_summarize);
    let mut prompt_text = format!(
        "<conversation>\n{}{}\n</conversation>\n\n",
        conversation,
        build_file_ops_tag(&read_files, &modified_files)
    );
    if let Some(previous_summary) = previous_summary {
        prompt_text.push_str(&format!(
            "<previous-summary>\n{}\n</previous-summary>\n\n",
            previous_summary
        ));
    }
    prompt_text.push_str(if previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT
    } else {
        SUMMARIZATION_PROMPT
    });
    if let Some(custom_instructions) = custom_instructions {
        prompt_text.push_str(&format!("\n\nAdditional focus: {custom_instructions}"));
    }

    let llm_messages = vec![Message::User(UserMessage {
        role: "user".to_string(),
        content: MessageContent::Text(prompt_text),
        timestamp: chrono::Utc::now(),
    })];

    let context = Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_string()),
        messages: llm_messages,
        tools: None,
    };

    let mut options = SimpleStreamOptions::default();
    options.base.max_tokens = Some(max_tokens);
    options.base.api_key = Some(api_key.to_string());
    options.base.signal = signal;
    if let Some(headers) = headers {
        options.base.headers = Some(headers.clone());
    }
    if let Some(level) = thinking_level {
        if *level != ThinkingLevel::Off {
            options.reasoning = Some(level.clone());
        }
    }

    let response = complete_simple(model, &context, Some(&options)).await;
    match response.stop_reason {
        StopReason::Aborted => Err(CompactionError::new(
            CompactionErrorCode::Aborted,
            response
                .error_message
                .unwrap_or_else(|| "Summarization aborted".to_string()),
        )),
        StopReason::Error => Err(CompactionError::new(
            CompactionErrorCode::SummarizationFailed,
            format!(
                "Summarization failed: {}",
                response
                    .error_message
                    .unwrap_or_else(|| "Unknown error".to_string())
            ),
        )),
        _ => Ok(response
            .content
            .iter()
            .filter_map(|content| match content {
                AssistantContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")),
    }
}

pub async fn generate_turn_prefix_summary(
    turn_prefix_messages: &[SessionTreeEntry],
    model: &Model,
    api_key: &str,
    headers: Option<&std::collections::HashMap<String, String>>,
    thinking_level: Option<&ThinkingLevel>,
    signal: Option<AbortSignal>,
    reserve_tokens: u64,
) -> Result<String, CompactionError> {
    use flown_ai::complete_simple;

    // Aligned with pi-mono: maxTokens = min(0.5 * reserveTokens, model.maxTokens)
    let max_tokens = (reserve_tokens as f64 * 0.5).min(model.max_tokens as f64) as u32;

    let prompt_text = format!(
        "<conversation>\n{}\n</conversation>\n\n{}",
        serialize_conversation(turn_prefix_messages),
        TURN_PREFIX_SUMMARIZATION_PROMPT
    );

    let llm_messages = vec![Message::User(UserMessage {
        role: "user".to_string(),
        content: MessageContent::Text(prompt_text),
        timestamp: chrono::Utc::now(),
    })];

    let context = Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_string()),
        messages: llm_messages,
        tools: None,
    };

    let mut options = SimpleStreamOptions::default();
    options.base.max_tokens = Some(max_tokens);
    options.base.api_key = Some(api_key.to_string());
    options.base.signal = signal;
    if let Some(headers) = headers {
        options.base.headers = Some(headers.clone());
    }
    if let Some(level) = thinking_level {
        if *level != ThinkingLevel::Off {
            options.reasoning = Some(level.clone());
        }
    }

    let response = complete_simple(model, &context, Some(&options)).await;
    match response.stop_reason {
        StopReason::Aborted => Err(CompactionError::new(
            CompactionErrorCode::Aborted,
            response
                .error_message
                .unwrap_or_else(|| "Turn prefix summarization aborted".to_string()),
        )),
        StopReason::Error => Err(CompactionError::new(
            CompactionErrorCode::SummarizationFailed,
            format!(
                "Turn prefix summarization failed: {}",
                response
                    .error_message
                    .unwrap_or_else(|| "Unknown error".to_string())
            ),
        )),
        _ => Ok(response
            .content
            .iter()
            .filter_map(|content| match content {
                AssistantContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")),
    }
}

pub async fn compact_with_llm(
    preparation: &CompactionPreparation,
    model: &Model,
    api_key: &str,
    headers: Option<&std::collections::HashMap<String, String>>,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    thinking_level: Option<&ThinkingLevel>,
    signal: Option<AbortSignal>,
) -> Result<CompactionResult, CompactionError> {
    if preparation.first_kept_entry_id.is_empty() {
        return Err(CompactionError::new(
            CompactionErrorCode::InvalidSession,
            "First kept entry has no UUID - session may need migration",
        ));
    }

    let summary = if preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty() {
        let (history_result, prefix_result) = futures::join!(
            async {
                if preparation.messages_to_summarize.is_empty() {
                    Ok("No prior history.".to_string())
                } else {
                    generate_summary(
                        &preparation.messages_to_summarize,
                        model,
                        api_key,
                        headers,
                        custom_instructions,
                        previous_summary.or(preparation.previous_summary.as_deref()),
                        thinking_level,
                        signal.clone(),
                        preparation.settings.reserve_tokens,
                    )
                    .await
                }
            },
            generate_turn_prefix_summary(
                &preparation.turn_prefix_messages,
                model,
                api_key,
                headers,
                thinking_level,
                signal.clone(),
                preparation.settings.reserve_tokens,
            )
        );
        format!(
            "{}\n\n---\n\n**Turn Context (split turn):**\n\n{}",
            history_result?, prefix_result?
        )
    } else {
        generate_summary(
            &preparation.messages_to_summarize,
            model,
            api_key,
            headers,
            custom_instructions,
            previous_summary.or(preparation.previous_summary.as_deref()),
            thinking_level,
            signal,
            preparation.settings.reserve_tokens,
        )
        .await?
    };

    let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
    let details = serde_json::json!({
        "readFiles": read_files,
        "modifiedFiles": modified_files,
    });
    Ok(CompactionResult {
        summary: format!(
            "{}{}",
            summary,
            build_file_ops_tag(
                details["readFiles"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                    .collect::<Vec<_>>()
                    .as_slice(),
                details["modifiedFiles"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                    .collect::<Vec<_>>()
                    .as_slice(),
            )
        ),
        first_kept_entry_id: preparation.first_kept_entry_id.clone(),
        tokens_before: preparation.tokens_before,
        details: Some(details.clone()),
        read_files: details["readFiles"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|value| value.as_str().map(ToOwned::to_owned))
            .collect(),
        modified_files: details["modifiedFiles"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|value| value.as_str().map(ToOwned::to_owned))
            .collect(),
    })
}

pub async fn compact(
    preparation: &CompactionPreparation,
    model: &Model,
    api_key: &str,
    headers: Option<&std::collections::HashMap<String, String>>,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    thinking_level: Option<&ThinkingLevel>,
    signal: Option<AbortSignal>,
) -> Result<CompactionResult, CompactionError> {
    compact_with_llm(
        preparation,
        model,
        api_key,
        headers,
        custom_instructions,
        previous_summary,
        thinking_level,
        signal,
    )
    .await
}

fn extract_file_operations(entries: &[SessionTreeEntry]) -> FileOperations {
    let mut file_ops = FileOperations::default();
    for entry in entries {
        merge_file_ops_from_entry(entry, &mut file_ops);
    }
    file_ops
}

fn merge_file_ops_from_entry(entry: &SessionTreeEntry, file_ops: &mut FileOperations) {
    if let SessionTreeEntry::Message {
        message: SessionMessage(AgentMessage::Assistant(msg)),
        ..
    } = entry
    {
        for content in &msg.content {
            if let AssistantContent::ToolCall(tc) = content {
                if let Some(path) = tc.arguments.get("path").and_then(|value| value.as_str()) {
                    match tc.name.as_str() {
                        "read" | "read_file" => {
                            file_ops.read.insert(path.to_string());
                        }
                        "write" | "write_file" => {
                            file_ops.written.insert(path.to_string());
                        }
                        "edit" | "apply_diff" => {
                            file_ops.edited.insert(path.to_string());
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

fn compute_file_lists(file_ops: &FileOperations) -> (Vec<String>, Vec<String>) {
    let modified: std::collections::HashSet<_> =
        file_ops.written.union(&file_ops.edited).cloned().collect();
    let mut read_files: Vec<String> = file_ops.read.difference(&modified).cloned().collect();
    let mut modified_files: Vec<String> = modified.into_iter().collect();
    read_files.sort();
    modified_files.sort();
    (read_files, modified_files)
}

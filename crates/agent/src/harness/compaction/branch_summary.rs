use super::compaction::{estimate_message_tokens, estimate_tokens, extract_file_ops, serialize_conversation};
use crate::harness::types::*;
use crate::harness::{BranchSummaryError, BranchSummaryErrorCode};
use crate::harness::session::{
    ContentBlock, CustomMessageContent, SessionMessage, SessionTreeEntry,
};
use crate::types::AgentMessage;
use flown_ai::{
    AbortSignal, AssistantContent, Context, Message, MessageContent, Model, SimpleStreamOptions,
    StopReason, ThinkingLevel, ToolResultContent, Usage, UserMessage,
};

/// File-operation details stored on generated branch summary entries.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct BranchSummaryDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

/// Prepared branch content for summarization.
#[derive(Debug, Clone, Default)]
pub struct BranchPreparation {
    pub messages: Vec<AgentMessage>,
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
    pub total_tokens: u64,
}

/// Entries selected for branch summarization.
#[derive(Debug, Clone, Default)]
pub struct CollectEntriesResult {
    pub entries: Vec<SessionTreeEntry>,
    pub common_ancestor_id: Option<String>,
}

/// Collect entries for branch summary
pub fn collect_entries_for_branch_summary(
    entries: &[SessionTreeEntry],
    old_leaf_id: &str,
    target_id: &str,
) -> Vec<SessionTreeEntry> {
    // Build path set for old leaf
    let old_path = get_path_set(entries, old_leaf_id);
    let target_path = get_path(entries, target_id);

    // Find common ancestor
    let common_ancestor = target_path.iter().rev().find(|id| old_path.contains(*id));

    let common_ancestor = match common_ancestor {
        Some(id) => id.clone(),
        None => return Vec::new(),
    };

    // Collect entries from old leaf to common ancestor
    let mut result = Vec::new();
    let mut current = old_leaf_id.to_string();

    while current != common_ancestor {
        if let Some(entry) = entries.iter().find(|e| e.id() == current) {
            result.push(entry.clone());
            current = entry.parent_id().unwrap_or("").to_string();
        } else {
            break;
        }
    }

    result.reverse(); // chronological order
    result
}

/// Public alias that exposes the pi-mono branch-summary collection shape.
pub fn collect_entries_for_branch_summary_result(
    entries: &[SessionTreeEntry],
    old_leaf_id: &str,
    target_id: &str,
) -> CollectEntriesResult {
    let old_path = get_path_set(entries, old_leaf_id);
    let target_path = get_path(entries, target_id);
    let common_ancestor_id = target_path
        .iter()
        .rev()
        .find(|id| old_path.contains(*id))
        .cloned();

    CollectEntriesResult {
        entries: collect_entries_for_branch_summary(entries, old_leaf_id, target_id),
        common_ancestor_id,
    }
}

/// Branch summary result
#[derive(Debug, Clone)]
pub struct BranchSummaryResult {
    pub summary: String,
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

/// Branch summary generation options
#[derive(Debug, Clone)]
pub struct GenerateBranchSummaryOptions {
    pub model: Model,
    pub api_key: String,
    pub headers: Option<std::collections::HashMap<String, String>>,
    pub signal: Option<AbortSignal>,
    pub custom_instructions: Option<String>,
    pub replace_instructions: bool,
    pub reserve_tokens: u64,
}

/// Summarization system prompt
const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

/// Branch summary prompt
const BRANCH_SUMMARY_PROMPT: &str = "Create a structured summary of this conversation branch for context when returning later.\n\nUse this EXACT format:\n\n## Goal\n[What was the user trying to accomplish in this branch?]\n\n## Constraints & Preferences\n- [Any constraints, preferences, or requirements mentioned]\n- [Or \"(none)\" if none were mentioned]\n\n## Progress\n### Done\n- [x] [Completed tasks/changes]\n\n### In Progress\n- [ ] [Work that was started but not finished]\n\n### Blocked\n- [Issues preventing progress, if any]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale]\n\n## Next Steps\n1. [What should happen next to continue this work]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

/// Branch summary preamble
const BRANCH_SUMMARY_PREAMBLE: &str = "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n";

/// Internal tuple-based helper for branch summarization preparation.
fn prepare_branch_entries_tuple(
    entries: &[SessionTreeEntry],
    token_budget: u64,
) -> (Vec<AgentMessage>, Vec<String>, Vec<String>) {
    let mut messages = Vec::new();
    let mut total_tokens = 0u64;

    // Process entries in reverse to prioritize recent ones
    for entry in entries.iter().rev() {
        let msg = match entry {
            SessionTreeEntry::Message {
                message: SessionMessage(msg),
                ..
            } => {
                // Skip tool results
                if matches!(msg, AgentMessage::ToolResult(_)) {
                    continue;
                }
                Some(msg.clone())
            }
            SessionTreeEntry::BranchSummary { summary, .. } => {
                Some(AgentMessage::User(UserMessage {
                    role: "user".to_string(),
                    content: MessageContent::Text(format!("Branch summary: {}", summary)),
                    timestamp: chrono::Utc::now(),
                }))
            }
            SessionTreeEntry::CustomMessage {
                content, display, ..
            } => {
                if *display {
                    let text = match content {
                        CustomMessageContent::Text(t) => t.clone(),
                        CustomMessageContent::Blocks(blocks) => blocks
                            .iter()
                            .filter_map(|b| match b {
                                ContentBlock::Text(t) => Some(t.text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    Some(AgentMessage::User(UserMessage {
                        role: "user".to_string(),
                        content: MessageContent::Text(text),
                        timestamp: chrono::Utc::now(),
                    }))
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(msg) = msg {
            let tokens = estimate_message_tokens(entry);
            if token_budget > 0 && total_tokens + tokens > token_budget {
                // Only include compaction/branch_summary if we're under 90% of budget
                if matches!(
                    entry,
                    SessionTreeEntry::Compaction { .. } | SessionTreeEntry::BranchSummary { .. }
                ) {
                    if total_tokens < token_budget * 9 / 10 {
                        messages.insert(0, msg);
                    }
                }
                break;
            }
            messages.insert(0, msg);
            total_tokens += tokens;
        }
    }

    let (read_files, modified_files) = extract_file_ops(entries);
    (messages, read_files, modified_files)
}

/// Prepare branch entries for summarization within a token budget.
pub fn prepare_branch_entries(
    entries: &[SessionTreeEntry],
    token_budget: u64,
) -> BranchPreparation {
    let (messages, read_files, modified_files) = prepare_branch_entries_tuple(entries, token_budget);
    let total_tokens = messages
        .iter()
        .map(estimate_tokens)
        .sum();
    BranchPreparation {
        messages,
        read_files,
        modified_files,
        total_tokens,
    }
}

/// Generate branch summary using LLM
pub async fn generate_branch_summary_with_llm(
    entries: &[SessionTreeEntry],
    options: &GenerateBranchSummaryOptions,
) -> Result<BranchSummaryResult, BranchSummaryError> {
    use flown_ai::complete_simple;

    let context_window = options.model.context_window as u64; // u32 → u64
    let token_budget = context_window - options.reserve_tokens;

    let BranchPreparation {
        messages,
        read_files,
        modified_files,
        ..
    } = prepare_branch_entries(entries, token_budget);

    if messages.is_empty() {
        return Ok(BranchSummaryResult {
            summary: "No content to summarize".to_string(),
            read_files,
            modified_files,
        });
    }

    let conversation = serialize_conversation(
        &messages
            .iter()
            .map(|m| SessionTreeEntry::Message {
                id: String::new(),
                parent_id: None,
                timestamp: chrono::Utc::now().to_rfc3339(),
                message: SessionMessage(m.clone()),
            })
            .collect::<Vec<_>>(),
    );

    let instructions = if options.replace_instructions && options.custom_instructions.is_some() {
        options.custom_instructions.clone().unwrap()
    } else if let Some(custom) = &options.custom_instructions {
        format!("{}\n\nAdditional focus: {}", BRANCH_SUMMARY_PROMPT, custom)
    } else {
        BRANCH_SUMMARY_PROMPT.to_string()
    };

    let prompt_text = format!(
        "<conversation>\n{}\n</conversation>\n\n{}",
        conversation, instructions
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

    let mut stream_options = SimpleStreamOptions::default();
    stream_options.base.max_tokens = Some(2048); // Aligned with pi-mono
    if let Some(headers) = &options.headers {
        stream_options.base.headers = Some(headers.clone());
    }
    stream_options.base.api_key = Some(options.api_key.clone());
    stream_options.base.signal = options.signal.clone();

    let response = complete_simple(&options.model, &context, Some(&stream_options))
        .await
        .map_err(|err| {
            BranchSummaryError::new(
                BranchSummaryErrorCode::SummarizationFailed,
                format!("Branch summary failed: {err}"),
            )
        })?;

    match response.stop_reason {
        StopReason::Aborted => Err(BranchSummaryError::new(
            BranchSummaryErrorCode::Aborted,
            response
                .error_message
                .unwrap_or_else(|| "Branch summary aborted".to_string()),
        )),
        StopReason::Error => Err(BranchSummaryError::new(
            BranchSummaryErrorCode::SummarizationFailed,
            format!(
                "Branch summary failed: {}",
                response
                    .error_message
                    .unwrap_or_else(|| "Unknown error".to_string())
            ),
        )),
        _ => {
            let text: String = response
                .content
                .iter()
                .filter_map(|c| {
                    if let AssistantContent::Text(t) = c {
                        Some(t.text.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");

            let summary = format!("{}{}", BRANCH_SUMMARY_PREAMBLE, text);

            Ok(BranchSummaryResult {
                summary,
                read_files,
                modified_files,
            })
        }
    }
}

/// Pi-mono-aligned alias for the public branch-summary generator.
pub async fn generate_branch_summary(
    entries: &[SessionTreeEntry],
    options: &GenerateBranchSummaryOptions,
) -> Result<BranchSummaryResult, BranchSummaryError> {
    generate_branch_summary_with_llm(entries, options).await
}

/// Get path from entry to root as a set of IDs
fn get_path_set(entries: &[SessionTreeEntry], leaf_id: &str) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    let mut current = leaf_id.to_string();

    loop {
        set.insert(current.clone());
        if let Some(entry) = entries.iter().find(|e| e.id() == current) {
            if let Some(parent) = entry.parent_id() {
                current = parent.to_string();
            } else {
                break;
            }
        } else {
            break;
        }
    }

    set
}

/// Get path from entry to root as ordered list
fn get_path(entries: &[SessionTreeEntry], leaf_id: &str) -> Vec<String> {
    let mut path = Vec::new();
    let mut current = leaf_id.to_string();

    loop {
        path.push(current.clone());
        if let Some(entry) = entries.iter().find(|e| e.id() == current) {
            if let Some(parent) = entry.parent_id() {
                current = parent.to_string();
            } else {
                break;
            }
        } else {
            break;
        }
    }

    path.reverse();
    path
}

use crate::types::AgentMessage;
use flown_ai::{
    ImageContent, Message, MessageContent, TextContent, ToolCall, ToolResultContent,
    UserContentBlock, UserMessage,
};

/// Bash execution message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashExecutionMessage {
    pub role: String,
    pub command: String,
    pub output: String,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    pub full_output_path: Option<String>,
    pub timestamp: i64,
    pub exclude_from_context: Option<bool>,
}

/// Custom message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomMessage {
    pub role: String,
    pub custom_type: String,
    pub content: CustomMessageContent,
    pub display: bool,
    pub timestamp: i64,
}

/// Custom message content
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CustomMessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// Content block
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "contentType")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text(TextContent),
    #[serde(rename = "image")]
    Image(ImageContent),
}

/// Branch summary message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchSummaryMessage {
    pub role: String,
    pub summary: String,
    pub from_id: String,
    pub timestamp: i64,
}

/// Compaction summary message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionSummaryMessage {
    pub role: String,
    pub summary: String,
    pub tokens_before: u64,
    pub timestamp: i64,
}

/// Extended message type for harness
#[derive(Debug, Clone)]
pub enum HarnessMessage {
    Agent(AgentMessage),
    BashExecution(BashExecutionMessage),
    Custom(CustomMessage),
    BranchSummary(BranchSummaryMessage),
    CompactionSummary(CompactionSummaryMessage),
}

impl HarnessMessage {
    pub fn timestamp(&self) -> i64 {
        match self {
            Self::Agent(AgentMessage::User(m)) => m.timestamp.timestamp_millis(),
            Self::Agent(AgentMessage::Assistant(m)) => m.timestamp.timestamp_millis(),
            Self::Agent(AgentMessage::ToolResult(m)) => m.timestamp.timestamp_millis(),
            Self::Agent(AgentMessage::Custom(m)) => m.timestamp.timestamp_millis(),
            Self::BashExecution(m) => m.timestamp,
            Self::Custom(m) => m.timestamp,
            Self::BranchSummary(m) => m.timestamp,
            Self::CompactionSummary(m) => m.timestamp,
        }
    }
}

/// Convert harness messages to LLM messages
pub fn convert_to_llm(messages: &[HarnessMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|m| convert_single_to_llm(m))
        .collect()
}

fn convert_single_to_llm(message: &HarnessMessage) -> Option<Message> {
    match message {
        HarnessMessage::Agent(msg) => Some(match msg {
            AgentMessage::User(m) => Message::User(m.clone()),
            AgentMessage::Assistant(m) => Message::Assistant(m.clone()),
            AgentMessage::ToolResult(m) => Message::ToolResult(m.clone()),
            AgentMessage::Custom(m) => Message::User(UserMessage {
                role: "user".to_string(),
                content: MessageContent::Text(m.content.clone()),
                timestamp: m.timestamp,
            }),
        }),
        HarnessMessage::BashExecution(msg) => {
            if msg.exclude_from_context == Some(true) {
                return None;
            }
            Some(Message::User(UserMessage {
                role: "user".to_string(),
                content: MessageContent::Text(bash_execution_to_text(msg)),
                timestamp: chrono::Utc::now(),
            }))
        }
        HarnessMessage::Custom(msg) => {
            let content = match &msg.content {
                CustomMessageContent::Text(t) => MessageContent::Text(t.clone()),
                CustomMessageContent::Blocks(blocks) => {
                    let user_blocks: Vec<UserContentBlock> = blocks
                        .iter()
                        .map(|b| match b {
                            ContentBlock::Text(t) => UserContentBlock::Text(t.clone()),
                            ContentBlock::Image(i) => UserContentBlock::Image(i.clone()),
                        })
                        .collect();
                    MessageContent::Blocks(user_blocks)
                }
            };
            Some(Message::User(UserMessage {
                role: "user".to_string(),
                content,
                timestamp: chrono::Utc::now(),
            }))
        }
        HarnessMessage::BranchSummary(msg) => Some(Message::User(UserMessage {
            role: "user".to_string(),
            content: MessageContent::Text(format!(
                "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n{}\n</summary>",
                msg.summary
            )),
            timestamp: chrono::Utc::now(),
        })),
        HarnessMessage::CompactionSummary(msg) => Some(Message::User(UserMessage {
            role: "user".to_string(),
            content: MessageContent::Text(format!(
                "The conversation history before this point was compacted into the following summary:\n\n<summary>\n{}\n</summary>",
                msg.summary
            )),
            timestamp: chrono::Utc::now(),
        })),
    }
}

pub fn bash_execution_to_text(msg: &BashExecutionMessage) -> String {
    let mut text = format!("```bash\n{}\n```", msg.command);

    if !msg.output.is_empty() {
        text.push_str(&format!("\n\nOutput:\n```\n{}\n```", msg.output));
    }

    if let Some(exit_code) = msg.exit_code {
        text.push_str(&format!("\n\nExit code: {}", exit_code));
    }

    if msg.truncated {
        text.push_str("\n\n(Output was truncated)");
    }

    if msg.cancelled {
        text.push_str("\n\n(Command was cancelled)");
    }

    text
}

/// Create a branch summary message
pub fn create_branch_summary_message(summary: &str, from_id: &str) -> HarnessMessage {
    HarnessMessage::BranchSummary(BranchSummaryMessage {
        role: "branchSummary".to_string(),
        summary: summary.to_string(),
        from_id: from_id.to_string(),
        timestamp: chrono::Utc::now().timestamp_millis(),
    })
}

/// Create a compaction summary message
pub fn create_compaction_summary_message(summary: &str, tokens_before: u64) -> HarnessMessage {
    HarnessMessage::CompactionSummary(CompactionSummaryMessage {
        role: "compactionSummary".to_string(),
        summary: summary.to_string(),
        tokens_before,
        timestamp: chrono::Utc::now().timestamp_millis(),
    })
}

use serde::{Deserialize, Serialize};

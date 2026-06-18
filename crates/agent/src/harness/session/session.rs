use super::storage::SessionStorage;
use super::types::*;
use crate::types::{AgentMessage, CustomAgentMessage};
use flown_ai::{MessageContent, UserMessage};

/// Session wraps a SessionStorage and provides the tree-based transcript API
pub struct Session {
    storage: Box<dyn SessionStorage>,
}

impl Session {
    pub fn new(storage: Box<dyn SessionStorage>) -> Self {
        Self { storage }
    }

    pub fn storage(&self) -> &dyn SessionStorage {
        self.storage.as_ref()
    }

    pub async fn get_metadata(&self) -> &SessionMetadata {
        self.storage.metadata()
    }

    pub async fn get_leaf_id(&self) -> Option<String> {
        self.storage.get_leaf_id().await
    }

    pub async fn get_entry(&self, id: &str) -> Option<SessionTreeEntry> {
        self.storage.get_entry(id).await
    }

    pub async fn get_entries(&self) -> Vec<SessionTreeEntry> {
        self.storage.get_entries().await
    }

    /// Get the path from leaf (or given entry) to root
    pub async fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionTreeEntry> {
        let leaf_id = match from_id {
            Some(id) => Some(id.to_string()),
            None => self.storage.get_leaf_id().await,
        };
        self.storage.get_path_to_root(leaf_id.as_deref()).await
    }

    /// Build context from the current branch
    pub async fn build_context(&self) -> SessionContext {
        let entries = self.get_branch(None).await;
        build_session_context(&entries)
    }

    pub async fn copy_branch_to(
        &self,
        target: &Session,
        from_id: Option<&str>,
    ) -> Vec<SessionTreeEntry> {
        let entries = self.get_branch(from_id).await;
        for entry in &entries {
            target.storage().append_entry(entry.clone()).await;
        }
        entries
    }

    /// Materialize a blank derived session.
    pub async fn materialize_new(&self) -> Vec<SessionTreeEntry> {
        self.get_entries().await
    }

    /// Materialize a side branch from another session and append the follow-up prompt.
    pub async fn materialize_branch_from(
        &self,
        source: &Session,
        from_id: Option<&str>,
        follow_up_text: &str,
    ) -> Vec<SessionTreeEntry> {
        source.copy_branch_to(self, from_id).await;
        self.append_message(AgentMessage::User(UserMessage {
            role: "user".to_string(),
            content: MessageContent::Text(follow_up_text.to_string()),
            timestamp: chrono::Utc::now(),
        }))
        .await;
        self.get_entries().await
    }

    /// Materialize a continuation session that starts from a compaction summary.
    pub async fn materialize_compaction_continuation(
        &self,
        summary: &str,
    ) -> Vec<SessionTreeEntry> {
        self.append_message(AgentMessage::Custom(CustomAgentMessage {
            custom_type: "compaction_summary".to_string(),
            content: summary.to_string(),
            timestamp: chrono::Utc::now(),
        }))
        .await;
        self.get_entries().await
    }

    pub async fn get_label(&self, id: &str) -> Option<String> {
        self.storage.get_label(id).await
    }

    pub async fn get_session_name(&self) -> Option<String> {
        let entries = self.storage.find_entries("session_info").await;
        entries.iter().rev().find_map(|e| {
            if let SessionTreeEntry::SessionInfo { name, .. } = e {
                name.clone().filter(|n| !n.trim().is_empty())
            } else {
                None
            }
        })
    }

    /// Append a message entry
    pub async fn append_message(&self, message: AgentMessage) -> String {
        let entry_id = self.storage.create_entry_id();
        let parent_id = self.storage.get_leaf_id().await;
        let entry = SessionTreeEntry::Message {
            id: entry_id.clone(),
            parent_id,
            timestamp: super::storage::create_timestamp(),
            message: super::types::SessionMessage(message),
        };
        self.storage.append_entry(entry).await;
        entry_id
    }

    /// Append a thinking level change
    pub async fn append_thinking_level_change(&self, level: &str) -> String {
        let entry_id = self.storage.create_entry_id();
        let parent_id = self.storage.get_leaf_id().await;
        let entry = SessionTreeEntry::ThinkingLevelChange {
            id: entry_id.clone(),
            parent_id,
            timestamp: super::storage::create_timestamp(),
            thinking_level: level.to_string(),
        };
        self.storage.append_entry(entry).await;
        entry_id
    }

    /// Append a model change
    pub async fn append_model_change(&self, provider: &str, model_id: &str) -> String {
        let entry_id = self.storage.create_entry_id();
        let parent_id = self.storage.get_leaf_id().await;
        let entry = SessionTreeEntry::ModelChange {
            id: entry_id.clone(),
            parent_id,
            timestamp: super::storage::create_timestamp(),
            provider: provider.to_string(),
            model_id: model_id.to_string(),
        };
        self.storage.append_entry(entry).await;
        entry_id
    }

    /// Append an active tools change
    pub async fn append_active_tools_change(&self, active_tool_names: Vec<String>) -> String {
        let entry_id = self.storage.create_entry_id();
        let parent_id = self.storage.get_leaf_id().await;
        let entry = SessionTreeEntry::ActiveToolsChange {
            id: entry_id.clone(),
            parent_id,
            timestamp: super::storage::create_timestamp(),
            active_tool_names,
        };
        self.storage.append_entry(entry).await;
        entry_id
    }

    /// Append a compaction entry
    pub async fn append_compaction(
        &self,
        summary: &str,
        first_kept_entry_id: &str,
        tokens_before: u64,
        details: Option<&serde_json::Value>,
        from_hook: Option<bool>,
    ) -> String {
        let entry_id = self.storage.create_entry_id();
        let parent_id = self.storage.get_leaf_id().await;
        let entry = SessionTreeEntry::Compaction {
            id: entry_id.clone(),
            parent_id,
            timestamp: super::storage::create_timestamp(),
            summary: summary.to_string(),
            first_kept_entry_id: first_kept_entry_id.to_string(),
            tokens_before,
            details: details.cloned(),
            from_hook,
        };
        self.storage.append_entry(entry).await;
        entry_id
    }

    /// Append a custom message entry
    pub async fn append_custom_message(
        &self,
        custom_type: &str,
        content: CustomMessageContent,
        display: bool,
    ) -> String {
        let entry_id = self.storage.create_entry_id();
        let parent_id = self.storage.get_leaf_id().await;
        let entry = SessionTreeEntry::CustomMessage {
            id: entry_id.clone(),
            parent_id,
            timestamp: super::storage::create_timestamp(),
            custom_type: custom_type.to_string(),
            content,
            display,
            details: None,
        };
        self.storage.append_entry(entry).await;
        entry_id
    }

    /// Append a label
    pub async fn append_label(&self, target_id: &str, label: Option<&str>) -> String {
        let entry_id = self.storage.create_entry_id();
        let parent_id = self.storage.get_leaf_id().await;
        let entry = SessionTreeEntry::Label {
            id: entry_id.clone(),
            parent_id,
            timestamp: super::storage::create_timestamp(),
            target_id: target_id.to_string(),
            label: label.map(|s| s.to_string()),
        };
        self.storage.append_entry(entry).await;
        entry_id
    }

    /// Append session name
    pub async fn append_session_name(&self, name: &str) -> String {
        let entry_id = self.storage.create_entry_id();
        let parent_id = self.storage.get_leaf_id().await;
        let entry = SessionTreeEntry::SessionInfo {
            id: entry_id.clone(),
            parent_id,
            timestamp: super::storage::create_timestamp(),
            name: Some(name.trim().to_string()),
        };
        self.storage.append_entry(entry).await;
        entry_id
    }

    /// Append a custom entry
    pub async fn append_custom_entry(&self, custom_type: &str, data: &serde_json::Value) -> String {
        let entry_id = self.storage.create_entry_id();
        let parent_id = self.storage.get_leaf_id().await;
        let entry = SessionTreeEntry::Custom {
            id: entry_id.clone(),
            parent_id,
            timestamp: super::storage::create_timestamp(),
            custom_type: custom_type.to_string(),
            data: data.clone(),
        };
        self.storage.append_entry(entry).await;
        entry_id
    }

    /// Append a custom message entry
    pub async fn append_custom_message_entry(
        &self,
        custom_type: &str,
        content: &str,
        display: Option<&str>,
        details: Option<&serde_json::Value>,
    ) -> String {
        let entry_id = self.storage.create_entry_id();
        let parent_id = self.storage.get_leaf_id().await;
        let entry = SessionTreeEntry::CustomMessage {
            id: entry_id.clone(),
            parent_id,
            timestamp: super::storage::create_timestamp(),
            custom_type: custom_type.to_string(),
            content: CustomMessageContent::Text(content.to_string()),
            display: display.is_some(),
            details: details.cloned(),
        };
        self.storage.append_entry(entry).await;
        entry_id
    }

    /// Set leaf ID directly
    pub async fn set_leaf_id(
        &self,
        leaf_id: &str,
    ) -> Result<(), super::jsonl_storage::SessionError> {
        self.storage.set_leaf_id(Some(leaf_id.to_string())).await
    }

    /// Move to a new leaf position (tree navigation)
    pub async fn move_to(
        &self,
        entry_id: Option<&str>,
        summary: Option<String>,
        details: Option<serde_json::Value>,
        from_hook: Option<bool>,
    ) -> Result<Option<String>, super::jsonl_storage::SessionError> {
        self.storage
            .set_leaf_id(entry_id.map(|id| id.to_string()))
            .await?;

        if let Some(summary) = summary {
            let entry_id = entry_id.unwrap_or("root");
            let summary_id = self.storage.create_entry_id();
            let entry = SessionTreeEntry::BranchSummary {
                id: summary_id.clone(),
                parent_id: if entry_id == "root" {
                    None
                } else {
                    Some(entry_id.to_string())
                },
                timestamp: super::storage::create_timestamp(),
                from_id: entry_id.to_string(),
                summary,
                details,
                from_hook,
            };
            self.storage.append_entry(entry).await;
            Ok(Some(summary_id))
        } else {
            Ok(None)
        }
    }
}

/// Build session context from a path of entries (root-first order)
pub fn build_session_context(entries: &[SessionTreeEntry]) -> SessionContext {
    let mut messages = Vec::new();
    let mut thinking_level = "off".to_string();
    let mut model: Option<(String, String)> = None;
    let mut active_tool_names: Option<Vec<String>> = None;
    let mut compaction: Option<&SessionTreeEntry> = None;

    // First pass: find compaction, thinking level, model
    for entry in entries {
        match entry {
            SessionTreeEntry::ThinkingLevelChange {
                thinking_level: tl, ..
            } => {
                thinking_level = tl.clone();
            }
            SessionTreeEntry::ModelChange {
                provider, model_id, ..
            } => {
                model = Some((provider.clone(), model_id.clone()));
            }
            SessionTreeEntry::Compaction { .. } => {
                compaction = Some(entry);
            }
            SessionTreeEntry::ActiveToolsChange {
                active_tool_names: names,
                ..
            } => {
                active_tool_names = Some(names.clone());
            }
            _ => {}
        }
    }

    // Second pass: build messages
    if let Some(compaction_entry) = compaction {
        // Add compaction summary as a message
        if let SessionTreeEntry::Compaction { summary, .. } = compaction_entry {
            messages.push(AgentMessage::User(UserMessage {
                role: "user".to_string(),
                content: MessageContent::Text(format!(
                    "The conversation history before this point was compacted into the following summary:\n\n<summary>\n{}\n</summary>",
                    summary
                )),
                timestamp: chrono::Utc::now(),
            }));

            // Find the first kept entry id and only include entries from that point
            if let SessionTreeEntry::Compaction {
                first_kept_entry_id,
                ..
            } = compaction_entry
            {
                let mut found_start = false;
                for entry in entries {
                    if entry.id() == first_kept_entry_id {
                        found_start = true;
                    }
                    if found_start {
                        if let Some(msg) = entry_to_message(entry) {
                            messages.push(msg);
                        }
                    }
                }
            }
        }
    } else {
        // No compaction - include all message entries
        for entry in entries {
            if let Some(msg) = entry_to_message(entry) {
                messages.push(msg);
            }
        }
    }

    // Derive model from last assistant message if not set from explicit change
    if model.is_none() {
        for entry in entries.iter().rev() {
            if let SessionTreeEntry::Message {
                message: super::types::SessionMessage(AgentMessage::Assistant(assistant)),
                ..
            } = entry
            {
                model = Some((assistant.provider.to_string(), assistant.model.clone()));
                break;
            }
        }
    }

    SessionContext {
        messages,
        thinking_level,
        model,
        active_tool_names,
    }
}

/// Convert a session tree entry to an AgentMessage
fn entry_to_message(entry: &SessionTreeEntry) -> Option<AgentMessage> {
    match entry {
        SessionTreeEntry::Message { message, .. } => Some(message.0.clone()),
        SessionTreeEntry::BranchSummary {
            summary,
            from_id: _,
            ..
        } => Some(AgentMessage::User(UserMessage {
            role: "user".to_string(),
            content: MessageContent::Text(format!(
                "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n{}\n</summary>",
                summary
            )),
            timestamp: chrono::Utc::now(),
        })),
        SessionTreeEntry::CustomMessage {
            custom_type: _,
            content,
            display,
            ..
        } => {
            if !*display {
                return None;
            }
            // Convert custom message to a user message
            let text = match content {
                CustomMessageContent::Text(t) => t.clone(),
                CustomMessageContent::Blocks(blocks) => {
                    // Extract text from blocks
                    blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text(t) => Some(t.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            };
            Some(AgentMessage::User(UserMessage {
                role: "user".to_string(),
                content: MessageContent::Text(text),
                timestamp: chrono::Utc::now(),
            }))
        }
        _ => None,
    }
}

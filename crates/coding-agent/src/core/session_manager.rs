use flown_agent::AgentMessage;
use flown_agent::FileSystem;
use flown_agent::{
    JsonlSessionCreateOptions, JsonlSessionForkOptions, JsonlSessionListOptions,
    JsonlSessionMetadata, JsonlSessionRepo, Session, SessionError, SessionRepo,
};
use std::sync::Arc;

/// Manages conversation sessions with JSONL persistence.
///
/// Sessions are stored under `~/.flown/agent/sessions/` with subdirectories
/// encoded from the working directory (cwd), matching pi-mono's layout.
///
/// Each session file is named `<timestamp>_<uuid-v7>.jsonl` and contains
/// an append-only tree of entries (messages, tool calls, compaction, etc.).
pub struct SessionManager {
    repo: JsonlSessionRepo,
    current_session: Option<Session>,
    current_metadata: Option<JsonlSessionMetadata>,
    cwd: String,
}

impl SessionManager {
    /// Create a new SessionManager.
    ///
    /// `sessions_root` is typically `~/.flown/agent/sessions/`.
    pub fn new(fs: Arc<dyn FileSystem>, sessions_root: impl Into<String>) -> Self {
        let cwd = fs.cwd().to_string();
        Self {
            repo: JsonlSessionRepo::new(fs, sessions_root),
            current_session: None,
            current_metadata: None,
            cwd,
        }
    }

    /// Start a brand-new session for the current cwd.
    pub async fn start_new_session(&mut self) -> Result<&JsonlSessionMetadata, SessionError> {
        let session = self
            .repo
            .create(JsonlSessionCreateOptions {
                id: None,
                cwd: self.cwd.clone(),
                parent_session_path: None,
            })
            .await?;

        // Re-read the full JSONL metadata from the header
        let jsonl_meta = self.read_jsonl_metadata(&session).await;

        self.current_session = Some(session);
        self.current_metadata = Some(jsonl_meta.clone());
        Ok(self.current_metadata.as_ref().unwrap())
    }

    /// Continue an existing session from its metadata.
    pub async fn continue_session(
        &mut self,
        metadata: JsonlSessionMetadata,
    ) -> Result<(), SessionError> {
        let session = self.repo.open(metadata.clone()).await?;
        self.current_session = Some(session);
        self.current_metadata = Some(metadata);
        Ok(())
    }

    /// List all sessions for the current cwd, sorted by creation time (newest first).
    pub async fn list_sessions(&self) -> Result<Vec<JsonlSessionMetadata>, SessionError> {
        self.repo
            .list(JsonlSessionListOptions {
                cwd: Some(self.cwd.clone()),
            })
            .await
    }

    /// List all sessions across all cwds.
    pub async fn list_all_sessions(&self) -> Result<Vec<JsonlSessionMetadata>, SessionError> {
        self.repo.list(JsonlSessionListOptions { cwd: None }).await
    }

    /// Get the current session, or create a new one if none exists.
    pub async fn get_or_create_session(&mut self) -> Result<&JsonlSessionMetadata, SessionError> {
        if self.current_session.is_some() {
            return Ok(self.current_metadata.as_ref().unwrap());
        }
        self.start_new_session().await
    }

    /// Delete a session by its metadata.
    pub async fn delete_session(&self, metadata: JsonlSessionMetadata) -> Result<(), SessionError> {
        self.repo.delete(metadata).await
    }

    /// Fork the current session at a specific entry.
    pub async fn fork_session(
        &mut self,
        entry_id: Option<&str>,
    ) -> Result<&JsonlSessionMetadata, SessionError> {
        let source = self
            .current_metadata
            .as_ref()
            .ok_or_else(|| SessionError::NotFound("no current session".to_string()))?;

        let session = self
            .repo
            .fork(
                source.clone(),
                JsonlSessionForkOptions {
                    id: None,
                    cwd: self.cwd.clone(),
                    parent_session_path: None,
                    entry_id: entry_id.map(|s| s.to_string()),
                    position: None,
                },
            )
            .await?;

        let jsonl_meta = self.read_jsonl_metadata(&session).await;
        self.current_session = Some(session);
        self.current_metadata = Some(jsonl_meta.clone());
        Ok(self.current_metadata.as_ref().unwrap())
    }

    // --- Message appending ---

    /// Append a user message to the current session.
    pub async fn append_user_message(&mut self, content: &str) -> Result<String, SessionError> {
        let session = self.get_or_create_session_mut().await?;
        let msg = flown_ai::UserMessage {
            role: "user".to_string(),
            content: flown_ai::MessageContent::Text(content.to_string()),
            timestamp: chrono::Utc::now(),
        };
        let id = session.append_message(AgentMessage::User(msg)).await;
        Ok(id)
    }

    /// Append an assistant message to the current session.
    pub async fn append_assistant_message(
        &mut self,
        message: &AgentMessage,
    ) -> Result<String, SessionError> {
        let session = self.get_or_create_session_mut().await?;
        let id = session.append_message(message.clone()).await;
        Ok(id)
    }

    /// Append a model change entry.
    pub async fn append_model_change(
        &mut self,
        provider: &str,
        model_id: &str,
    ) -> Result<String, SessionError> {
        let session = self.get_or_create_session_mut().await?;
        let id = session.append_model_change(provider, model_id).await;
        Ok(id)
    }

    /// Append a thinking level change entry.
    pub async fn append_thinking_level_change(
        &mut self,
        level: &str,
    ) -> Result<String, SessionError> {
        let session = self.get_or_create_session_mut().await?;
        let id = session.append_thinking_level_change(level).await;
        Ok(id)
    }

    /// Append a compaction entry.
    pub async fn append_compaction(
        &mut self,
        summary: &str,
        first_kept_entry_id: &str,
        tokens_before: u64,
    ) -> Result<String, SessionError> {
        let session = self.get_or_create_session_mut().await?;
        let id = session
            .append_compaction(summary, first_kept_entry_id, tokens_before, None, None)
            .await;
        Ok(id)
    }

    /// Append a custom entry (e.g., workflow state).
    pub async fn append_custom_entry(
        &mut self,
        custom_type: &str,
        data: &serde_json::Value,
    ) -> Result<String, SessionError> {
        let session = self.get_or_create_session_mut().await?;
        let id = session.append_custom_entry(custom_type, data).await;
        Ok(id)
    }

    /// Set the session name.
    pub async fn set_session_name(&mut self, name: &str) -> Result<String, SessionError> {
        let session = self.get_or_create_session_mut().await?;
        let id = session.append_session_name(name).await;
        Ok(id)
    }

    /// Get the session name.
    pub async fn session_name(&self) -> Option<String> {
        self.current_session.as_ref()?.get_session_name().await
    }

    // --- Context building ---

    /// Build LLM context from the current session's branch.
    pub async fn build_context(&self) -> Result<flown_agent::SessionContext, SessionError> {
        let session = self
            .current_session
            .as_ref()
            .ok_or_else(|| SessionError::NotFound("no current session".to_string()))?;
        Ok(session.build_context().await)
    }

    /// Get the current session.
    pub fn current_session(&self) -> Option<&Session> {
        self.current_session.as_ref()
    }

    /// Get the current session metadata.
    pub fn current_metadata(&self) -> Option<&JsonlSessionMetadata> {
        self.current_metadata.as_ref()
    }

    /// Get the current session ID.
    pub fn current_session_id(&self) -> Option<&str> {
        self.current_metadata.as_ref().map(|m| m.base.id.as_str())
    }

    /// Check if a session is currently loaded.
    pub fn has_session(&self) -> bool {
        self.current_session.is_some()
    }

    // --- Helpers ---

    async fn get_or_create_session_mut(&mut self) -> Result<&mut Session, SessionError> {
        if self.current_session.is_none() {
            self.start_new_session().await?;
        }
        Ok(self.current_session.as_mut().unwrap())
    }

    async fn read_jsonl_metadata(&self, session: &Session) -> JsonlSessionMetadata {
        let meta = session.get_metadata().await;
        JsonlSessionMetadata {
            base: meta.clone(),
            cwd: self.cwd.clone(),
            path: String::new(), // Will be filled from the repo
            parent_session_path: None,
        }
    }
}

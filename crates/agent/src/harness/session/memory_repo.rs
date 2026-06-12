use super::jsonl_storage::SessionError;
use super::repo_utils::{create_session_id, create_timestamp, get_entries_to_fork, to_session};
use super::session::Session;
use super::storage::{InMemorySessionStorage, SessionStorage};
use super::types::{ForkPosition, SessionMetadata};
use parking_lot::RwLock;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct MemorySessionCreateOptions {
    pub id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MemorySessionForkOptions {
    pub id: Option<String>,
    pub entry_id: Option<String>,
    pub position: Option<ForkPosition>,
}

#[async_trait::async_trait]
pub trait SessionRepo<TMetadata, TCreateOptions, TListOptions, TForkOptions>: Send + Sync {
    async fn create(&self, options: TCreateOptions) -> Result<Session, SessionError>;
    async fn open(&self, metadata: TMetadata) -> Result<Session, SessionError>;
    async fn list(&self, options: TListOptions) -> Result<Vec<TMetadata>, SessionError>;
    async fn delete(&self, metadata: TMetadata) -> Result<(), SessionError>;
    async fn fork(
        &self,
        source_metadata: TMetadata,
        options: TForkOptions,
    ) -> Result<Session, SessionError>;
}

pub struct InMemorySessionRepo {
    sessions: RwLock<HashMap<String, InMemorySessionStorage>>,
}

impl InMemorySessionRepo {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemorySessionRepo {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SessionRepo<SessionMetadata, MemorySessionCreateOptions, (), MemorySessionForkOptions>
    for InMemorySessionRepo
{
    async fn create(&self, options: MemorySessionCreateOptions) -> Result<Session, SessionError> {
        let metadata = SessionMetadata {
            id: options.id.unwrap_or_else(create_session_id),
            created_at: create_timestamp(),
        };
        let storage = InMemorySessionStorage::from_entries(metadata.clone(), Vec::new());
        self.sessions
            .write()
            .insert(metadata.id.clone(), storage.clone());
        Ok(to_session(Box::new(storage)))
    }

    async fn open(&self, metadata: SessionMetadata) -> Result<Session, SessionError> {
        let storage = self
            .sessions
            .read()
            .get(&metadata.id)
            .cloned()
            .ok_or_else(|| SessionError::NotFound(format!("session not found: {}", metadata.id)))?;
        Ok(to_session(Box::new(storage)))
    }

    async fn list(&self, _options: ()) -> Result<Vec<SessionMetadata>, SessionError> {
        Ok(self
            .sessions
            .read()
            .values()
            .map(|storage| storage.metadata().clone())
            .collect())
    }

    async fn delete(&self, metadata: SessionMetadata) -> Result<(), SessionError> {
        self.sessions.write().remove(&metadata.id);
        Ok(())
    }

    async fn fork(
        &self,
        source_metadata: SessionMetadata,
        options: MemorySessionForkOptions,
    ) -> Result<Session, SessionError> {
        let source = self.open(source_metadata).await?;
        let entries = get_entries_to_fork(
            source.storage(),
            options.entry_id.as_deref(),
            options.position,
        )
        .await?;
        let metadata = SessionMetadata {
            id: options.id.unwrap_or_else(create_session_id),
            created_at: create_timestamp(),
        };
        let storage = InMemorySessionStorage::from_entries(metadata.clone(), entries);
        self.sessions
            .write()
            .insert(metadata.id.clone(), storage.clone());
        Ok(to_session(Box::new(storage)))
    }
}

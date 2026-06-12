use super::jsonl_storage::{JsonlSessionStorage, SessionError};
use super::memory_repo::SessionRepo;
use super::repo_utils::{create_session_id, create_timestamp, get_entries_to_fork, to_session};
use super::session::Session;
use super::storage::SessionStorage;
use super::types::{ForkPosition, JsonlSessionMetadata};
use crate::harness::env::types::{FileKind, FileSystem};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct JsonlSessionCreateOptions {
    pub id: Option<String>,
    pub cwd: String,
    pub parent_session_path: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct JsonlSessionListOptions {
    pub cwd: Option<String>,
}

#[derive(Debug, Clone)]
pub struct JsonlSessionForkOptions {
    pub id: Option<String>,
    pub cwd: String,
    pub parent_session_path: Option<String>,
    pub entry_id: Option<String>,
    pub position: Option<ForkPosition>,
}

pub struct JsonlSessionRepo {
    fs: Arc<dyn FileSystem>,
    sessions_root_input: String,
}

impl JsonlSessionRepo {
    pub fn new(fs: Arc<dyn FileSystem>, sessions_root: impl Into<String>) -> Self {
        Self {
            fs,
            sessions_root_input: sessions_root.into(),
        }
    }

    fn encode_cwd(cwd: &str) -> String {
        let encoded = cwd
            .trim_start_matches(['/', '\\'])
            .replace(['/', '\\', ':'], "-");
        format!("--{encoded}--")
    }

    fn sessions_root(&self) -> Result<String, SessionError> {
        self.fs
            .absolute_path(&self.sessions_root_input)
            .map_err(|error| SessionError::Storage(error.to_string()))
    }

    fn session_dir(&self, cwd: &str) -> Result<String, SessionError> {
        self.fs
            .join_path(&[&self.sessions_root()?, &Self::encode_cwd(cwd)])
            .map_err(|error| SessionError::Storage(error.to_string()))
    }

    fn session_file_path(
        &self,
        cwd: &str,
        session_id: &str,
        timestamp: &str,
    ) -> Result<String, SessionError> {
        let filename = format!(
            "{}_{}.jsonl",
            timestamp.replace([':', '.'], "-"),
            session_id
        );
        self.fs
            .join_path(&[&self.session_dir(cwd)?, &filename])
            .map_err(|error| SessionError::Storage(error.to_string()))
    }

    async fn list_session_dirs(&self) -> Result<Vec<String>, SessionError> {
        let sessions_root = self.sessions_root()?;
        if !self
            .fs
            .exists(&sessions_root)
            .await
            .map_err(|error| SessionError::Storage(error.to_string()))?
        {
            return Ok(Vec::new());
        }
        let entries = self
            .fs
            .list_dir(&sessions_root)
            .await
            .map_err(|error| SessionError::Storage(error.to_string()))?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.kind == FileKind::Directory)
            .map(|entry| entry.path)
            .collect())
    }
}

#[async_trait::async_trait]
impl
    SessionRepo<
        JsonlSessionMetadata,
        JsonlSessionCreateOptions,
        JsonlSessionListOptions,
        JsonlSessionForkOptions,
    > for JsonlSessionRepo
{
    async fn create(&self, options: JsonlSessionCreateOptions) -> Result<Session, SessionError> {
        let id = options.id.unwrap_or_else(create_session_id);
        let created_at = create_timestamp();
        let session_dir = self.session_dir(&options.cwd)?;
        self.fs
            .create_dir(&session_dir, true)
            .await
            .map_err(|error| SessionError::Storage(error.to_string()))?;
        let file_path = self.session_file_path(&options.cwd, &id, &created_at)?;
        let storage = JsonlSessionStorage::create(
            self.fs.clone(),
            file_path,
            options.cwd,
            id,
            options.parent_session_path,
        )
        .await?;
        Ok(to_session(Box::new(storage)))
    }

    async fn open(&self, metadata: JsonlSessionMetadata) -> Result<Session, SessionError> {
        if !self
            .fs
            .exists(&metadata.path)
            .await
            .map_err(|error| SessionError::Storage(error.to_string()))?
        {
            return Err(SessionError::NotFound(format!(
                "session not found: {}",
                metadata.path
            )));
        }
        let storage = JsonlSessionStorage::open(self.fs.clone(), metadata.path).await?;
        Ok(to_session(Box::new(storage)))
    }

    async fn list(
        &self,
        options: JsonlSessionListOptions,
    ) -> Result<Vec<JsonlSessionMetadata>, SessionError> {
        let dirs = if let Some(cwd) = options.cwd {
            vec![self.session_dir(&cwd)?]
        } else {
            self.list_session_dirs().await?
        };
        let mut sessions = Vec::new();
        for dir in dirs {
            if !self
                .fs
                .exists(&dir)
                .await
                .map_err(|error| SessionError::Storage(error.to_string()))?
            {
                continue;
            }
            for entry in self
                .fs
                .list_dir(&dir)
                .await
                .map_err(|error| SessionError::Storage(error.to_string()))?
            {
                if entry.kind == FileKind::Directory || !entry.name.ends_with(".jsonl") {
                    continue;
                }
                match JsonlSessionStorage::load_metadata(self.fs.clone(), &entry.path).await {
                    Ok(meta) => sessions.push(meta),
                    Err(SessionError::InvalidSession(_)) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        sessions.sort_by(|a, b| b.base.created_at.cmp(&a.base.created_at));
        Ok(sessions)
    }

    async fn delete(&self, metadata: JsonlSessionMetadata) -> Result<(), SessionError> {
        self.fs
            .remove(&metadata.path, false, true)
            .await
            .map_err(|error| SessionError::Storage(error.to_string()))
    }

    async fn fork(
        &self,
        source_metadata: JsonlSessionMetadata,
        options: JsonlSessionForkOptions,
    ) -> Result<Session, SessionError> {
        let source = self.open(source_metadata.clone()).await?;
        let entries = get_entries_to_fork(
            source.storage(),
            options.entry_id.as_deref(),
            options.position,
        )
        .await?;
        let id = options.id.unwrap_or_else(create_session_id);
        let created_at = create_timestamp();
        let session_dir = self.session_dir(&options.cwd)?;
        self.fs
            .create_dir(&session_dir, true)
            .await
            .map_err(|error| SessionError::Storage(error.to_string()))?;
        let storage = JsonlSessionStorage::create(
            self.fs.clone(),
            self.session_file_path(&options.cwd, &id, &created_at)?,
            options.cwd,
            id,
            options
                .parent_session_path
                .or_else(|| Some(source_metadata.path.clone())),
        )
        .await?;
        for entry in entries {
            storage.append_entry(entry).await;
        }
        Ok(to_session(Box::new(storage)))
    }
}

use super::storage::{SessionStorage, generate_entry_id_with_check};
use super::types::*;
use crate::harness::env::types::FileSystem;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// JSONL file header
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JsonlHeader {
    #[serde(rename = "type")]
    header_type: String,
    version: u32,
    id: String,
    timestamp: String,
    cwd: String,
    #[serde(rename = "parentSession")]
    parent_session: Option<String>,
}

/// JSONL session storage - persistent file-based storage
pub struct JsonlSessionStorage {
    fs: Arc<dyn FileSystem>,
    path: String,
    metadata: JsonlSessionMetadata,
    entries: Arc<RwLock<Vec<SessionTreeEntry>>>,
    by_id: Arc<RwLock<HashMap<String, SessionTreeEntry>>>,
    labels: Arc<RwLock<HashMap<String, String>>>,
    leaf_id: Arc<RwLock<Option<String>>>,
}

impl JsonlSessionStorage {
    /// Open an existing JSONL session file
    pub async fn open(
        fs: Arc<dyn FileSystem>,
        path: impl Into<String>,
    ) -> Result<Self, SessionError> {
        let path = path.into();
        let content = fs
            .read_text_file(&path)
            .await
            .map_err(|e| SessionError::Storage(e.to_string()))?;
        let mut lines = content.lines();

        // Read header
        let header_line = lines
            .next()
            .ok_or_else(|| SessionError::InvalidSession("empty file".to_string()))?;

        let header: JsonlHeader = serde_json::from_str(&header_line)
            .map_err(|e| SessionError::InvalidSession(format!("invalid header: {}", e)))?;

        let metadata = JsonlSessionMetadata {
            base: SessionMetadata {
                id: header.id,
                created_at: header.timestamp,
            },
            cwd: header.cwd,
            path: path.clone(),
            parent_session_path: header.parent_session,
        };

        let mut entries = Vec::new();
        let mut by_id = HashMap::new();
        let mut labels = HashMap::new();
        let mut current_leaf_id: Option<String> = None;

        // Read all entries
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            let entry: SessionTreeEntry = serde_json::from_str(&line)
                .map_err(|e| SessionError::InvalidEntry(format!("invalid entry: {}", e)))?;

            // Update leaf tracking
            if let Some(new_leaf) = entry.leaf_id_after() {
                current_leaf_id = Some(new_leaf);
            }

            // Update label cache - indexed by target_id, not entry id
            if let SessionTreeEntry::Label {
                target_id, label, ..
            } = &entry
            {
                if let Some(label) = label {
                    labels.insert(target_id.clone(), label.clone());
                } else {
                    labels.remove(target_id);
                }
            }

            let id = entry.id().to_string();
            by_id.insert(id, entry.clone());
            entries.push(entry);
        }

        Ok(Self {
            fs,
            path,
            metadata,
            entries: Arc::new(RwLock::new(entries)),
            by_id: Arc::new(RwLock::new(by_id)),
            labels: Arc::new(RwLock::new(labels)),
            leaf_id: Arc::new(RwLock::new(current_leaf_id)),
        })
    }

    /// Create a new JSONL session file
    pub async fn create(
        fs: Arc<dyn FileSystem>,
        path: impl Into<String>,
        cwd: impl Into<String>,
        session_id: impl Into<String>,
        parent_session_path: Option<String>,
    ) -> Result<Self, SessionError> {
        let path = path.into();
        let session_id = session_id.into();
        let parent_session_path = parent_session_path;
        let header = JsonlHeader {
            header_type: "session".to_string(),
            version: 3,
            id: session_id.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            cwd: cwd.into(),
            parent_session: parent_session_path.clone(),
        };

        let header_json =
            serde_json::to_string(&header).map_err(|e| SessionError::Storage(e.to_string()))?;

        fs.write_file(&path, format!("{}\n", header_json).as_bytes())
            .await
            .map_err(|e| SessionError::Storage(e.to_string()))?;

        let metadata = JsonlSessionMetadata {
            base: SessionMetadata {
                id: session_id,
                created_at: header.timestamp,
            },
            cwd: header.cwd,
            path: path.clone(),
            parent_session_path,
        };

        Ok(Self {
            fs,
            path,
            metadata,
            entries: Arc::new(RwLock::new(Vec::new())),
            by_id: Arc::new(RwLock::new(HashMap::new())),
            labels: Arc::new(RwLock::new(HashMap::new())),
            leaf_id: Arc::new(RwLock::new(None)),
        })
    }

    async fn append_to_file(&self, entry: &SessionTreeEntry) -> Result<(), SessionError> {
        let json =
            serde_json::to_string(entry).map_err(|e| SessionError::Storage(e.to_string()))?;

        self.fs
            .append_file(&self.path, format!("{}\n", json).as_bytes())
            .await
            .map_err(|e| SessionError::Storage(e.to_string()))?;

        Ok(())
    }

    pub fn metadata_jsonl(&self) -> &JsonlSessionMetadata {
        &self.metadata
    }

    /// Load only the session metadata (header line) from a JSONL file.
    /// Aligned with pi-mono's `loadJsonlSessionMetadata()`.
    pub async fn load_metadata(
        fs: Arc<dyn FileSystem>,
        path: &str,
    ) -> Result<JsonlSessionMetadata, SessionError> {
        let lines = fs
            .read_text_lines(path, Some(1))
            .await
            .map_err(|e| SessionError::Storage(e.to_string()))?;

        let line = lines
            .into_iter()
            .next()
            .ok_or_else(|| SessionError::InvalidSession("missing session header".to_string()))?;

        if line.trim().is_empty() {
            return Err(SessionError::InvalidSession("missing session header".to_string()));
        }

        let header: JsonlHeader = serde_json::from_str(&line)
            .map_err(|e| SessionError::InvalidSession(format!("invalid header: {}", e)))?;

        Ok(JsonlSessionMetadata {
            base: SessionMetadata {
                id: header.id,
                created_at: header.timestamp,
            },
            cwd: header.cwd,
            path: path.to_string(),
            parent_session_path: header.parent_session,
        })
    }
}

#[async_trait::async_trait]
impl SessionStorage for JsonlSessionStorage {
    fn metadata(&self) -> &SessionMetadata {
        &self.metadata.base
    }

    async fn get_leaf_id(&self) -> Option<String> {
        self.leaf_id.read().clone()
    }

    async fn set_leaf_id(&self, leaf_id: Option<String>) -> Result<(), SessionError> {
        if let Some(target_id) = leaf_id.as_ref() {
            if !self.by_id.read().contains_key(target_id) {
                return Err(SessionError::NotFound(format!(
                    "entry not found: {target_id}"
                )));
            }
        }
        // Create a leaf entry
        let entry_id = self.create_entry_id();
        let parent_id = self.leaf_id.read().clone();
        let entry = SessionTreeEntry::Leaf {
            id: entry_id,
            parent_id,
            timestamp: chrono::Utc::now().to_rfc3339(),
            target_id: leaf_id.clone(),
        };

        // Append to file
        self.append_to_file(&entry).await?;

        // Update in-memory state
        *self.leaf_id.write() = leaf_id;
        self.by_id
            .write()
            .insert(entry.id().to_string(), entry.clone());
        self.entries.write().push(entry);
        Ok(())
    }

    fn create_entry_id(&self) -> String {
        let by_id = self.by_id.read();
        generate_entry_id_with_check(|id| by_id.contains_key(id))
    }

    async fn append_entry(&self, entry: SessionTreeEntry) {
        // Update leaf tracking
        let new_leaf = entry.leaf_id_after();
        if new_leaf.is_some() {
            *self.leaf_id.write() = new_leaf;
        }

        // Update label cache - indexed by target_id, not entry id
        if let SessionTreeEntry::Label {
            target_id, label, ..
        } = &entry
        {
            let mut labels = self.labels.write();
            if let Some(label) = label {
                labels.insert(target_id.clone(), label.clone());
            } else {
                labels.remove(target_id);
            }
        }

        // Append to file
        let _ = self.append_to_file(&entry).await;

        let id = entry.id().to_string();
        self.by_id.write().insert(id, entry.clone());
        self.entries.write().push(entry);
    }

    async fn get_entry(&self, id: &str) -> Option<SessionTreeEntry> {
        self.by_id.read().get(id).cloned()
    }

    async fn find_entries(&self, entry_type: &str) -> Vec<SessionTreeEntry> {
        self.entries
            .read()
            .iter()
            .filter(|e| e.entry_type() == entry_type)
            .cloned()
            .collect()
    }

    async fn get_label(&self, id: &str) -> Option<String> {
        self.labels.read().get(id).cloned()
    }

    async fn get_path_to_root(&self, leaf_id: Option<&str>) -> Vec<SessionTreeEntry> {
        let by_id = self.by_id.read();
        let mut path = Vec::new();
        let mut current_id = leaf_id.map(|s| s.to_string());

        while let Some(id) = current_id {
            if let Some(entry) = by_id.get(&id) {
                current_id = entry.parent_id().map(|s| s.to_string());
                path.push(entry.clone());
            } else {
                break;
            }
        }

        path.reverse(); // root-first order
        path
    }

    async fn get_entries(&self) -> Vec<SessionTreeEntry> {
        self.entries.read().clone()
    }
}

/// Session error types
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid session: {0}")]
    InvalidSession(String),
    #[error("invalid entry: {0}")]
    InvalidEntry(String),
    #[error("invalid fork target: {0}")]
    InvalidForkTarget(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("unknown error: {0}")]
    Unknown(String),
}

use super::jsonl_storage::SessionError;
use super::types::*;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Session storage trait
#[async_trait::async_trait]
pub trait SessionStorage: Send + Sync {
    fn metadata(&self) -> &SessionMetadata;
    async fn get_leaf_id(&self) -> Option<String>;
    async fn set_leaf_id(&self, leaf_id: Option<String>) -> Result<(), SessionError>;
    fn create_entry_id(&self) -> String;
    async fn append_entry(&self, entry: SessionTreeEntry);
    async fn get_entry(&self, id: &str) -> Option<SessionTreeEntry>;
    async fn find_entries(&self, entry_type: &str) -> Vec<SessionTreeEntry>;
    async fn get_label(&self, id: &str) -> Option<String>;
    async fn get_path_to_root(&self, leaf_id: Option<&str>) -> Vec<SessionTreeEntry>;
    async fn get_entries(&self) -> Vec<SessionTreeEntry>;
}

/// In-memory session storage for testing
#[derive(Clone)]
pub struct InMemorySessionStorage {
    metadata: SessionMetadata,
    entries: Arc<RwLock<Vec<SessionTreeEntry>>>,
    by_id: Arc<RwLock<HashMap<String, SessionTreeEntry>>>,
    labels: Arc<RwLock<HashMap<String, String>>>,
    leaf_id: Arc<RwLock<Option<String>>>,
}

impl InMemorySessionStorage {
    pub fn new(id: impl Into<String>) -> Self {
        Self::from_entries(
            SessionMetadata {
                id: id.into(),
                created_at: create_timestamp(),
            },
            Vec::new(),
        )
    }

    pub fn from_entries(metadata: SessionMetadata, entries: Vec<SessionTreeEntry>) -> Self {
        let storage = Self {
            metadata,
            entries: Arc::new(RwLock::new(Vec::new())),
            by_id: Arc::new(RwLock::new(HashMap::new())),
            labels: Arc::new(RwLock::new(HashMap::new())),
            leaf_id: Arc::new(RwLock::new(None)),
        };
        for entry in entries {
            storage.append_entry_sync(entry);
        }
        storage
    }

    fn append_entry_sync(&self, entry: SessionTreeEntry) {
        let new_leaf = entry.leaf_id_after();
        if new_leaf.is_some() {
            *self.leaf_id.write() = new_leaf;
        }

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

        let id = entry.id().to_string();
        self.by_id.write().insert(id, entry.clone());
        self.entries.write().push(entry);
    }
}

#[async_trait::async_trait]
impl SessionStorage for InMemorySessionStorage {
    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
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

        let entry = SessionTreeEntry::Leaf {
            id: self.create_entry_id(),
            parent_id: self.leaf_id.read().clone(),
            timestamp: create_timestamp(),
            target_id: leaf_id.clone(),
        };
        self.by_id
            .write()
            .insert(entry.id().to_string(), entry.clone());
        self.entries.write().push(entry);
        *self.leaf_id.write() = leaf_id;
        Ok(())
    }

    fn create_entry_id(&self) -> String {
        let by_id = self.by_id.read();
        generate_entry_id_with_check(|id| by_id.contains_key(id))
    }

    async fn append_entry(&self, entry: SessionTreeEntry) {
        self.append_entry_sync(entry);
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
        let mut seen = std::collections::HashSet::new();

        while let Some(id) = current_id {
            if !seen.insert(id.clone()) {
                break;
            }
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

/// Generate a UUIDv7 entry ID with collision checking.
pub fn generate_entry_id_with_check(exists: impl Fn(&str) -> bool) -> String {
    for _ in 0..100 {
        let id = uuid::Uuid::now_v7().to_string();
        if !exists(&id) {
            return id;
        }
    }
    uuid::Uuid::now_v7().to_string()
}

/// Generate a session ID
pub fn create_session_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Create ISO 8601 timestamp
pub fn create_timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

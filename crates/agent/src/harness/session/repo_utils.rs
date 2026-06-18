use super::jsonl_storage::SessionError;
use super::session::Session;
use super::storage::{SessionStorage, create_session_id as storage_create_session_id};
use super::types::{ForkPosition, SessionTreeEntry};
use crate::harness::{FileError, FileErrorCode};

pub fn create_session_id() -> String {
    storage_create_session_id()
}

pub fn create_timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub fn to_session(storage: Box<dyn SessionStorage>) -> Session {
    Session::new(storage)
}

pub fn get_file_system_result_or_throw<T>(
    result: Result<T, FileError>,
    message: &str,
) -> Result<T, SessionError> {
    result.map_err(|error| match error.code {
        FileErrorCode::NotFound => SessionError::NotFound(format!("{message}: {error}")),
        _ => SessionError::Storage(format!("{message}: {error}")),
    })
}

pub async fn get_entries_to_fork(
    storage: &dyn SessionStorage,
    entry_id: Option<&str>,
    position: Option<ForkPosition>,
) -> Result<Vec<SessionTreeEntry>, SessionError> {
    let Some(entry_id) = entry_id else {
        return Ok(storage.get_entries().await);
    };
    let target = storage
        .get_entry(entry_id)
        .await
        .ok_or_else(|| SessionError::InvalidForkTarget(format!("entry not found: {entry_id}")))?;
    let effective_leaf_id = match position.unwrap_or(ForkPosition::Before) {
        ForkPosition::At => Some(target.id().to_string()),
        ForkPosition::Before => match &target {
            SessionTreeEntry::Message {
                message, parent_id, ..
            } => match &message.0 {
                crate::types::AgentMessage::User(_) => parent_id.clone(),
                _ => {
                    return Err(SessionError::InvalidForkTarget(format!(
                        "entry is not a user message: {entry_id}"
                    )));
                }
            },
            _ => {
                return Err(SessionError::InvalidForkTarget(format!(
                    "entry is not a user message: {entry_id}"
                )));
            }
        },
    };
    Ok(storage.get_path_to_root(effective_leaf_id.as_deref()).await)
}

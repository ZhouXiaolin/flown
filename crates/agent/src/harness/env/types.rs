use std::{collections::HashMap, fmt, sync::Arc};

pub use flown_ai::types::AbortSignal;
use serde::{Deserialize, Serialize};

/// File system error codes
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileErrorCode {
    #[error("operation aborted")]
    Aborted,
    #[error("not found")]
    NotFound,
    #[error("permission denied")]
    PermissionDenied,
    #[error("not a directory")]
    NotDirectory,
    #[error("is a directory")]
    IsDirectory,
    #[error("invalid path")]
    Invalid,
    #[error("not supported")]
    NotSupported,
    #[error("unknown error")]
    Unknown,
}

/// File system error
#[derive(Debug, thiserror::Error)]
#[error("{code}")]
pub struct FileError {
    pub code: FileErrorCode,
    pub path: Option<String>,
}

impl FileError {
    pub fn new(code: FileErrorCode, path: impl Into<String>) -> Self {
        Self {
            code,
            path: Some(path.into()),
        }
    }
}

/// Shell execution error codes
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecutionErrorCode {
    #[error("operation aborted")]
    Aborted,
    #[error("execution timeout")]
    Timeout,
    #[error("shell unavailable")]
    ShellUnavailable,
    #[error("spawn error")]
    SpawnError,
    #[error("callback error")]
    CallbackError,
    #[error("unknown error")]
    Unknown,
}

/// Shell execution error
#[derive(Debug, thiserror::Error)]
#[error("{code}")]
pub struct ExecutionError {
    pub code: ExecutionErrorCode,
}

pub type ShellOutputUpdateFn = Arc<dyn Fn(&str) -> Result<(), ExecutionError> + Send + Sync>;

/// File kind
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}

/// File information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    pub kind: FileKind,
    pub size: u64,
    pub mtime_ms: u64,
}

/// Shell execution options.
///
/// Maps to pi-mono `ExecutionEnvExecOptions`. Rust uses `AbortSignal` for
/// cancellation and fallible callbacks for stdout/stderr streaming updates.
#[derive(Clone, Default)]
pub struct ExecOptions {
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub timeout: Option<u64>,
    pub abort_signal: Option<AbortSignal>,
    pub on_stdout: Option<ShellOutputUpdateFn>,
    pub on_stderr: Option<ShellOutputUpdateFn>,
}

impl fmt::Debug for ExecOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecOptions")
            .field("cwd", &self.cwd)
            .field("env", &self.env)
            .field("timeout", &self.timeout)
            .field("abort_signal", &self.abort_signal)
            .field("on_stdout", &self.on_stdout.as_ref().map(|_| "<callback>"))
            .field("on_stderr", &self.on_stderr.as_ref().map(|_| "<callback>"))
            .finish()
    }
}

/// Shell execution result
#[derive(Debug, Clone)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// File system abstraction
#[async_trait::async_trait]
pub trait FileSystem: Send + Sync {
    fn cwd(&self) -> &str;

    fn absolute_path(&self, path: &str) -> Result<String, FileError>;

    fn join_path(&self, parts: &[&str]) -> Result<String, FileError>;

    async fn read_text_file(&self, path: &str) -> Result<String, FileError>;

    async fn read_text_lines(
        &self,
        path: &str,
        max_lines: Option<usize>,
    ) -> Result<Vec<String>, FileError>;

    async fn read_binary_file(&self, path: &str) -> Result<Vec<u8>, FileError>;

    async fn write_file(&self, path: &str, content: &[u8]) -> Result<(), FileError>;

    async fn append_file(&self, path: &str, content: &[u8]) -> Result<(), FileError>;

    async fn file_info(&self, path: &str) -> Result<FileInfo, FileError>;

    async fn list_dir(&self, path: &str) -> Result<Vec<FileInfo>, FileError>;

    async fn canonical_path(&self, path: &str) -> Result<String, FileError>;

    async fn exists(&self, path: &str) -> Result<bool, FileError>;

    async fn create_dir(&self, path: &str, recursive: bool) -> Result<(), FileError>;

    async fn remove(&self, path: &str, recursive: bool, force: bool) -> Result<(), FileError>;

    async fn create_temp_dir(&self, prefix: Option<&str>) -> Result<String, FileError>;

    async fn create_temp_file(
        &self,
        prefix: Option<&str>,
        suffix: Option<&str>,
    ) -> Result<String, FileError>;

    async fn cleanup(&self) -> Result<(), FileError>;
}

/// Shell abstraction
#[async_trait::async_trait]
pub trait Shell: Send + Sync {
    async fn exec(&self, command: &str, options: ExecOptions)
    -> Result<ExecResult, ExecutionError>;

    async fn cleanup(&self) -> Result<(), ExecutionError>;
}

/// Combined execution environment
pub trait ExecutionEnv: FileSystem + Shell {}

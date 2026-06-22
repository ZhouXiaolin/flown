use std::path::{Path, PathBuf};
use std::process::Stdio;

use flown_agent::{
    ExecOptions, ExecResult, ExecutionEnv, ExecutionError, ExecutionErrorCode, FileError,
    FileErrorCode, FileInfo, FileKind, FileSystem, Shell,
};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Native execution environment using std::fs and std::process
pub struct NativeExecutionEnv {
    cwd: PathBuf,
    temp_dirs: Vec<PathBuf>,
}

impl NativeExecutionEnv {
    pub fn new() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            temp_dirs: Vec::new(),
        }
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }
}

#[async_trait::async_trait]
impl FileSystem for NativeExecutionEnv {
    fn cwd(&self) -> &str {
        self.cwd.to_str().unwrap_or(".")
    }

    fn absolute_path(&self, path: &str) -> Result<String, FileError> {
        Ok(self.resolve_path(path).to_string_lossy().to_string())
    }

    fn join_path(&self, parts: &[&str]) -> Result<String, FileError> {
        if parts.is_empty() {
            return Err(FileError::new(FileErrorCode::Invalid, ""));
        }
        let mut result = PathBuf::from(parts[0]);
        for part in &parts[1..] {
            result.push(part);
        }
        Ok(result.to_string_lossy().to_string())
    }

    async fn read_text_file(&self, path: &str) -> Result<String, FileError> {
        let path = self.resolve_path(path);
        fs::read_to_string(&path)
            .await
            .map_err(|e| map_io_error(e, &path))
    }

    async fn read_text_lines(
        &self,
        path: &str,
        max_lines: Option<usize>,
    ) -> Result<Vec<String>, FileError> {
        let path = self.resolve_path(path);
        let content = fs::read_to_string(&path)
            .await
            .map_err(|e| map_io_error(e, &path))?;
        let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
        Ok(match max_lines {
            Some(n) => lines.into_iter().take(n).collect(),
            None => lines,
        })
    }

    async fn read_binary_file(&self, path: &str) -> Result<Vec<u8>, FileError> {
        let path = self.resolve_path(path);
        fs::read(&path).await.map_err(|e| map_io_error(e, &path))
    }

    async fn write_file(&self, path: &str, content: &[u8]) -> Result<(), FileError> {
        let path = self.resolve_path(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| map_io_error(e, parent))?;
        }
        fs::write(&path, content)
            .await
            .map_err(|e| map_io_error(e, &path))
    }

    async fn append_file(&self, path: &str, content: &[u8]) -> Result<(), FileError> {
        use tokio::io::AsyncWriteExt;
        let path = self.resolve_path(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| map_io_error(e, parent))?;
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|e| map_io_error(e, &path))?;
        file.write_all(content)
            .await
            .map_err(|e| map_io_error(e, &path))
    }

    async fn file_info(&self, path: &str) -> Result<FileInfo, FileError> {
        let path = self.resolve_path(path);
        let meta = fs::symlink_metadata(&path)
            .await
            .map_err(|e| map_io_error(e, &path))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let kind = if meta.file_type().is_symlink() {
            FileKind::Symlink
        } else if meta.is_dir() {
            FileKind::Directory
        } else {
            FileKind::File
        };
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        Ok(FileInfo {
            name,
            path: path.to_string_lossy().to_string(),
            kind,
            size: meta.len(),
            mtime_ms,
        })
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<FileInfo>, FileError> {
        let path = self.resolve_path(path);
        let mut entries = Vec::new();
        let mut dir = fs::read_dir(&path)
            .await
            .map_err(|e| map_io_error(e, &path))?;
        while let Some(entry) = dir.next_entry().await.map_err(|e| map_io_error(e, &path))? {
            let entry_path = entry.path();
            let meta = entry
                .metadata()
                .await
                .map_err(|e| map_io_error(e, &entry_path))?;
            let name = entry.file_name().to_string_lossy().to_string();
            let kind = if meta.file_type().is_symlink() {
                FileKind::Symlink
            } else if meta.is_dir() {
                FileKind::Directory
            } else {
                FileKind::File
            };
            let mtime_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            entries.push(FileInfo {
                name,
                path: entry.path().to_string_lossy().to_string(),
                kind,
                size: meta.len(),
                mtime_ms,
            });
        }
        Ok(entries)
    }

    async fn canonical_path(&self, path: &str) -> Result<String, FileError> {
        let path = self.resolve_path(path);
        fs::canonicalize(&path)
            .await
            .map(|path| path.to_string_lossy().to_string())
            .map_err(|e| map_io_error(e, &path))
    }

    async fn exists(&self, path: &str) -> Result<bool, FileError> {
        Ok(self.resolve_path(path).exists())
    }

    async fn create_dir(&self, path: &str, recursive: bool) -> Result<(), FileError> {
        let path = self.resolve_path(path);
        if recursive {
            fs::create_dir_all(&path)
                .await
                .map_err(|e| map_io_error(e, &path))
        } else {
            fs::create_dir(&path)
                .await
                .map_err(|e| map_io_error(e, &path))
        }
    }

    async fn remove(&self, path: &str, recursive: bool, force: bool) -> Result<(), FileError> {
        let path = self.resolve_path(path);
        if !path.exists() {
            if force {
                return Ok(());
            }
            return Err(FileError::new(
                FileErrorCode::NotFound,
                format!("{} not found", path.display()),
            ));
        }
        if recursive {
            fs::remove_dir_all(&path)
                .await
                .map_err(|e| map_io_error(e, &path))
        } else {
            fs::remove_file(&path)
                .await
                .map_err(|e| map_io_error(e, &path))
        }
    }

    async fn create_temp_dir(&self, prefix: Option<&str>) -> Result<String, FileError> {
        let prefix = prefix.unwrap_or("flown");
        let temp_dir = std::env::temp_dir();
        let dir_name = format!("{}_{}", prefix, uuid::Uuid::new_v4());
        let path = temp_dir.join(dir_name);
        fs::create_dir_all(&path)
            .await
            .map_err(|e| map_io_error(e, &path))?;
        Ok(path.to_string_lossy().to_string())
    }

    async fn create_temp_file(
        &self,
        prefix: Option<&str>,
        suffix: Option<&str>,
    ) -> Result<String, FileError> {
        let prefix = prefix.unwrap_or("flown");
        let suffix = suffix.unwrap_or(".tmp");
        let temp_dir = std::env::temp_dir();
        let file_name = format!("{}_{}{}", prefix, uuid::Uuid::new_v4(), suffix);
        let path = temp_dir.join(file_name);
        fs::write(&path, "")
            .await
            .map_err(|e| map_io_error(e, &path))?;
        Ok(path.to_string_lossy().to_string())
    }

    async fn cleanup(&self) -> Result<(), FileError> {
        for dir in &self.temp_dirs {
            if dir.exists() {
                let _ = fs::remove_dir_all(dir).await;
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Shell for NativeExecutionEnv {
    async fn exec(
        &self,
        command: &str,
        options: ExecOptions,
    ) -> Result<ExecResult, ExecutionError> {
        let cwd = options
            .cwd
            .as_ref()
            .map(|c| self.resolve_path(c))
            .unwrap_or_else(|| self.cwd.clone());

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd.current_dir(&cwd);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        if let Some(env) = &options.env {
            for (key, value) in env {
                cmd.env(key, value);
            }
        }

        let mut child = cmd.spawn().map_err(|e| {
            ExecutionError::with_source(
                ExecutionErrorCode::SpawnError,
                format!("failed to spawn shell command: {command}"),
                e,
            )
        })?;

        // Take stdout/stderr pipes and read them concurrently with
        // `tokio::select!`, interleaving into a single buffer in arrival order
        // (mirrors pi-mono's single `onData` callback wired to both streams).
        let stdout = child.stdout.take().ok_or_else(|| {
            ExecutionError::new(
                ExecutionErrorCode::SpawnError,
                "shell command produced no stdout pipe",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            ExecutionError::new(
                ExecutionErrorCode::SpawnError,
                "shell command produced no stderr pipe",
            )
        })?;

        let mut stdout_lines = BufReader::new(stdout).lines();
        let mut stderr_lines = BufReader::new(stderr).lines();
        let mut output = String::new();

        // Helper: push a line and fire the streaming callback. Reconstructs the
        // trailing newline that `next_line` strips, so `output` matches what
        // `wait_with_output` would have produced.
        fn emit_line(
            line: &str,
            output: &mut String,
            on_output: &Option<flown_agent::ShellOutputUpdateFn>,
        ) {
            output.push_str(line);
            output.push('\n');
            if let Some(on_output) = on_output {
                let _ = on_output(&format!("{line}\n"));
            }
        }

        let on_output = &options.on_output;

        // Drain both streams to EOF, interleaving lines into `output` in
        // arrival order. Both futures are polled in a single task, so tokio's
        // scheduler gives each a chance as data arrives on its pipe. When a
        // stream hits EOF we mark it done; the loop exits once both are drained,
        // after which `child.wait()` completes immediately (pipes are closed).
        let run = async {
            let mut stdout_eof = false;
            let mut stderr_eof = false;
            while !(stdout_eof && stderr_eof) {
                tokio::select! {
                    // Biased so neither stream can starve the other under load.
                    biased;

                    line = async {
                        if stdout_eof { std::future::pending::<std::io::Result<Option<String>>>().await }
                        else { stdout_lines.next_line().await }
                    } => {
                        match line {
                            Ok(Some(line)) => emit_line(&line, &mut output, on_output),
                            Ok(None) => stdout_eof = true,
                            Err(e) => {
                                return Err(ExecutionError::with_source(
                                    ExecutionErrorCode::Unknown,
                                    format!("failed to read shell stdout: {command}"),
                                    e,
                                ));
                            }
                        }
                    }
                    line = async {
                        if stderr_eof { std::future::pending::<std::io::Result<Option<String>>>().await }
                        else { stderr_lines.next_line().await }
                    } => {
                        match line {
                            Ok(Some(line)) => emit_line(&line, &mut output, on_output),
                            Ok(None) => stderr_eof = true,
                            Err(e) => {
                                return Err(ExecutionError::with_source(
                                    ExecutionErrorCode::Unknown,
                                    format!("failed to read shell stderr: {command}"),
                                    e,
                                ));
                            }
                        }
                    }
                }
            }
            let status = child.wait().await.map_err(|e| {
                ExecutionError::with_source(
                    ExecutionErrorCode::SpawnError,
                    format!("failed to wait for shell command: {command}"),
                    e,
                )
            })?;
            Ok::<_, ExecutionError>((output, status.code().unwrap_or(-1)))
        };

        let (output, exit_code) = if let Some(timeout) = options.timeout {
            let timeout = std::time::Duration::from_millis(timeout);
            match tokio::time::timeout(timeout, run).await {
                Ok(result) => result?,
                Err(_) => {
                    // Try to kill the child so it doesn't linger after timeout.
                    let _ = child.kill().await;
                    return Err(ExecutionError::new(
                        ExecutionErrorCode::Timeout,
                        format!("shell command timed out: {command}"),
                    ));
                }
            }
        } else {
            run.await?
        };

        Ok(ExecResult { output, exit_code })
    }

    async fn cleanup(&self) -> Result<(), ExecutionError> {
        // Clean up temp directories
        for dir in &self.temp_dirs {
            if dir.exists() {
                let _ = fs::remove_dir_all(dir).await;
            }
        }
        Ok(())
    }
}

impl ExecutionEnv for NativeExecutionEnv {}

fn map_io_error(error: std::io::Error, path: &Path) -> FileError {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => FileErrorCode::NotFound,
        std::io::ErrorKind::PermissionDenied => FileErrorCode::PermissionDenied,
        std::io::ErrorKind::AlreadyExists => FileErrorCode::Invalid,
        _ => FileErrorCode::Unknown,
    };
    FileError::new(code, format!("{}: {}", path.display(), error))
}

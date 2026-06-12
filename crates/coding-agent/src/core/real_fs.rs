use flown_agent::harness::env::types::*;
use std::path::{Path, PathBuf};
use tokio::fs;

/// Real filesystem implementation using tokio::fs
pub struct RealFileSystem {
    cwd: String,
}

impl RealFileSystem {
    pub fn new() -> Self {
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());
        Self { cwd }
    }
}

impl Default for RealFileSystem {
    fn default() -> Self {
        Self::new()
    }
}

fn resolve_path(base: &str, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        Path::new(base).join(p)
    }
}

#[async_trait::async_trait]
impl FileSystem for RealFileSystem {
    fn cwd(&self) -> &str {
        &self.cwd
    }

    fn absolute_path(&self, path: &str) -> Result<String, FileError> {
        let full = resolve_path(&self.cwd, path);
        if full.exists() {
            std::fs::canonicalize(&full)
                .map(|p| p.to_string_lossy().to_string())
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::NotFound => FileError::new(FileErrorCode::NotFound, path),
                    std::io::ErrorKind::PermissionDenied => {
                        FileError::new(FileErrorCode::PermissionDenied, path)
                    }
                    _ => FileError::new(FileErrorCode::Unknown, path),
                })
        } else {
            // For non-existent paths, normalize without resolving symlinks
            Ok(full.to_string_lossy().to_string())
        }
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
        let full = resolve_path(&self.cwd, path);
        fs::read_to_string(&full).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => FileError::new(FileErrorCode::NotFound, path),
            std::io::ErrorKind::PermissionDenied => {
                FileError::new(FileErrorCode::PermissionDenied, path)
            }
            _ => FileError::new(FileErrorCode::Unknown, path),
        })
    }

    async fn read_text_lines(
        &self,
        path: &str,
        max_lines: Option<usize>,
    ) -> Result<Vec<String>, FileError> {
        let content = self.read_text_file(path).await?;
        let lines: Vec<String> = if let Some(max) = max_lines {
            content.lines().take(max).map(|s| s.to_string()).collect()
        } else {
            content.lines().map(|s| s.to_string()).collect()
        };
        Ok(lines)
    }

    async fn read_binary_file(&self, path: &str) -> Result<Vec<u8>, FileError> {
        let full = resolve_path(&self.cwd, path);
        fs::read(&full).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => FileError::new(FileErrorCode::NotFound, path),
            std::io::ErrorKind::PermissionDenied => {
                FileError::new(FileErrorCode::PermissionDenied, path)
            }
            _ => FileError::new(FileErrorCode::Unknown, path),
        })
    }

    async fn write_file(&self, path: &str, content: &[u8]) -> Result<(), FileError> {
        let full = resolve_path(&self.cwd, path);
        // Create parent directories if needed
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).await.map_err(|e| match e.kind() {
                std::io::ErrorKind::PermissionDenied => {
                    FileError::new(FileErrorCode::PermissionDenied, path)
                }
                _ => FileError::new(FileErrorCode::Unknown, path),
            })?;
        }
        fs::write(&full, content).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::PermissionDenied => {
                FileError::new(FileErrorCode::PermissionDenied, path)
            }
            _ => FileError::new(FileErrorCode::Unknown, path),
        })
    }

    async fn append_file(&self, path: &str, content: &[u8]) -> Result<(), FileError> {
        let full = resolve_path(&self.cwd, path);
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&full)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => FileError::new(FileErrorCode::NotFound, path),
                std::io::ErrorKind::PermissionDenied => {
                    FileError::new(FileErrorCode::PermissionDenied, path)
                }
                _ => FileError::new(FileErrorCode::Unknown, path),
            })?;
        file.write_all(content).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::PermissionDenied => {
                FileError::new(FileErrorCode::PermissionDenied, path)
            }
            _ => FileError::new(FileErrorCode::Unknown, path),
        })
    }

    async fn file_info(&self, path: &str) -> Result<FileInfo, FileError> {
        let full = resolve_path(&self.cwd, path);
        let meta = fs::metadata(&full).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => FileError::new(FileErrorCode::NotFound, path),
            std::io::ErrorKind::PermissionDenied => {
                FileError::new(FileErrorCode::PermissionDenied, path)
            }
            _ => FileError::new(FileErrorCode::Unknown, path),
        })?;
        let kind = if meta.is_dir() {
            FileKind::Directory
        } else if meta.file_type().is_symlink() {
            FileKind::Symlink
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
            name: full
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            path: full.to_string_lossy().to_string(),
            kind,
            size: meta.len(),
            mtime_ms,
        })
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<FileInfo>, FileError> {
        let full = resolve_path(&self.cwd, path);
        let mut entries = fs::read_dir(&full).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => FileError::new(FileErrorCode::NotFound, path),
            std::io::ErrorKind::PermissionDenied => {
                FileError::new(FileErrorCode::PermissionDenied, path)
            }
            std::io::ErrorKind::NotADirectory => FileError::new(FileErrorCode::NotDirectory, path),
            _ => FileError::new(FileErrorCode::Unknown, path),
        })?;
        let mut result = Vec::new();
        while let Some(entry) = entries.next_entry().await.map_err(|e| match e.kind() {
            std::io::ErrorKind::PermissionDenied => {
                FileError::new(FileErrorCode::PermissionDenied, path)
            }
            _ => FileError::new(FileErrorCode::Unknown, path),
        })? {
            let meta = entry.metadata().await;
            if let Ok(meta) = meta {
                let kind = if meta.is_dir() {
                    FileKind::Directory
                } else if meta.file_type().is_symlink() {
                    FileKind::Symlink
                } else {
                    FileKind::File
                };
                let mtime_ms = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                result.push(FileInfo {
                    name: entry.file_name().to_string_lossy().to_string(),
                    path: entry.path().to_string_lossy().to_string(),
                    kind,
                    size: meta.len(),
                    mtime_ms,
                });
            }
        }
        Ok(result)
    }

    async fn canonical_path(&self, path: &str) -> Result<String, FileError> {
        let full = resolve_path(&self.cwd, path);
        fs::canonicalize(&full)
            .await
            .map(|p| p.to_string_lossy().to_string())
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => FileError::new(FileErrorCode::NotFound, path),
                std::io::ErrorKind::PermissionDenied => {
                    FileError::new(FileErrorCode::PermissionDenied, path)
                }
                _ => FileError::new(FileErrorCode::Unknown, path),
            })
    }

    async fn exists(&self, path: &str) -> Result<bool, FileError> {
        let full = resolve_path(&self.cwd, path);
        Ok(full.exists())
    }

    async fn create_dir(&self, path: &str, recursive: bool) -> Result<(), FileError> {
        let full = resolve_path(&self.cwd, path);
        if recursive {
            fs::create_dir_all(&full).await
        } else {
            fs::create_dir(&full).await
        }
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::PermissionDenied => {
                FileError::new(FileErrorCode::PermissionDenied, path)
            }
            _ => FileError::new(FileErrorCode::Unknown, path),
        })
    }

    async fn remove(&self, path: &str, recursive: bool, force: bool) -> Result<(), FileError> {
        let full = resolve_path(&self.cwd, path);
        if !full.exists() {
            if force {
                return Ok(());
            }
            return Err(FileError::new(FileErrorCode::NotFound, path));
        }
        let result = if recursive {
            fs::remove_dir_all(&full).await
        } else {
            match fs::remove_file(&full).await {
                Ok(()) => Ok(()),
                Err(_) => fs::remove_dir(&full).await,
            }
        };
        result.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => FileError::new(FileErrorCode::NotFound, path),
            std::io::ErrorKind::PermissionDenied => {
                FileError::new(FileErrorCode::PermissionDenied, path)
            }
            _ => FileError::new(FileErrorCode::Unknown, path),
        })
    }

    async fn create_temp_dir(&self, prefix: Option<&str>) -> Result<String, FileError> {
        let dir = std::env::temp_dir();
        let prefix = prefix.unwrap_or("tmp-");
        let name = format!(
            "{}{}",
            prefix,
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let path = dir.join(name);
        fs::create_dir_all(&path)
            .await
            .map_err(|_| FileError::new(FileErrorCode::Unknown, ""))?;
        Ok(path.to_string_lossy().to_string())
    }

    async fn create_temp_file(
        &self,
        prefix: Option<&str>,
        suffix: Option<&str>,
    ) -> Result<String, FileError> {
        let dir = std::env::temp_dir();
        let prefix = prefix.unwrap_or("");
        let suffix = suffix.unwrap_or("");
        let name = format!(
            "{}{}{}",
            prefix,
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0),
            suffix
        );
        let path = dir.join(name);
        fs::write(&path, "")
            .await
            .map_err(|_| FileError::new(FileErrorCode::Unknown, ""))?;
        Ok(path.to_string_lossy().to_string())
    }

    async fn cleanup(&self) -> Result<(), FileError> {
        Ok(())
    }
}

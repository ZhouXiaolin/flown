use super::truncate::{DEFAULT_MAX_BYTES, truncate_tail};
use crate::harness::{AbortSignal, ExecOptions, ExecutionEnv, ExecutionError, ExecutionErrorCode};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct ShellCaptureOptions {
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub timeout: Option<u64>,
    pub abort_signal: Option<AbortSignal>,
    pub on_chunk: Option<Arc<dyn Fn(&str) -> Result<(), ExecutionError> + Send + Sync>>,
}

/// Result of shell command capture.
/// Aligned with pi-mono `BashResult` which keeps `truncated: boolean` only.
/// Detailed truncation info is computed at the tool level via `truncate_tail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCaptureResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    pub full_output_path: Option<String>,
}

pub fn sanitize_binary_output(content: &str) -> String {
    content
        .chars()
        .filter(|ch| {
            let code = *ch as u32;
            matches!(code, 0x09 | 0x0a | 0x0d)
                || (code > 0x1f && !(0xfff9..=0xfffb).contains(&code))
        })
        .collect()
}

pub async fn execute_shell_with_capture(
    env: &dyn ExecutionEnv,
    command: &str,
    options: ShellCaptureOptions,
) -> Result<ShellCaptureResult, ExecutionError> {
    let output_chunks = Arc::new(Mutex::new(Vec::<String>::new()));
    let total_bytes = Arc::new(Mutex::new(0usize));
    let capture_error = Arc::new(Mutex::new(None::<ExecutionErrorCode>));

    let on_chunk = {
        let output_chunks = output_chunks.clone();
        let total_bytes = total_bytes.clone();
        let capture_error = capture_error.clone();
        let user_on_chunk = options.on_chunk.clone();
        Arc::new(move |chunk: &str| -> Result<(), ExecutionError> {
            let text = sanitize_binary_output(chunk).replace('\r', "");
            *total_bytes.lock().expect("total bytes lock poisoned") += chunk.len();
            output_chunks
                .lock()
                .expect("output chunks lock poisoned")
                .push(text.clone());
            if let Some(on_chunk) = &user_on_chunk {
                if let Err(error) = on_chunk(&text) {
                    *capture_error.lock().expect("capture error lock poisoned") =
                        Some(error.code.clone());
                    return Err(error);
                }
            }
            Ok(())
        })
    };

    let exec_options = ExecOptions {
        cwd: options.cwd.clone(),
        env: options.env.clone(),
        timeout: options.timeout,
        abort_signal: options.abort_signal.clone(),
        on_stdout: Some(on_chunk.clone()),
        on_stderr: Some(on_chunk),
    };

    let exec_result = env.exec(command, exec_options).await;
    let tail_output = output_chunks
        .lock()
        .expect("output chunks lock poisoned")
        .join("");
    let truncation = truncate_tail(&tail_output, Default::default());
    let output = if truncation.truncated {
        truncation.content
    } else {
        tail_output.clone()
    };

    if let Some(code) = capture_error
        .lock()
        .expect("capture error lock poisoned")
        .clone()
    {
        return Err(ExecutionError::new(code, "shell output capture failed"));
    }

    let total_bytes = *total_bytes.lock().expect("total bytes lock poisoned");
    let mut full_output_path = None;
    if total_bytes > DEFAULT_MAX_BYTES || truncation.truncated {
        let path = env
            .create_temp_file(Some("bash-"), Some(".log"))
            .await
            .map_err(|e| {
                ExecutionError::with_source(
                    ExecutionErrorCode::Unknown,
                    "failed to create temp file",
                    e,
                )
            })?;
        env.append_file(&path, tail_output.as_bytes())
            .await
            .map_err(|e| {
                ExecutionError::with_source(
                    ExecutionErrorCode::Unknown,
                    "failed to write full output log",
                    e,
                )
            })?;
        full_output_path = Some(path);
    }

    match exec_result {
        Ok(result) => {
            let cancelled = options
                .abort_signal
                .as_ref()
                .is_some_and(AbortSignal::is_cancelled);
            Ok(ShellCaptureResult {
                output,
                exit_code: if cancelled {
                    None
                } else {
                    Some(result.exit_code)
                },
                cancelled,
                truncated: truncation.truncated,
                full_output_path,
            })
        }
        Err(error)
            if error.code == ExecutionErrorCode::Aborted
                || options
                    .abort_signal
                    .as_ref()
                    .is_some_and(AbortSignal::is_cancelled) =>
        {
            Ok(ShellCaptureResult {
                output,
                exit_code: None,
                cancelled: true,
                truncated: truncation.truncated,
                full_output_path,
            })
        }
        Err(error) => Err(error),
    }
}

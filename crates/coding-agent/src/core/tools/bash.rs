use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use flown_agent::{ExecOptions, ExecutionEnv, ExecutionErrorCode};
use flown_agent::{AgentTool, AgentToolError, ToolExecutionMode};
use serde_json::{Value, json};

use super::common::*;

static BASH_OUTPUT_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn tool(env: Arc<dyn ExecutionEnv>) -> AgentTool {
    AgentTool {
        name: "bash".to_string(),
        label: "bash".to_string(),
        description: format!(
            "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last {} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
            DEFAULT_MAX_LINES,
            DEFAULT_MAX_BYTES / 1024
        ),
        parameters: json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": { "type": "string", "description": "Bash command to execute" },
                "timeout": { "type": "integer", "minimum": 1, "description": "Timeout in seconds (optional, no default timeout)" }
            }
        }),
        execute: Arc::new(move |_id, args, _abort, _update| {
            let env = env.clone();
            Box::pin(async move {
                let command = required_string(&args, "command")?;
                let timeout = optional_u64(&args, "timeout")?;
                let options = ExecOptions {
                    cwd: optional_string(&args, "cwd")?,
                    env: None,
                    timeout,
                    ..ExecOptions::default()
                };
                let result = match env.exec(&command, options).await {
                    Ok(result) => result,
                    Err(error) if error.code == ExecutionErrorCode::Timeout => {
                        let secs = timeout
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "?".to_string());
                        return Err(AgentToolError::new(format!(
                            "Command timed out after {secs} seconds"
                        )));
                    }
                    Err(error) => return Err(tool_error(error)),
                };
                let output = format!("{}{}", result.stdout, result.stderr);
                if output.is_empty() {
                    return Ok(text_result(
                        String::new(),
                        json!({
                            "command": command,
                            "exit_code": result.exit_code,
                            "stdout": result.stdout,
                            "stderr": result.stderr,
                            "truncation": Value::Null,
                            "fullOutputPath": Value::Null,
                        }),
                    ));
                }
                let formatted = format_bash_output(&output, "(no output)")?;
                if result.exit_code != 0 {
                    return Err(AgentToolError::new(append_status(
                        &formatted.text,
                        &format!("Command exited with code {}", result.exit_code),
                    )));
                }

                Ok(text_result(
                    formatted.text,
                    json!({
                        "command": command,
                        "exit_code": result.exit_code,
                        "stdout": result.stdout,
                        "stderr": result.stderr,
                        "truncation": formatted.truncation,
                        "fullOutputPath": formatted.full_output_path,
                    }),
                ))
            })
        }),
        prepare_arguments: None,
        execution_mode: Some(ToolExecutionMode::Sequential),
    }
}

#[derive(Debug)]
struct FormattedBashOutput {
    text: String,
    truncation: Value,
    full_output_path: Value,
}

fn format_bash_output(
    output: &str,
    empty_text: &str,
) -> Result<FormattedBashOutput, AgentToolError> {
    let truncation = truncate_tail(output);
    if !truncation.truncated {
        return Ok(FormattedBashOutput {
            text: if output.is_empty() {
                empty_text.to_string()
            } else {
                output.to_string()
            },
            truncation: Value::Null,
            full_output_path: Value::Null,
        });
    }

    let full_output_path = persist_full_bash_output(output)?;
    let start_line = truncation
        .total_lines
        .saturating_sub(truncation.output_lines)
        + 1;
    let end_line = truncation.total_lines;
    let mut text = truncation.content.clone();
    if truncation.last_line_partial {
        text.push_str(&format!(
            "\n\n[Showing last {} of line {end_line} (line is {}). Full output: {full_output_path}]",
            format_size(truncation.output_bytes),
            format_size(truncation.last_line_bytes),
        ));
    } else if truncation.truncated_by == "lines" {
        text.push_str(&format!(
            "\n\n[Showing lines {start_line}-{end_line} of {}. Full output: {full_output_path}]",
            truncation.total_lines,
        ));
    } else {
        text.push_str(&format!(
            "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {full_output_path}]",
            truncation.total_lines,
            format_size(DEFAULT_MAX_BYTES),
        ));
    }

    Ok(FormattedBashOutput {
        text,
        truncation: truncation.to_value(false),
        full_output_path: json!(full_output_path),
    })
}

fn persist_full_bash_output(output: &str) -> Result<String, AgentToolError> {
    let id = BASH_OUTPUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("flown-bash-{}-{id}.log", std::process::id()));
    std::fs::write(&path, output).map_err(tool_error)?;
    Ok(path.to_string_lossy().to_string())
}

#[derive(Debug)]
struct TailTruncation {
    content: String,
    truncated: bool,
    truncated_by: &'static str,
    total_lines: usize,
    total_bytes: usize,
    output_lines: usize,
    output_bytes: usize,
    last_line_partial: bool,
    first_line_exceeds_limit: bool,
    last_line_bytes: usize,
}

impl TailTruncation {
    fn to_value(&self, first_line_exceeds_limit: bool) -> Value {
        json!({
            "truncated": self.truncated,
            "truncatedBy": self.truncated_by,
            "totalLines": self.total_lines,
            "totalBytes": self.total_bytes,
            "outputLines": self.output_lines,
            "outputBytes": self.output_bytes,
            "lastLinePartial": self.last_line_partial,
            "firstLineExceedsLimit": first_line_exceeds_limit || self.first_line_exceeds_limit,
            "maxLines": DEFAULT_MAX_LINES,
            "maxBytes": DEFAULT_MAX_BYTES,
        })
    }
}

fn truncate_tail(content: &str) -> TailTruncation {
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();
    let total_bytes = content.len();
    if total_lines <= DEFAULT_MAX_LINES && total_bytes <= DEFAULT_MAX_BYTES {
        return TailTruncation {
            content: content.to_string(),
            truncated: false,
            truncated_by: "none",
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            last_line_bytes: lines.last().map(|line| line.len()).unwrap_or(0),
        };
    }

    let mut selected = Vec::new();
    let mut output_bytes = 0usize;
    let mut truncated_by = "lines";
    let mut last_line_partial = false;

    for line in lines.iter().rev().take(DEFAULT_MAX_LINES) {
        let line_bytes = line.len() + usize::from(!selected.is_empty());
        if output_bytes + line_bytes > DEFAULT_MAX_BYTES {
            truncated_by = "bytes";
            if selected.is_empty() {
                let truncated = truncate_string_to_bytes_from_end(line, DEFAULT_MAX_BYTES);
                output_bytes = truncated.len();
                selected.push(truncated);
                last_line_partial = true;
            }
            break;
        }
        output_bytes += line_bytes;
        selected.push((*line).to_string());
    }
    selected.reverse();

    if selected.len() >= DEFAULT_MAX_LINES && output_bytes <= DEFAULT_MAX_BYTES {
        truncated_by = "lines";
    }
    let output = selected.join("\n");
    let output_bytes = output.len();

    TailTruncation {
        content: output,
        truncated: true,
        truncated_by,
        total_lines,
        total_bytes,
        output_lines: selected.len(),
        output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        last_line_bytes: lines.last().map(|line| line.len()).unwrap_or(0),
    }
}

fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines = content.split('\n').collect::<Vec<_>>();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn truncate_string_to_bytes_from_end(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut start = text.len() - max_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

use std::sync::Arc;

use flown_agent::ExecutionEnv;
use flown_agent::{AgentTool, AgentToolError, AgentToolResult, ToolExecutionMode};
use flown_ai::{ImageContent, ToolResultContent};
use serde_json::{Value, json};

use super::common::*;

pub fn tool(env: Arc<dyn ExecutionEnv>) -> AgentTool {
    AgentTool {
        name: "read".to_string(),
        label: "read".to_string(),
        description: format!(
            "Read the contents of a file. Supports text files and images (jpg, png, gif, webp). Images are sent as attachments. For text files, output is truncated to {} lines or {}KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
            DEFAULT_MAX_LINES,
            DEFAULT_MAX_BYTES / 1024
        ),
        parameters: json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string", "description": "Path to the file to read (relative or absolute)" },
                "offset": { "type": "integer", "minimum": 1, "description": "Line number to start reading from (1-indexed)" },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of lines to read" }
            }
        }),
        execute: Arc::new(move |_id, args, _abort, _update| {
            let env = env.clone();
            Box::pin(async move {
                let path = required_string(&args, "path")?;
                let offset = optional_usize(&args, "offset")?;
                let limit = optional_usize(&args, "limit")?;

                let resolved_path = env.absolute_path(&path).map_err(tool_error)?;
                let bytes = env
                    .read_binary_file(&resolved_path)
                    .await
                    .map_err(tool_error)?;

                // Check if it's an image
                if let Some(mime_type) = detect_image_mime_type(&bytes) {
                    return Ok(AgentToolResult {
                        content: vec![
                            text_block(format!("Read image file [{mime_type}]")),
                            image_block(base64_encode(&bytes), mime_type),
                        ],
                        details: json!({ "path": path }),
                        terminate: None,
                    });
                }

                // Read as text
                let text = String::from_utf8_lossy(&bytes);
                let all_lines: Vec<&str> = text.split('\n').collect();
                let total_file_lines = all_lines.len();

                // Apply offset (convert from 1-indexed to 0-indexed)
                let start_line = offset.map(|o| o.saturating_sub(1)).unwrap_or(0);
                let start_line_display = start_line + 1;

                // Check if offset is out of bounds
                if start_line >= all_lines.len() {
                    return Err(AgentToolError::new(format!(
                        "Offset {} is beyond end of file ({} lines total)",
                        offset.unwrap_or(1),
                        total_file_lines
                    )));
                }

                // Apply user limit or take all lines from start
                let selected_content = if let Some(limit) = limit {
                    let end_line = (start_line + limit).min(all_lines.len());
                    all_lines[start_line..end_line].join("\n")
                } else {
                    all_lines[start_line..].join("\n")
                };

                // Apply truncation (head truncation - keep first N lines/bytes)
                let truncation = truncate_head(&selected_content);
                let mut output_text;
                let mut details = Value::Null;

                if truncation.first_line_exceeds_limit {
                    // First line alone exceeds byte limit
                    let first_line_size = format_size(all_lines[start_line].len());
                    output_text = format!(
                        "[Line {} is {}, exceeds {} limit. Use bash: sed -n '{}p' {} | head -c {}]",
                        start_line_display,
                        first_line_size,
                        format_size(DEFAULT_MAX_BYTES),
                        start_line_display,
                        path,
                        DEFAULT_MAX_BYTES
                    );
                    details = json!({ "truncation": truncation.to_value() });
                } else if truncation.truncated {
                    // Truncation occurred
                    let truncation_value = truncation.to_value();
                    let end_line_display = start_line_display + truncation.output_lines - 1;
                    let next_offset = end_line_display + 1;
                    output_text = truncation.content;

                    if truncation.truncated_by == "lines" {
                        output_text.push_str(&format!(
                            "\n\n[Showing lines {}-{} of {}. Use offset={} to continue.]",
                            start_line_display, end_line_display, total_file_lines, next_offset
                        ));
                    } else {
                        output_text.push_str(&format!(
                            "\n\n[Showing lines {}-{} of {} ({} limit). Use offset={} to continue.]",
                            start_line_display, end_line_display, total_file_lines,
                            format_size(DEFAULT_MAX_BYTES), next_offset
                        ));
                    }
                    details = json!({ "truncation": truncation_value });
                } else if let Some(user_limit) = limit {
                    let end_line = start_line + user_limit;
                    if end_line < all_lines.len() {
                        // User-specified limit stopped early, file has more content
                        let remaining = all_lines.len() - end_line;
                        let next_offset = end_line + 1;
                        output_text = truncation.content;
                        output_text.push_str(&format!(
                            "\n\n[{} more lines in file. Use offset={} to continue.]",
                            remaining, next_offset
                        ));
                    } else {
                        output_text = truncation.content;
                    }
                } else {
                    output_text = truncation.content;
                }

                Ok(AgentToolResult {
                    content: vec![text_block(output_text)],
                    details: json!({ "path": path, "truncation": details }),
                    terminate: None,
                })
            })
        }),
        prepare_arguments: None,
        execution_mode: Some(ToolExecutionMode::Parallel),
    }
}

/// Truncation result
struct TruncationResult {
    content: String,
    truncated: bool,
    truncated_by: &'static str,
    total_lines: usize,
    total_bytes: usize,
    output_lines: usize,
    output_bytes: usize,
    first_line_exceeds_limit: bool,
}

impl TruncationResult {
    fn to_value(&self) -> Value {
        json!({
            "truncated": self.truncated,
            "truncatedBy": self.truncated_by,
            "totalLines": self.total_lines,
            "totalBytes": self.total_bytes,
            "outputLines": self.output_lines,
            "outputBytes": self.output_bytes,
            "firstLineExceedsLimit": self.first_line_exceeds_limit,
            "maxLines": DEFAULT_MAX_LINES,
            "maxBytes": DEFAULT_MAX_BYTES,
        })
    }
}

/// Truncate content from the head (keep first N lines/bytes).
/// Suitable for file reads where you want to see the beginning.
fn truncate_head(content: &str) -> TruncationResult {
    let total_bytes = content.len();
    let lines: Vec<&str> = content.split('\n').collect();
    let total_lines = lines.len();

    // Check if no truncation needed
    if total_lines <= DEFAULT_MAX_LINES && total_bytes <= DEFAULT_MAX_BYTES {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: "none",
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            first_line_exceeds_limit: false,
        };
    }

    // Check if first line alone exceeds byte limit
    let first_line_bytes = lines[0].len();
    if first_line_bytes > DEFAULT_MAX_BYTES {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: "bytes",
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            first_line_exceeds_limit: true,
        };
    }

    // Collect complete lines that fit
    let mut output_lines_vec = Vec::new();
    let mut output_bytes_count = 0;
    let mut truncated_by = "lines";

    for (i, line) in lines.iter().enumerate().take(DEFAULT_MAX_LINES) {
        let line_bytes = line.len() + usize::from(i > 0); // +1 for newline

        if output_bytes_count + line_bytes > DEFAULT_MAX_BYTES {
            truncated_by = "bytes";
            break;
        }

        output_lines_vec.push(*line);
        output_bytes_count += line_bytes;
    }

    // If we exited due to line limit
    if output_lines_vec.len() >= DEFAULT_MAX_LINES && output_bytes_count <= DEFAULT_MAX_BYTES {
        truncated_by = "lines";
    }

    let output_content = output_lines_vec.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by,
        total_lines,
        total_bytes,
        output_lines: output_lines_vec.len(),
        output_bytes: final_output_bytes,
        first_line_exceeds_limit: false,
    }
}

fn detect_image_mime_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn image_block(data: String, mime_type: &str) -> ToolResultContent {
    ToolResultContent::Image(ImageContent {
        content_type: "image".to_string(),
        data,
        mime_type: mime_type.to_string(),
    })
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        let value = ((first as u32) << 16) | ((second as u32) << 8) | third as u32;

        output.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
        output.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[((value >> 6) & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(value & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

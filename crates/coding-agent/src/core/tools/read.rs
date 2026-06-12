use std::sync::Arc;

use flown_agent::harness::env::types::ExecutionEnv;
use flown_agent::types::{AgentTool, AgentToolError, AgentToolResult, ToolExecutionMode};
use flown_ai::types::{ImageContent, ToolResultContent};
use serde_json::{Value, json};

use super::common::*;

pub fn tool(env: Arc<dyn ExecutionEnv>) -> AgentTool {
    AgentTool {
        name: "read".to_string(),
        label: "Read".to_string(),
        description: "Read a UTF-8 text file through the session filesystem".to_string(),
        parameters: json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string" },
                "offset": { "type": "integer", "minimum": 1 },
                "limit": { "type": "integer", "minimum": 1 }
            }
        }),
        execute: Arc::new(move |_id, args, _abort, _update| {
            let env = env.clone();
            Box::pin(async move {
                let path = required_string(&args, "path")?;

                let resolved_path = env.absolute_path(&path).map_err(tool_error)?;
                let offset = optional_usize(&args, "offset")?;
                let limit = optional_usize(&args, "limit")?;
                let bytes = env
                    .read_binary_file(&resolved_path)
                    .await
                    .map_err(tool_error)?;
                if let Some(mime_type) = detect_image_mime_type(&bytes) {
                    return Ok(AgentToolResult {
                        content: vec![
                            text_block(format!("Read image file [{mime_type}]")),
                            image_block(base64_encode(&bytes), mime_type),
                        ],
                        details: read_details_with_path(Value::Null, &path),
                        terminate: None,
                    });
                }

                let text = String::from_utf8_lossy(&bytes);
                let read = read_text_slice(&path, &text, offset, limit)?;

                Ok(AgentToolResult {
                    content: vec![text_block(read.text)],
                    details: read_details_with_path(read.details, &path),
                    terminate: None,
                })
            })
        }),
        prepare_arguments: None,
        execution_mode: Some(ToolExecutionMode::Parallel),
    }
}

#[derive(Debug)]
struct ReadSlice {
    text: String,
    details: Value,
}

fn read_text_slice(
    path: &str,
    text: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ReadSlice, AgentToolError> {
    let lines: Vec<&str> = text.split('\n').collect();
    let total_lines = lines.len();
    let total_bytes = text.len();
    let start = offset.unwrap_or(1);
    if start == 0 {
        return Err(AgentToolError::new("offset must be 1 or greater"));
    }
    if start > total_lines {
        return Err(AgentToolError::new(format!(
            "Offset {start} is beyond end of file ({total_lines} lines total)"
        )));
    }

    let start_index = start - 1;
    let available = total_lines - start_index;
    let max_lines = limit.unwrap_or(DEFAULT_MAX_LINES).min(DEFAULT_MAX_LINES);
    let requested = max_lines.min(available);
    let mut output_count = 0usize;
    let mut output_bytes = 0usize;
    let mut truncated_by = None;

    for (index, line) in lines[start_index..start_index + requested]
        .iter()
        .enumerate()
    {
        let line_bytes = line.len() + usize::from(index > 0);
        if output_bytes + line_bytes > DEFAULT_MAX_BYTES {
            truncated_by = Some("bytes");
            break;
        }
        output_count += 1;
        output_bytes += line_bytes;
    }

    if output_count == 0 && requested > 0 {
        let first_line_bytes = lines[start_index].len();
        let text = format!(
            "[Line {start} is {}, exceeds {} limit. Use bash: sed -n '{start}p' {path} | head -c {DEFAULT_MAX_BYTES}]",
            format_size(first_line_bytes),
            format_size(DEFAULT_MAX_BYTES),
        );
        return Ok(ReadSlice {
            text,
            details: json!({
                "truncation": truncation_details(
                    total_lines,
                    total_bytes,
                    0,
                    0,
                    "bytes",
                    true,
                )
            }),
        });
    }

    if output_count == requested && output_count < available && requested == DEFAULT_MAX_LINES {
        truncated_by = Some("lines");
    }

    let mut selected = lines[start_index..start_index + output_count].join("\n");
    let next_offset = start + output_count;
    let mut details = Value::Null;
    if let Some(truncated_by) = truncated_by {
        let end_line = next_offset.saturating_sub(1);
        let limit_note = if truncated_by == "bytes" {
            format!(" ({}) limit", format_size(DEFAULT_MAX_BYTES))
        } else {
            String::new()
        };
        selected.push_str(&format!(
            "\n\n[Showing lines {start}-{end_line} of {total_lines}{limit_note}. Use offset={next_offset} to continue.]"
        ));
        details = json!({
            "truncation": truncation_details(
                total_lines,
                text.len(),
                output_count,
                output_bytes,
                truncated_by,
                false,
            )
        });
    } else if next_offset <= total_lines {
        let remaining = total_lines - next_offset + 1;
        selected.push_str(&format!(
            "\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]"
        ));
    }

    Ok(ReadSlice {
        text: selected,
        details,
    })
}

fn truncation_details(
    total_lines: usize,
    total_bytes: usize,
    output_lines: usize,
    output_bytes: usize,
    truncated_by: &str,
    first_line_exceeds_limit: bool,
) -> Value {
    json!({
        "truncated": true,
        "truncatedBy": truncated_by,
        "totalLines": total_lines,
        "totalBytes": total_bytes,
        "outputLines": output_lines,
        "outputBytes": output_bytes,
        "lastLinePartial": false,
        "firstLineExceedsLimit": first_line_exceeds_limit,
        "maxLines": DEFAULT_MAX_LINES,
        "maxBytes": DEFAULT_MAX_BYTES,
    })
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

fn read_details_with_path(details: Value, path: &str) -> Value {
    match details {
        Value::Object(mut object) => {
            object.insert("path".to_string(), Value::String(path.to_string()));
            Value::Object(object)
        }
        _ => json!({ "path": path }),
    }
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

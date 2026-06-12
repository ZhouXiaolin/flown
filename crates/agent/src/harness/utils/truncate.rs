pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
pub const GREP_MAX_LINE_LENGTH: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TruncationLimit {
    Lines,
    Bytes,
}

#[derive(Debug, Clone)]
pub struct TruncationOptions {
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
}

impl Default for TruncationOptions {
    fn default() -> Self {
        Self {
            max_lines: Some(DEFAULT_MAX_LINES),
            max_bytes: Some(DEFAULT_MAX_BYTES),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub truncated_by: Option<TruncationLimit>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineTruncationResult {
    pub text: String,
    pub was_truncated: bool,
}

pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let total_bytes = utf8_byte_len(content);
    let lines: Vec<&str> = content.split('\n').collect();
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    if utf8_byte_len(lines.first().copied().unwrap_or("")) > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncationLimit::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    let mut output_lines = Vec::new();
    let mut output_bytes = 0;
    let mut truncated_by = TruncationLimit::Lines;

    for (index, line) in lines.iter().enumerate().take(max_lines) {
        let line_bytes = utf8_byte_len(line) + usize::from(index > 0);
        if output_bytes + line_bytes > max_bytes {
            truncated_by = TruncationLimit::Bytes;
            break;
        }
        output_lines.push(*line);
        output_bytes += line_bytes;
    }

    if output_lines.len() >= max_lines && output_bytes <= max_bytes {
        truncated_by = TruncationLimit::Lines;
    }

    let output = output_lines.join("\n");
    let output_bytes = utf8_byte_len(&output);
    TruncationResult {
        content: output,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines.len(),
        output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let total_bytes = utf8_byte_len(content);
    let mut lines: Vec<&str> = content.split('\n').collect();
    if lines.len() > 1 && lines.last() == Some(&"") {
        lines.pop();
    }
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    let mut output_lines = Vec::new();
    let mut output_bytes = 0;
    let mut truncated_by = TruncationLimit::Lines;
    let mut last_line_partial = false;

    for line in lines.iter().rev().take(max_lines) {
        let line_bytes = utf8_byte_len(line) + usize::from(!output_lines.is_empty());
        if output_bytes + line_bytes > max_bytes {
            truncated_by = TruncationLimit::Bytes;
            if output_lines.is_empty() {
                let truncated = truncate_string_to_bytes_from_end(line, max_bytes);
                output_bytes = utf8_byte_len(&truncated);
                output_lines.insert(0, truncated);
                last_line_partial = true;
            }
            break;
        }
        output_lines.insert(0, (*line).to_string());
        output_bytes += line_bytes;
    }

    if output_lines.len() >= max_lines && output_bytes <= max_bytes {
        truncated_by = TruncationLimit::Lines;
    }

    let output = output_lines.join("\n");
    let output_bytes = utf8_byte_len(&output);
    TruncationResult {
        content: output,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines.len(),
        output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

pub fn truncate_line(line: &str, max_chars: Option<usize>) -> LineTruncationResult {
    let max_chars = max_chars.unwrap_or(GREP_MAX_LINE_LENGTH);
    if line.chars().count() <= max_chars {
        return LineTruncationResult {
            text: line.to_string(),
            was_truncated: false,
        };
    }
    let prefix: String = line.chars().take(max_chars).collect();
    LineTruncationResult {
        text: format!("{prefix}... [truncated]"),
        was_truncated: true,
    }
}

fn utf8_byte_len(content: &str) -> usize {
    content.len()
}

fn truncate_string_to_bytes_from_end(content: &str, max_bytes: usize) -> String {
    if max_bytes == 0 {
        return String::new();
    }
    let mut bytes = 0;
    let mut start = content.len();
    for (index, ch) in content.char_indices().rev() {
        let len = ch.len_utf8();
        if bytes + len > max_bytes {
            break;
        }
        bytes += len;
        start = index;
    }
    content[start..].to_string()
}

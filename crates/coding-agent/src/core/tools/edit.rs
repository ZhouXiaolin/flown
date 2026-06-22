use std::sync::Arc;

use flown_agent::ExecutionEnv;
use flown_agent::{AgentTool, AgentToolError, AgentToolResult, FileErrorCode, ToolExecutionMode};
use serde_json::{Value, json};

use super::common::*;

pub fn tool(env: Arc<dyn ExecutionEnv>) -> AgentTool {
    AgentTool {
        name: "edit".to_string(),
        label: "edit".to_string(),
        description: "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.".to_string(),
        parameters: json!({
            "type": "object",
            "required": ["path", "edits"],
            "additionalProperties": false,
            "properties": {
                "path": { "type": "string", "description": "Path to the file to edit (relative or absolute)" },
                "edits": {
                    "type": "array",
                    "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits.",
                    "items": {
                        "type": "object",
                        "required": ["oldText", "newText"],
                        "additionalProperties": false,
                        "properties": {
                            "oldText": { "type": "string", "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call." },
                            "newText": { "type": "string", "description": "Replacement text for this targeted edit." }
                        }
                    }
                }
            }
        }),
        execute: Arc::new(move |_id, args, _abort, _update| {
            let env = env.clone();
            Box::pin(async move {
                let path = required_string(&args, "path")?;
                let resolved_path = env.absolute_path(&path).map_err(tool_error)?;
                let edits = required_edits(&args)?;

                // Pre-check readability before attempting to read, so a missing
                // or unreadable file reports a clear "Could not edit file"
                // error instead of an opaque read failure. Mirrors pi-mono's
                // `access(R_OK | W_OK)` guard.
                if let Err(error) = env.file_info(&resolved_path).await {
                    return Err(AgentToolError::new(format!(
                        "Could not edit file: {path}. Error code: {}.",
                        error_code_name(&error)
                    )));
                }

                let content = env
                    .read_text_file(&resolved_path)
                    .await
                    .map_err(tool_error)?;
                let (bom, content_without_bom) = strip_bom(&content);
                let line_ending = detect_line_ending(content_without_bom);
                let normalized = normalize_to_lf(content_without_bom);
                let applied = apply_edits_to_normalized_content(&normalized, &edits, &path)?;
                let final_content =
                    format!("{bom}{}", restore_line_endings(&applied.new_content, line_ending));
                env.write_file(&resolved_path, final_content.as_bytes())
                    .await
                    .map_err(tool_error)?;
                let diff_result = generate_diff_string(&applied.base_content, &applied.new_content);
                let patch = generate_unified_patch(&path, &applied.base_content, &applied.new_content);
                let first_changed_line = diff_result.first_changed_line;

                Ok(AgentToolResult {
                    content: vec![text_block(format!(
                        "Successfully replaced {} block(s) in {path}.",
                        edits.len()
                    ))],
                    details: json!({
                        "diff": diff_result.diff,
                        "patch": patch,
                        "firstChangedLine": first_changed_line,
                    }),
                    terminate: None,
                })
            })
        }),
        prepare_arguments: Some(Arc::new(prepare_edit_arguments)),
        execution_mode: Some(ToolExecutionMode::Sequential),
    }
}

#[derive(Debug, Clone)]
struct Edit {
    old_text: String,
    new_text: String,
}

/// Map a file error code to the Node.js-style `error.code` string that
/// pi-mono surfaces via `access()`. Used so edit's pre-check failure message
/// reads `Could not edit file: {path}. Error code: ENOENT.` etc.
fn error_code_name(error: &flown_agent::FileError) -> &'static str {
    match error.code {
        FileErrorCode::NotFound => "ENOENT",
        FileErrorCode::PermissionDenied => "EACCES",
        FileErrorCode::IsDirectory => "EISDIR",
        FileErrorCode::NotDirectory => "ENOTDIR",
        FileErrorCode::Invalid => "EINVAL",
        FileErrorCode::NotSupported => "ENOTSUP",
        FileErrorCode::Aborted => "EABORT",
        FileErrorCode::Unknown => "UNKNOWN",
    }
}

#[derive(Debug)]
struct AppliedEdits {
    base_content: String,
    new_content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
    Lf,
    Crlf,
}

fn required_edits(args: &Value) -> Result<Vec<Edit>, AgentToolError> {
    let edits = args.get("edits").and_then(Value::as_array).ok_or_else(|| {
        AgentToolError::new(
            "Edit tool input is invalid. edits must contain at least one replacement.",
        )
    })?;
    if edits.is_empty() {
        return Err(AgentToolError::new(
            "Edit tool input is invalid. edits must contain at least one replacement.",
        ));
    }

    edits
        .iter()
        .map(|edit| {
            let old_text = required_string(edit, "oldText")?;
            let new_text = required_string(edit, "newText")?;
            Ok(Edit { old_text, new_text })
        })
        .collect()
}

fn apply_edits_to_normalized_content(
    content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEdits, AgentToolError> {
    let normalized_edits = edits
        .iter()
        .map(|edit| Edit {
            old_text: normalize_to_lf(&edit.old_text),
            new_text: normalize_to_lf(&edit.new_text),
        })
        .collect::<Vec<_>>();
    let use_fuzzy_content = normalized_edits
        .iter()
        .any(|edit| content.find(&edit.old_text).is_none());
    let base_content = if use_fuzzy_content {
        normalize_for_fuzzy_match(content)
    } else {
        content.to_string()
    };

    let mut ranges = Vec::with_capacity(edits.len());
    for (index, edit) in normalized_edits.iter().enumerate() {
        let old_text = if use_fuzzy_content {
            normalize_for_fuzzy_match(&edit.old_text)
        } else {
            edit.old_text.clone()
        };
        if old_text.is_empty() {
            return Err(AgentToolError::new(if normalized_edits.len() == 1 {
                format!("oldText must not be empty in {path}.")
            } else {
                format!("edits[{index}].oldText must not be empty in {path}.")
            }));
        }
        let matches: Vec<_> = base_content.match_indices(&old_text).collect();
        if matches.is_empty() {
            let message = if normalized_edits.len() == 1 {
                format!(
                    "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
                )
            } else {
                format!(
                    "Could not find edits[{index}] in {path}. The oldText must match exactly including all whitespace and newlines."
                )
            };
            return Err(AgentToolError::new(message));
        }
        if matches.len() > 1 {
            let message = if normalized_edits.len() == 1 {
                format!(
                    "Found {} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique.",
                    matches.len()
                )
            } else {
                format!(
                    "Found {} occurrences of edits[{index}] in {path}. Each oldText must be unique. Please provide more context to make it unique.",
                    matches.len()
                )
            };
            return Err(AgentToolError::new(message));
        }
        let start = matches[0].0;
        let end = start + old_text.len();
        ranges.push((index, start, end, edit.new_text.clone()));
    }
    ranges.sort_by_key(|(_, start, _, _)| *start);
    for window in ranges.windows(2) {
        if window[0].2 > window[1].1 {
            return Err(AgentToolError::new(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                window[0].0, window[1].0,
            )));
        }
    }

    let mut output = String::with_capacity(base_content.len());
    let mut cursor = 0;
    for (_, start, end, new_text) in &ranges {
        output.push_str(&base_content[cursor..*start]);
        output.push_str(new_text);
        cursor = *end;
    }
    output.push_str(&base_content[cursor..]);

    if output == base_content {
        return Err(AgentToolError::new(if normalized_edits.len() == 1 {
            format!(
                "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
            )
        } else {
            format!("No changes made to {path}. The replacements produced identical content.")
        }));
    }

    Ok(AppliedEdits {
        base_content,
        new_content: output,
    })
}

fn strip_bom(content: &str) -> (&str, &str) {
    content
        .strip_prefix('\u{feff}')
        .map(|text| ("\u{feff}", text))
        .unwrap_or(("", content))
}

fn detect_line_ending(content: &str) -> LineEnding {
    match (content.find("\r\n"), content.find('\n')) {
        (Some(crlf), Some(lf)) if crlf <= lf => LineEnding::Crlf,
        _ => LineEnding::Lf,
    }
}

fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn restore_line_endings(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Lf => text.to_string(),
        LineEnding::Crlf => text.replace('\n', "\r\n"),
    }
}

fn normalize_for_fuzzy_match(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;

    // NFKC first (compatibility decomposition + canonical composition), so
    // ligatures (ﬁ→fi), fullwidth Latin, superscripts (²→2) etc. collapse to
    // their ASCII equivalents — mirrors JS `String.prototype.normalize("NFKC")`.
    text.nfkc()
        .collect::<String>()
        .split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .replace(['\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'], "'")
        .replace(['\u{201c}', '\u{201d}', '\u{201e}', '\u{201f}'], "\"")
        .replace(
            [
                '\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}',
            ],
            "-",
        )
        .replace(
            [
                '\u{00a0}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}',
                '\u{2008}', '\u{2009}', '\u{200a}', '\u{202f}', '\u{205f}', '\u{3000}',
            ],
            " ",
        )
}

/// Generate a standard unified patch with 4 lines of context.
///
/// Mirrors pi-mono's `generateUnifiedPatch`, which calls
/// `Diff.createTwoFilesPatch(path, path, old, new, ..., { context: 4,
/// headerOptions: FILE_HEADERS_ONLY })`.
fn generate_unified_patch(path: &str, old: &str, new: &str) -> String {
    similar::udiff::unified_diff(
        similar::Algorithm::default(),
        old,
        new,
        4,
        Some((path, path)),
    )
}

const DIFF_CONTEXT_LINES: usize = 4;

/// Result of a display-oriented diff: the formatted string and the line number
/// of the first changed line in the new file (1-indexed).
struct DiffResult {
    diff: String,
    first_changed_line: Option<usize>,
}

/// Generate a display-oriented diff string with line numbers and folded context.
///
/// Mirrors pi-mono's `generateDiffString`: walks the line-level diff ops,
/// prefixes added/removed lines with their new/old line numbers, and shows only
/// `DIFF_CONTEXT_LINES` of surrounding equal text around each change (folding
/// the rest into `...`). Equal runs that are not adjacent to a change are
/// dropped entirely.
fn generate_diff_string(old_content: &str, new_content: &str) -> DiffResult {
    let old_lines: Vec<&str> = old_content.split('\n').collect();
    let new_lines: Vec<&str> = new_content.split('\n').collect();
    let max_line_num = old_lines.len().max(new_lines.len());
    let line_num_width = max_line_num.to_string().len();

    // Build a part list analogous to jsdiff's diffLines output: each part is
    // (is_added, is_removed, raw_lines). Replace ops are split into a removed
    // part immediately followed by an added part, matching jsdiff ordering.
    let diff = similar::TextDiff::from_lines(old_content, new_content);
    struct Part {
        added: bool,
        removed: bool,
        lines: Vec<String>,
    }
    let mut parts: Vec<Part> = Vec::new();
    for op in diff.ops() {
        match op {
            similar::DiffOp::Equal { .. } => {
                let range = op.old_range();
                let lines = old_lines[range.start..range.end]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                parts.push(Part {
                    added: false,
                    removed: false,
                    lines,
                });
            }
            similar::DiffOp::Insert { .. } => {
                let range = op.new_range();
                let lines = new_lines[range.start..range.end]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                parts.push(Part {
                    added: true,
                    removed: false,
                    lines,
                });
            }
            similar::DiffOp::Delete { .. } => {
                let range = op.old_range();
                let lines = old_lines[range.start..range.end]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                parts.push(Part {
                    added: false,
                    removed: true,
                    lines,
                });
            }
            similar::DiffOp::Replace { .. } => {
                let old_range = op.old_range();
                let new_range = op.new_range();
                let removed_lines = old_lines[old_range.start..old_range.end]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                parts.push(Part {
                    added: false,
                    removed: true,
                    lines: removed_lines,
                });
                let added_lines = new_lines[new_range.start..new_range.end]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                parts.push(Part {
                    added: true,
                    removed: false,
                    lines: added_lines,
                });
            }
        }
    }

    let mut output: Vec<String> = Vec::new();
    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    for (i, part) in parts.iter().enumerate() {
        let is_change = part.added || part.removed;
        if is_change && first_changed_line.is_none() {
            first_changed_line = Some(new_line_num);
        }

        if is_change {
            for line in &part.lines {
                let num_str = if part.added {
                    let n = new_line_num;
                    new_line_num += 1;
                    n
                } else {
                    let n = old_line_num;
                    old_line_num += 1;
                    n
                };
                let sign = if part.added { '+' } else { '-' };
                output.push(format!(
                    "{sign}{num:>line_num_width$} {line}",
                    num = num_str,
                    line = line,
                    line_num_width = line_num_width,
                ));
            }
            last_was_change = true;
        } else {
            let next_is_change =
                i + 1 < parts.len() && (parts[i + 1].added || parts[i + 1].removed);
            let has_leading = last_was_change;
            let has_trailing = next_is_change;
            let raw = &part.lines;
            let raw_len = raw.len();

            if has_leading && has_trailing {
                if raw_len <= DIFF_CONTEXT_LINES * 2 {
                    for line in raw {
                        let num = old_line_num;
                        output.push(format!(
                            " {num:>line_num_width$} {line}",
                            num = num,
                            line = line,
                            line_num_width = line_num_width,
                        ));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                } else {
                    let leading = &raw[..DIFF_CONTEXT_LINES];
                    let trailing = &raw[raw_len - DIFF_CONTEXT_LINES..];
                    let skipped = raw_len - leading.len() - trailing.len();
                    for line in leading {
                        let num = old_line_num;
                        output.push(format!(
                            " {num:>line_num_width$} {line}",
                            num = num,
                            line = line,
                            line_num_width = line_num_width,
                        ));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                    output.push(format!(
                        " {:>line_num_width$} ...",
                        "",
                        line_num_width = line_num_width,
                    ));
                    old_line_num += skipped;
                    new_line_num += skipped;
                    for line in trailing {
                        let num = old_line_num;
                        output.push(format!(
                            " {num:>line_num_width$} {line}",
                            num = num,
                            line = line,
                            line_num_width = line_num_width,
                        ));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                }
            } else if has_leading {
                let shown = &raw[..raw_len.min(DIFF_CONTEXT_LINES)];
                let skipped = raw_len - shown.len();
                for line in shown {
                    let num = old_line_num;
                    output.push(format!(
                        " {num:>line_num_width$} {line}",
                        num = num,
                        line = line,
                        line_num_width = line_num_width,
                    ));
                    old_line_num += 1;
                    new_line_num += 1;
                }
                if skipped > 0 {
                    output.push(format!(
                        " {:>line_num_width$} ...",
                        "",
                        line_num_width = line_num_width,
                    ));
                    old_line_num += skipped;
                    new_line_num += skipped;
                }
            } else if has_trailing {
                let skipped = raw_len.saturating_sub(DIFF_CONTEXT_LINES);
                if skipped > 0 {
                    output.push(format!(
                        " {:>line_num_width$} ...",
                        "",
                        line_num_width = line_num_width,
                    ));
                    old_line_num += skipped;
                    new_line_num += skipped;
                }
                for line in &raw[skipped..] {
                    let num = old_line_num;
                    output.push(format!(
                        " {num:>line_num_width$} {line}",
                        num = num,
                        line = line,
                        line_num_width = line_num_width,
                    ));
                    old_line_num += 1;
                    new_line_num += 1;
                }
            } else {
                old_line_num += raw_len;
                new_line_num += raw_len;
            }
            last_was_change = false;
        }
    }

    DiffResult {
        diff: output.join("\n"),
        first_changed_line,
    }
}

fn prepare_edit_arguments(input: Value) -> Value {
    let Value::Object(mut args) = input else {
        return input;
    };

    if let Some(Value::String(edits)) = args.get("edits")
        && let Ok(parsed) = serde_json::from_str::<Value>(edits)
        && parsed.as_array().is_some()
    {
        args.insert("edits".to_string(), parsed);
    }

    let old_text = args.remove("oldText");
    let new_text = args.remove("newText");
    match (old_text, new_text) {
        (Some(Value::String(old_text)), Some(Value::String(new_text))) => {
            let mut edits = match args.remove("edits") {
                Some(Value::Array(edits)) => edits,
                _ => Vec::new(),
            };
            edits.push(json!({ "oldText": old_text, "newText": new_text }));
            args.insert("edits".to_string(), Value::Array(edits));
            Value::Object(args)
        }
        (old_text, new_text) => {
            if let Some(old_text) = old_text {
                args.insert("oldText".to_string(), old_text);
            }
            if let Some(new_text) = new_text {
                args.insert("newText".to_string(), new_text);
            }
            Value::Object(args)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_diff_string_marks_added_and_removed_lines() {
        let result = generate_diff_string("a\nb\nc", "a\nB\nc");
        // b → B is a replace: removed line 2 then added line 2.
        assert!(result.diff.contains("-2 b"));
        assert!(result.diff.contains("+2 B"));
        // First change is on new-file line 2.
        assert_eq!(result.first_changed_line, Some(2));
    }

    #[test]
    fn generate_diff_string_folds_context_between_changes() {
        // Two changes separated by >2*DIFF_CONTEXT_LINES equal lines.
        let mut old = String::new();
        let mut new = String::new();
        for i in 1..=20 {
            old.push_str(&format!("line{i}\n"));
            new.push_str(&format!("line{i}\n"));
        }
        // Change at line 1 and line 20.
        let old = format!("OLD1\n{}", &old[6..]);
        let new = format!("NEW1\n{}", &new[6..]);
        let old = format!("{old}{}", old.replace("line20", "OLD20"));
        let new = format!("{new}{}", new.replace("line20", "NEW20"));

        let result = generate_diff_string(&old, &new);
        assert!(
            result.diff.contains("..."),
            "folded context should appear: {}",
            result.diff
        );
    }

    #[test]
    fn generate_unified_patch_has_standard_headers() {
        let patch = generate_unified_patch("file.txt", "a\nb", "a\nB");
        assert!(patch.starts_with("--- file.txt\n+++ file.txt\n"));
        assert!(patch.contains("@@"));
    }

    #[test]
    fn normalize_for_fuzzy_match_applies_nfkc_to_ligatures() {
        // U+FB01 LATIN SMALL LIGATURE FI should collapse to "fi" under NFKC.
        assert_eq!(normalize_for_fuzzy_match("eﬀect"), "effect");
    }

    #[test]
    fn normalize_for_fuzzy_match_strips_trailing_whitespace() {
        // split('\n').map(trimEnd).join('\n') keeps a trailing empty element as
        // a trailing newline, matching pi-mono exactly.
        assert_eq!(normalize_for_fuzzy_match("a   \nb \n"), "a\nb\n");
        assert_eq!(normalize_for_fuzzy_match("a   \nb "), "a\nb");
    }

    #[test]
    fn error_code_name_maps_not_found_to_enoent() {
        let err = flown_agent::FileError::new(FileErrorCode::NotFound, "/x");
        assert_eq!(error_code_name(&err), "ENOENT");
        let err = flown_agent::FileError::new(FileErrorCode::PermissionDenied, "/x");
        assert_eq!(error_code_name(&err), "EACCES");
    }
}

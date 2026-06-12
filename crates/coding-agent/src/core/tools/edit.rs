use std::sync::Arc;

use flown_agent::harness::env::types::ExecutionEnv;
use flown_agent::types::{AgentTool, AgentToolError, AgentToolResult, ToolExecutionMode};
use serde_json::{Value, json};

use super::common::*;

pub fn tool(env: Arc<dyn ExecutionEnv>) -> AgentTool {
    AgentTool {
        name: "edit".to_string(),
        label: "Edit".to_string(),
        description:
            "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file."
                .to_string(),
        parameters: json!({
            "type": "object",
            "required": ["path", "edits"],
            "additionalProperties": false,
            "properties": {
                "path": { "type": "string" },
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["oldText", "newText"],
                        "additionalProperties": false,
                        "properties": {
                            "oldText": { "type": "string" },
                            "newText": { "type": "string" }
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
                let diff = simple_diff(&applied.base_content, &applied.new_content);
                let patch = generate_unified_patch(&path, &applied.base_content, &applied.new_content);
                let first_changed_line =
                    first_changed_line(&applied.base_content, &applied.new_content);

                Ok(AgentToolResult {
                    content: vec![text_block(format!(
                        "Successfully replaced {} block(s) in {path}.",
                        edits.len()
                    ))],
                    details: json!({
                        "diff": diff,
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
    text.split('\n')
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

fn generate_unified_patch(path: &str, old: &str, new: &str) -> String {
    format!("--- {path}\n+++ {path}\n@@\n-{old}\n+{new}")
}

fn first_changed_line(old: &str, new: &str) -> Option<usize> {
    old.split('\n')
        .zip(new.split('\n'))
        .position(|(old, new)| old != new)
        .map(|index| index + 1)
        .or_else(|| {
            let old_count = old.split('\n').count();
            let new_count = new.split('\n').count();
            (old_count != new_count).then_some(old_count.min(new_count) + 1)
        })
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

fn simple_diff(old: &str, new: &str) -> String {
    format!("--- original\n+++ modified\n@@\n-{old}\n+{new}")
}

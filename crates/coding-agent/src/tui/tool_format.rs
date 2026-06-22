//! Pure helpers that format tool calls for transcript display.
//!
//! Moved verbatim from the old `interactive_mode.rs` (Phase 4 of the previous
//! hand-written TUI). These are self-contained — the only dependencies are
//! `serde_json` and `similar`, both crate-level. The `<diff>…</diff>` markers
//! they emit are rewritten by the markdown renderer's `normalize_diff_blocks`
//! into `` ```diff-view `` fenced blocks, so this coupling with the markdown
//! module must be preserved.

/// Maximum number of body lines to show for a `write` tool call before
/// truncating with "...".
const MAX_TOOL_DISPLAY_LINES: usize = 100;

/// Format a tool call for display in the transcript.
pub fn format_tool_call(name: &str, args: &serde_json::Value) -> String {
    match name {
        "read" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let offset = args.get("offset").and_then(|v| v.as_u64());
            let limit = args.get("limit").and_then(|v| v.as_u64());
            match (offset, limit) {
                (Some(o), Some(l)) => format!("Read {path} (offset: {o}, limit: {l})"),
                (Some(o), None) => format!("Read {path} (offset: {o})"),
                (None, Some(l)) => format!("Read {path} (limit: {l})"),
                (None, None) => format!("Read {path}"),
            }
        }
        "write" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let lang = detect_language(path);
            let visible = truncate_first_lines(content);
            format!("Write {path}\n```{lang}\n{visible}\n```")
        }
        "edit" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let edits = normalized_edit_list(args);
            let (added, removed, diff) = build_edit_diffs(&edits);
            format!("Edit {path}(+{added} -{removed})\n<diff>{diff}\n</diff>")
        }
        "bash" => {
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            format!("Bash({command})")
        }
        _ => format!("Tool {name}"),
    }
}

fn truncate_first_lines(text: &str) -> String {
    let mut lines = text.lines();
    let mut visible: Vec<&str> = lines.by_ref().take(MAX_TOOL_DISPLAY_LINES).collect();
    if lines.next().is_some() {
        visible.push("...");
    }
    visible.join("\n")
}

fn normalized_edit_list(args: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut edits = match args.get("edits") {
        Some(serde_json::Value::Array(edits)) => edits.clone(),
        _ => Vec::new(),
    };
    if let (Some(old), Some(new)) = (
        args.get("oldText").and_then(|v| v.as_str()),
        args.get("newText").and_then(|v| v.as_str()),
    ) {
        edits.push(serde_json::json!({ "oldText": old, "newText": new }));
    }
    edits
}

fn build_edit_diffs(edits: &[serde_json::Value]) -> (usize, usize, String) {
    let mut total_added = 0usize;
    let mut total_removed = 0usize;
    let mut all_lines = Vec::new();

    for edit in edits {
        let old_text = edit.get("oldText").and_then(|v| v.as_str()).unwrap_or("");
        let new_text = edit.get("newText").and_then(|v| v.as_str()).unwrap_or("");
        let diff = build_line_diff(old_text, new_text);
        total_added += diff.added;
        total_removed += diff.removed;
        all_lines.extend(diff.lines);
    }

    let diff_text = truncate_first_lines(&all_lines.join("\n"));
    (total_added, total_removed, diff_text)
}

struct LineDiff {
    added: usize,
    removed: usize,
    lines: Vec<String>,
}

fn build_line_diff(old_text: &str, new_text: &str) -> LineDiff {
    let diff = similar::TextDiff::from_lines(old_text, new_text);
    let mut added = 0;
    let mut removed = 0;
    let mut lines = Vec::new();

    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Delete => {
                removed += 1;
                lines.push(format!(
                    "-{}",
                    change.to_string_lossy().trim_end_matches('\n')
                ));
            }
            similar::ChangeTag::Insert => {
                added += 1;
                lines.push(format!(
                    "+{}",
                    change.to_string_lossy().trim_end_matches('\n')
                ));
            }
            similar::ChangeTag::Equal => {
                lines.push(format!(
                    " {}",
                    change.to_string_lossy().trim_end_matches('\n')
                ));
            }
        }
    }

    LineDiff {
        added,
        removed,
        lines,
    }
}

/// Detect language from file extension for code fences.
pub fn detect_language(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("js") => "javascript",
        Some("ts") => "typescript",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("yaml") | Some("yml") => "yaml",
        Some("md") => "markdown",
        Some("sh") => "bash",
        Some("html") => "html",
        Some("css") => "css",
        Some("go") => "go",
        Some("java") => "java",
        Some("c") => "c",
        Some("cpp") | Some("cc") | Some("cxx") => "cpp",
        Some("h") | Some("hpp") => "c",
        _ => "",
    }
}

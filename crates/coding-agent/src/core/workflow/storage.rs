use std::path::{Path, PathBuf};

use flown_agent::create_session_id;

pub fn write_workflow_draft(
    workflows_dir: &Path,
    name: &str,
    _description: &str,
    source: &str,
) -> std::io::Result<PathBuf> {
    let id = create_session_id();
    let path = workflows_dir.join(format!("{id}-{}.js", workflow_slug(name)));
    std::fs::create_dir_all(workflows_dir)?;
    std::fs::write(&path, source)?;
    Ok(path)
}

pub fn result_path_for_draft(draft_path: &Path) -> PathBuf {
    let stem = draft_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("workflow");
    draft_path.with_file_name(format!("{stem}-result.json"))
}

pub fn write_workflow_result(path: &Path, result: &serde_json::Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(result).map_err(std::io::Error::other)?;
    std::fs::write(path, format!("{json}\n"))
}

pub fn workflow_slug(name: &str) -> String {
    let slug = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "workflow".to_string()
    } else {
        slug
    }
}

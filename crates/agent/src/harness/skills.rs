use crate::harness::env::types::{FileError, FileErrorCode, FileInfo, FileKind, FileSystem};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const MAX_NAME_LENGTH: usize = 64;
const MAX_DESCRIPTION_LENGTH: usize = 1024;
const IGNORE_FILE_NAMES: &[&str] = &[".gitignore", ".ignore", ".fdignore"];

/// Skill definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub content: String,
    pub file_path: String,
    pub disable_model_invocation: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillDiagnosticCode {
    FileInfoFailed,
    ListFailed,
    ReadFailed,
    ParseFailed,
    InvalidMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDiagnostic {
    #[serde(rename = "type")]
    pub diagnostic_type: String,
    pub code: SkillDiagnosticCode,
    pub message: String,
    pub path: String,
}

#[derive(Debug, Clone, Default)]
pub struct LoadSkillsResult {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<SkillDiagnostic>,
}

#[derive(Debug, Clone)]
pub struct SourcedSkillInput<TSource> {
    pub path: String,
    pub source: TSource,
}

#[derive(Debug, Clone)]
pub struct SourcedSkill<TSource> {
    pub skill: Skill,
    pub source: TSource,
}

#[derive(Debug, Clone)]
pub struct SourcedSkillDiagnostic<TSource> {
    pub diagnostic: SkillDiagnostic,
    pub source: TSource,
}

#[derive(Debug, Clone, Default)]
pub struct LoadSourcedSkillsResult<TSource> {
    pub skills: Vec<SourcedSkill<TSource>>,
    pub diagnostics: Vec<SourcedSkillDiagnostic<TSource>>,
}

/// Load skills from directories, preserving the legacy Vec-returning API.
pub async fn load_skills(fs: &dyn FileSystem, dirs: &[&str]) -> Result<Vec<Skill>, FileError> {
    Ok(load_skills_with_diagnostics(fs, dirs).await.skills)
}

pub async fn load_skills_with_diagnostics(fs: &dyn FileSystem, dirs: &[&str]) -> LoadSkillsResult {
    let mut result = LoadSkillsResult::default();
    for dir in dirs {
        let info = match fs.file_info(dir).await {
            Ok(info) => info,
            Err(error) => {
                if error.code != FileErrorCode::NotFound {
                    result.diagnostics.push(diagnostic(
                        SkillDiagnosticCode::FileInfoFailed,
                        error.to_string(),
                        dir,
                    ));
                }
                continue;
            }
        };
        if resolve_kind(fs, &info, &mut result.diagnostics).await != Some(FileKind::Directory) {
            continue;
        }
        load_skills_from_dir(fs, &info.path, &mut result, true, Vec::new(), &info.path).await;
    }
    result
}

pub async fn load_sourced_skills<TSource: Clone>(
    fs: &dyn FileSystem,
    inputs: &[SourcedSkillInput<TSource>],
) -> LoadSourcedSkillsResult<TSource> {
    let mut sourced = LoadSourcedSkillsResult {
        skills: Vec::new(),
        diagnostics: Vec::new(),
    };
    for input in inputs {
        let result = load_skills_with_diagnostics(fs, &[&input.path]).await;
        sourced
            .skills
            .extend(result.skills.into_iter().map(|skill| SourcedSkill {
                skill,
                source: input.source.clone(),
            }));
        sourced
            .diagnostics
            .extend(
                result
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| SourcedSkillDiagnostic {
                        diagnostic,
                        source: input.source.clone(),
                    }),
            );
    }
    sourced
}

fn load_skills_from_dir<'a>(
    fs: &'a dyn FileSystem,
    dir: &'a str,
    result: &'a mut LoadSkillsResult,
    include_root_files: bool,
    ignore_patterns: Vec<String>,
    root_dir: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let info = match fs.file_info(dir).await {
            Ok(info) => info,
            Err(error) => {
                if error.code != FileErrorCode::NotFound {
                    result.diagnostics.push(diagnostic(
                        SkillDiagnosticCode::FileInfoFailed,
                        error.to_string(),
                        dir,
                    ));
                }
                return;
            }
        };
        if resolve_kind(fs, &info, &mut result.diagnostics).await != Some(FileKind::Directory) {
            return;
        }

        let mut ignore_patterns = ignore_patterns;
        add_ignore_rules(
            fs,
            dir,
            root_dir,
            &mut ignore_patterns,
            &mut result.diagnostics,
        )
        .await;

        let entries = match fs.list_dir(dir).await {
            Ok(entries) => entries,
            Err(error) => {
                result.diagnostics.push(diagnostic(
                    SkillDiagnosticCode::ListFailed,
                    error.to_string(),
                    dir,
                ));
                return;
            }
        };

        for entry in &entries {
            if entry.name != "SKILL.md" {
                continue;
            }
            if resolve_kind(fs, entry, &mut result.diagnostics).await != Some(FileKind::File) {
                continue;
            }
            if is_ignored(root_dir, &entry.path, false, &ignore_patterns) {
                continue;
            }
            if let Some(skill) =
                load_skill_from_file(fs, &entry.path, &mut result.diagnostics).await
            {
                result.skills.push(skill);
            }
            return;
        }

        let mut entries = entries;
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in &entries {
            if entry.name.starts_with('.') || entry.name == "node_modules" || entry.name == "target"
            {
                continue;
            }
            let Some(kind) = resolve_kind(fs, entry, &mut result.diagnostics).await else {
                continue;
            };
            if is_ignored(
                root_dir,
                &entry.path,
                kind == FileKind::Directory,
                &ignore_patterns,
            ) {
                continue;
            }
            if kind == FileKind::Directory {
                load_skills_from_dir(
                    fs,
                    &entry.path,
                    result,
                    false,
                    ignore_patterns.clone(),
                    root_dir,
                )
                .await;
            } else if include_root_files && entry.name.ends_with(".md") {
                if let Some(skill) =
                    load_skill_from_file(fs, &entry.path, &mut result.diagnostics).await
                {
                    result.skills.push(skill);
                }
            }
        }
    })
}

async fn add_ignore_rules(
    fs: &dyn FileSystem,
    dir: &str,
    root_dir: &str,
    ignore_patterns: &mut Vec<String>,
    diagnostics: &mut Vec<SkillDiagnostic>,
) {
    let prefix = relative_env_path(root_dir, dir);
    let prefix = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    for filename in IGNORE_FILE_NAMES {
        let path = join_env_path(dir, filename);
        match fs.file_info(&path).await {
            Ok(info) if info.kind == FileKind::File => {}
            Ok(_) => continue,
            Err(error) => {
                if error.code != FileErrorCode::NotFound {
                    diagnostics.push(diagnostic(
                        SkillDiagnosticCode::FileInfoFailed,
                        error.to_string(),
                        &path,
                    ));
                }
                continue;
            }
        }
        let content = match fs.read_text_file(&path).await {
            Ok(content) => content,
            Err(error) => {
                diagnostics.push(diagnostic(
                    SkillDiagnosticCode::ReadFailed,
                    error.to_string(),
                    &path,
                ));
                continue;
            }
        };
        for line in content.lines() {
            if let Some(pattern) = prefix_ignore_pattern(line, &prefix) {
                ignore_patterns.push(pattern);
            }
        }
    }
}

async fn load_skill_from_file(
    fs: &dyn FileSystem,
    path: &str,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Option<Skill> {
    let content = match fs.read_text_file(path).await {
        Ok(content) => content,
        Err(error) => {
            diagnostics.push(diagnostic(
                SkillDiagnosticCode::ReadFailed,
                error.to_string(),
                path,
            ));
            return None;
        }
    };
    let (frontmatter, body) = match parse_frontmatter(&content) {
        Ok(parsed) => parsed,
        Err(error) => {
            diagnostics.push(diagnostic(
                SkillDiagnosticCode::ParseFailed,
                error.to_string(),
                path,
            ));
            return None;
        }
    };
    let parent_dir_name = basename_env_path(&dirname_env_path(path));
    let name = frontmatter
        .get("name")
        .cloned()
        .unwrap_or_else(|| parent_dir_name.clone());
    let description = frontmatter.get("description").cloned().unwrap_or_default();

    for message in validate_name(&name, &parent_dir_name) {
        diagnostics.push(diagnostic(
            SkillDiagnosticCode::InvalidMetadata,
            message,
            path,
        ));
    }
    for message in validate_description(&description) {
        diagnostics.push(diagnostic(
            SkillDiagnosticCode::InvalidMetadata,
            message,
            path,
        ));
    }

    if description.trim().is_empty() {
        return None;
    }

    let disable_model_invocation = frontmatter
        .get("disable-model-invocation")
        .and_then(|value| value.parse::<bool>().ok());

    Some(Skill {
        name,
        description,
        content: body,
        file_path: path.to_string(),
        disable_model_invocation,
    })
}

/// Format skills for system prompt
pub fn format_skills_for_system_prompt(skills: &[Skill]) -> String {
    let visible: Vec<&Skill> = skills
        .iter()
        .filter(|s| s.disable_model_invocation != Some(true))
        .collect();

    if visible.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        "The following skills provide specialized instructions for specific tasks.".to_string(),
        "Read the full skill file when the task matches its description.".to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];
    for skill in &visible {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&skill.file_path)
        ));
        lines.push("  </skill>".to_string());
    }
    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

/// Format a skill invocation
pub fn format_skill_invocation(skill: &Skill, additional_instructions: Option<&str>) -> String {
    let mut result = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        escape_xml(&skill.name),
        escape_xml(&skill.file_path),
        escape_xml(&dirname_env_path(&skill.file_path)),
        skill.content,
    );

    if let Some(instructions) = additional_instructions {
        result.push_str("\n\n");
        result.push_str(instructions);
    }

    result
}

fn diagnostic(
    code: SkillDiagnosticCode,
    message: impl Into<String>,
    path: &str,
) -> SkillDiagnostic {
    SkillDiagnostic {
        diagnostic_type: "warning".to_string(),
        code,
        message: message.into(),
        path: path.to_string(),
    }
}

async fn resolve_kind(
    fs: &dyn FileSystem,
    info: &FileInfo,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Option<FileKind> {
    match info.kind {
        FileKind::File | FileKind::Directory => Some(info.kind.clone()),
        FileKind::Symlink => {
            let canonical = match fs.canonical_path(&info.path).await {
                Ok(path) => path,
                Err(error) => {
                    if error.code != FileErrorCode::NotFound {
                        diagnostics.push(diagnostic(
                            SkillDiagnosticCode::FileInfoFailed,
                            error.to_string(),
                            &info.path,
                        ));
                    }
                    return None;
                }
            };
            fs.file_info(&canonical).await.ok().map(|info| info.kind)
        }
    }
}

fn validate_name(name: &str, parent_dir_name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if name != parent_dir_name {
        errors.push(format!(
            "name \"{name}\" does not match parent directory \"{parent_dir_name}\""
        ));
    }
    if name.len() > MAX_NAME_LENGTH {
        errors.push(format!(
            "name exceeds {MAX_NAME_LENGTH} characters ({})",
            name.len()
        ));
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        errors.push(
            "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)"
                .to_string(),
        );
    }
    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".to_string());
    }
    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".to_string());
    }
    errors
}

fn validate_description(description: &str) -> Vec<String> {
    if description.trim().is_empty() {
        vec!["description is required".to_string()]
    } else if description.len() > MAX_DESCRIPTION_LENGTH {
        vec![format!(
            "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({})",
            description.len()
        )]
    } else {
        Vec::new()
    }
}

/// Parse YAML-like frontmatter from markdown content.
fn parse_frontmatter(
    content: &str,
) -> Result<(HashMap<String, String>, String), yaml_serde::Error> {
    let mut frontmatter = HashMap::new();
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.starts_with("---") {
        return Ok((frontmatter, normalized));
    }

    let rest = &normalized[3..];
    if let Some(end) = rest.find("\n---") {
        let yaml_str = &rest[..end];
        let body = rest[end + 4..].trim().to_string();

        let parsed: yaml_serde::Value = yaml_serde::from_str(yaml_str)?;
        if let Some(mapping) = parsed.as_mapping() {
            for (key, value) in mapping {
                let Some(key) = key.as_str() else {
                    continue;
                };
                let value = match value {
                    yaml_serde::Value::String(value) => value.clone(),
                    yaml_serde::Value::Bool(value) => value.to_string(),
                    yaml_serde::Value::Number(value) => value.to_string(),
                    yaml_serde::Value::Null => String::new(),
                    _ => continue,
                };
                frontmatter.insert(key.to_string(), value);
            }
        }
        Ok((frontmatter, body))
    } else {
        Ok((frontmatter, normalized))
    }
}

fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let mut pattern = trimmed.to_string();
    let negated = pattern.starts_with('!');
    if negated {
        pattern.remove(0);
    }
    if pattern.starts_with('/') {
        pattern.remove(0);
    }
    let prefixed = format!("{prefix}{pattern}");
    Some(if negated {
        format!("!{prefixed}")
    } else {
        prefixed
    })
}

fn is_ignored(root_dir: &str, path: &str, is_dir: bool, patterns: &[String]) -> bool {
    let rel = relative_env_path(root_dir, path);
    let rel_dir = if is_dir {
        format!("{rel}/")
    } else {
        rel.clone()
    };
    let mut ignored = false;
    for pattern in patterns {
        if let Some(unignored) = pattern.strip_prefix('!') {
            if pattern_matches(unignored, &rel, &rel_dir) {
                ignored = false;
            }
        } else if pattern_matches(pattern, &rel, &rel_dir) {
            ignored = true;
        }
    }
    ignored
}

fn pattern_matches(pattern: &str, rel: &str, rel_dir: &str) -> bool {
    let pattern = pattern.trim_end_matches('/');
    rel == pattern || rel.starts_with(&format!("{pattern}/")) || rel_dir == format!("{pattern}/")
}

fn join_env_path(base: &str, child: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        child.trim_start_matches('/')
    )
}

fn dirname_env_path(path: &str) -> String {
    let normalized = path.trim_end_matches('/');
    let Some(index) = normalized.rfind('/') else {
        return "/".to_string();
    };
    if index == 0 {
        "/".to_string()
    } else {
        normalized[..index].to_string()
    }
}

fn basename_env_path(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string()
}

fn relative_env_path(root: &str, path: &str) -> String {
    let root = root.trim_end_matches('/');
    let path = path.trim_end_matches('/');
    if path == root {
        String::new()
    } else if let Some(rest) = path.strip_prefix(&format!("{root}/")) {
        rest.to_string()
    } else {
        path.trim_start_matches('/').to_string()
    }
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

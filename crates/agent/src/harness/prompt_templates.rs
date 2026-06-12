use crate::harness::env::types::{FileError, FileErrorCode, FileInfo, FileKind, FileSystem};
use serde::{Deserialize, Serialize};

/// Prompt template definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTemplate {
    pub name: String,
    pub description: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptTemplateDiagnosticCode {
    FileInfoFailed,
    ListFailed,
    ReadFailed,
    ParseFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptTemplateDiagnostic {
    #[serde(rename = "type")]
    pub diagnostic_type: String,
    pub code: PromptTemplateDiagnosticCode,
    pub message: String,
    pub path: String,
}

#[derive(Debug, Clone, Default)]
pub struct LoadPromptTemplatesResult {
    pub prompt_templates: Vec<PromptTemplate>,
    pub diagnostics: Vec<PromptTemplateDiagnostic>,
}

#[derive(Debug, Clone)]
pub struct SourcedPromptTemplateInput<TSource> {
    pub path: String,
    pub source: TSource,
}

#[derive(Debug, Clone)]
pub struct SourcedPromptTemplate<TSource> {
    pub prompt_template: PromptTemplate,
    pub source: TSource,
}

#[derive(Debug, Clone)]
pub struct SourcedPromptTemplateDiagnostic<TSource> {
    pub diagnostic: PromptTemplateDiagnostic,
    pub source: TSource,
}

#[derive(Debug, Clone, Default)]
pub struct LoadSourcedPromptTemplatesResult<TSource> {
    pub prompt_templates: Vec<SourcedPromptTemplate<TSource>>,
    pub diagnostics: Vec<SourcedPromptTemplateDiagnostic<TSource>>,
}

/// Load prompt templates from paths, preserving the legacy Vec-returning API.
pub async fn load_prompt_templates(
    fs: &dyn FileSystem,
    paths: &[&str],
) -> Result<Vec<PromptTemplate>, FileError> {
    Ok(load_prompt_templates_with_diagnostics(fs, paths)
        .await
        .prompt_templates)
}

pub async fn load_prompt_templates_with_diagnostics(
    fs: &dyn FileSystem,
    paths: &[&str],
) -> LoadPromptTemplatesResult {
    let mut result = LoadPromptTemplatesResult::default();

    for path in paths {
        let info = match fs.file_info(path).await {
            Ok(info) => info,
            Err(error) => {
                if error.code != FileErrorCode::NotFound {
                    result.diagnostics.push(diagnostic(
                        PromptTemplateDiagnosticCode::FileInfoFailed,
                        error.to_string(),
                        path,
                    ));
                }
                continue;
            }
        };
        match resolve_kind(fs, &info, &mut result.diagnostics).await {
            Some(FileKind::Directory) => load_templates_from_dir(fs, &info.path, &mut result).await,
            Some(FileKind::File) if info.name.ends_with(".md") => {
                if let Some(template) =
                    load_template_from_file(fs, &info.path, &mut result.diagnostics).await
                {
                    result.prompt_templates.push(template);
                }
            }
            _ => {}
        }
    }

    result
}

pub async fn load_sourced_prompt_templates<TSource: Clone>(
    fs: &dyn FileSystem,
    inputs: &[SourcedPromptTemplateInput<TSource>],
) -> LoadSourcedPromptTemplatesResult<TSource> {
    let mut sourced = LoadSourcedPromptTemplatesResult {
        prompt_templates: Vec::new(),
        diagnostics: Vec::new(),
    };
    for input in inputs {
        let result = load_prompt_templates_with_diagnostics(fs, &[&input.path]).await;
        sourced
            .prompt_templates
            .extend(result.prompt_templates.into_iter().map(|prompt_template| {
                SourcedPromptTemplate {
                    prompt_template,
                    source: input.source.clone(),
                }
            }));
        sourced
            .diagnostics
            .extend(result.diagnostics.into_iter().map(|diagnostic| {
                SourcedPromptTemplateDiagnostic {
                    diagnostic,
                    source: input.source.clone(),
                }
            }));
    }
    sourced
}

async fn load_templates_from_dir(
    fs: &dyn FileSystem,
    dir: &str,
    result: &mut LoadPromptTemplatesResult,
) {
    let entries = match fs.list_dir(dir).await {
        Ok(entries) => entries,
        Err(error) => {
            result.diagnostics.push(diagnostic(
                PromptTemplateDiagnosticCode::ListFailed,
                error.to_string(),
                dir,
            ));
            return;
        }
    };

    let mut entries = entries;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    for entry in &entries {
        if resolve_kind(fs, entry, &mut result.diagnostics).await != Some(FileKind::File)
            || !entry.name.ends_with(".md")
        {
            continue;
        }
        if let Some(template) =
            load_template_from_file(fs, &entry.path, &mut result.diagnostics).await
        {
            result.prompt_templates.push(template);
        }
    }
}

async fn load_template_from_file(
    fs: &dyn FileSystem,
    path: &str,
    diagnostics: &mut Vec<PromptTemplateDiagnostic>,
) -> Option<PromptTemplate> {
    let content = match fs.read_text_file(path).await {
        Ok(content) => content,
        Err(error) => {
            diagnostics.push(diagnostic(
                PromptTemplateDiagnosticCode::ReadFailed,
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
                PromptTemplateDiagnosticCode::ParseFailed,
                error.to_string(),
                path,
            ));
            return None;
        }
    };
    let name = frontmatter
        .get("name")
        .cloned()
        .unwrap_or_else(|| basename_env_path(path).trim_end_matches(".md").to_string());
    let description = frontmatter.get("description").cloned().or_else(|| {
        let first_line = body.lines().find(|line| !line.trim().is_empty())?;
        if first_line.len() > 60 {
            Some(format!("{}...", &first_line[..60]))
        } else {
            Some(first_line.to_string())
        }
    });

    Some(PromptTemplate {
        name,
        description,
        content: body,
    })
}

/// Format a prompt template invocation with argument substitution
pub fn format_prompt_template_invocation(template: &PromptTemplate, args: &[&str]) -> String {
    substitute_args(&template.content, args)
}

/// Substitute arguments in content.
fn substitute_args(content: &str, args: &[&str]) -> String {
    let mut result = content.to_string();

    while let Some(start) = result.find("${@:") {
        let rest = &result[start + 4..];
        if let Some(end) = rest.find('}') {
            let spec = &rest[..end];
            let parts: Vec<&str> = spec.split(':').collect();
            let replacement = match parts.len() {
                1 => {
                    let n = parts[0].parse::<usize>().unwrap_or(1).saturating_sub(1);
                    if n < args.len() {
                        args[n..].join(" ")
                    } else {
                        String::new()
                    }
                }
                2 => {
                    let n = parts[0].parse::<usize>().unwrap_or(1).saturating_sub(1);
                    let len = parts[1].parse::<usize>().unwrap_or(args.len());
                    let end = (n + len).min(args.len());
                    if n < args.len() {
                        args[n..end].join(" ")
                    } else {
                        String::new()
                    }
                }
                _ => String::new(),
            };
            result = format!("{}{}{}", &result[..start], replacement, &rest[end + 1..]);
        } else {
            break;
        }
    }

    for (i, arg) in args.iter().enumerate() {
        result = result.replace(&format!("${}", i + 1), arg);
    }

    let all_args = args.join(" ");
    result = result.replace("$ARGUMENTS", &all_args);
    result = result.replace("$@", &all_args);

    result
}

/// Parse command arguments (shell-style)
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    for c in args_string.chars() {
        match c {
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            ' ' | '\t' if !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

fn parse_frontmatter(
    content: &str,
) -> Result<(std::collections::HashMap<String, String>, String), yaml_serde::Error> {
    let mut frontmatter = std::collections::HashMap::new();
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

async fn resolve_kind(
    fs: &dyn FileSystem,
    info: &FileInfo,
    diagnostics: &mut Vec<PromptTemplateDiagnostic>,
) -> Option<FileKind> {
    match info.kind {
        FileKind::File | FileKind::Directory => Some(info.kind.clone()),
        FileKind::Symlink => {
            let canonical = match fs.canonical_path(&info.path).await {
                Ok(path) => path,
                Err(error) => {
                    if error.code != FileErrorCode::NotFound {
                        diagnostics.push(diagnostic(
                            PromptTemplateDiagnosticCode::FileInfoFailed,
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

fn diagnostic(
    code: PromptTemplateDiagnosticCode,
    message: impl Into<String>,
    path: &str,
) -> PromptTemplateDiagnostic {
    PromptTemplateDiagnostic {
        diagnostic_type: "warning".to_string(),
        code,
        message: message.into(),
        path: path.to_string(),
    }
}

fn basename_env_path(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string()
}

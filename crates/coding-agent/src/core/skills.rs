use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const MAX_NAME_LENGTH: usize = 64;
const MAX_DESCRIPTION_LENGTH: usize = 1024;

/// Skill definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub content: String,
    pub file_path: String,
    pub disable_model_invocation: Option<bool>,
}

/// Load skills from directories
pub async fn load_skills(dirs: &[&str]) -> Result<Vec<Skill>, String> {
    let mut skills = Vec::new();
    for dir in dirs {
        let path = std::path::Path::new(dir);
        if !path.exists() || !path.is_dir() {
            continue;
        }
        load_skills_from_dir(path, &mut skills).await?;
    }
    Ok(skills)
}

async fn load_skills_from_dir(dir: &std::path::Path, skills: &mut Vec<Skill>) -> Result<(), String> {
    let mut entries = tokio::fs::read_dir(dir)
        .await
        .map_err(|e| format!("Failed to read directory {}: {}", dir.display(), e))?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| format!("Failed to read entry: {}", e))?
    {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();

        if file_name == "SKILL.md" {
            if let Some(skill) = load_skill_from_file(&path).await? {
                skills.push(skill);
            }
            return Ok(());
        }
    }

    // If no SKILL.md found in root, look in subdirectories
    let mut entries = tokio::fs::read_dir(dir)
        .await
        .map_err(|e| format!("Failed to read directory {}: {}", dir.display(), e))?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| format!("Failed to read entry: {}", e))?
    {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();

        if file_name.starts_with('.') || file_name == "node_modules" || file_name == "target" {
            continue;
        }

        if path.is_dir() {
            Box::pin(load_skills_from_dir(&path, skills)).await?;
        }
    }

    Ok(())
}

async fn load_skill_from_file(path: &std::path::Path) -> Result<Option<Skill>, String> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

    let (frontmatter, body) = parse_frontmatter(&content)?;
    let parent_dir_name = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let name = frontmatter
        .get("name")
        .cloned()
        .unwrap_or_else(|| parent_dir_name.clone());
    let description = frontmatter.get("description").cloned().unwrap_or_default();

    // Validate name
    for message in validate_name(&name, &parent_dir_name) {
        eprintln!("Warning: {} in {}", message, path.display());
    }

    // Validate description
    for message in validate_description(&description) {
        eprintln!("Warning: {} in {}", message, path.display());
    }

    if description.trim().is_empty() {
        return Ok(None);
    }

    let disable_model_invocation = frontmatter
        .get("disable-model-invocation")
        .and_then(|value| value.parse::<bool>().ok());

    Ok(Some(Skill {
        name,
        description,
        content: body,
        file_path: path.to_string_lossy().to_string(),
        disable_model_invocation,
    }))
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

    let mut output = Vec::new();

    output.push("The following skills provide specialized instructions for specific tasks.".to_string());
    output.push("Read the full skill file when the task matches its description.".to_string());
    output.push("When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string());
    output.push(String::new());
    output.push("<available_skills>".to_string());
    for skill in &visible {
        output.push("  <skill>".to_string());
        output.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        output.push(format!("    <description>{}</description>", escape_xml(&skill.description)));
        output.push(format!("    <location>{}</location>", escape_xml(&skill.file_path)));
        output.push("  </skill>".to_string());
    }
    output.push("</available_skills>".to_string());

    output.join("\n")
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
fn parse_frontmatter(content: &str) -> Result<(HashMap<String, String>, String), String> {
    let mut frontmatter = HashMap::new();
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.starts_with("---") {
        return Ok((frontmatter, normalized));
    }

    let rest = &normalized[3..];
    if let Some(end) = rest.find("\n---") {
        let yaml_str = &rest[..end];
        let body = rest[end + 4..].trim().to_string();

        // Simple YAML parser for frontmatter
        for line in yaml_str.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = parse_yaml_line(line) {
                frontmatter.insert(key, value);
            }
        }
        Ok((frontmatter, body))
    } else {
        Ok((frontmatter, normalized))
    }
}

fn parse_yaml_line(line: &str) -> Option<(String, String)> {
    let colon_pos = line.find(':')?;
    let key = line[..colon_pos].trim().to_string();
    let value = line[colon_pos + 1..].trim();

    // Remove quotes if present
    let value = if (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    };

    Some((key, value))
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

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

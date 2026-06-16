use std::collections::HashSet;
use std::path::Path;

use crate::core::skills::Skill;

pub const DEFAULT_SYSTEM_PROMPT_SENTINEL: &str = "You are Flown coding agent.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPromptSnippet {
    pub name: String,
    pub snippet: String,
}

impl ToolPromptSnippet {
    pub fn new(name: impl Into<String>, snippet: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            snippet: snippet.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectContextFile {
    pub path: String,
    pub content: String,
}

pub struct BuildSystemPromptOptions {
    pub custom_prompt: Option<String>,
    pub selected_tools: Vec<String>,
    pub tool_snippets: Vec<ToolPromptSnippet>,
    pub prompt_guidelines: Vec<String>,
    pub append_system_prompt: Option<String>,
    pub cwd: String,
    pub context_files: Vec<ProjectContextFile>,
    pub skills: Vec<Skill>,
}

impl Default for BuildSystemPromptOptions {
    fn default() -> Self {
        Self {
            custom_prompt: None,
            selected_tools: default_selected_tools(),
            tool_snippets: default_tool_snippets(),
            prompt_guidelines: Vec::new(),
            append_system_prompt: None,
            cwd: std::env::current_dir()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string()),
            context_files: Vec::new(),
            skills: Vec::new(),
        }
    }
}

pub async fn build_system_prompt(options: BuildSystemPromptOptions) -> String {
    let prompt_cwd = normalize_prompt_path(&options.cwd);
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let append_section = options
        .append_system_prompt
        .as_deref()
        .filter(|text| !text.is_empty())
        .map(|text| format!("\n\n{text}"))
        .unwrap_or_default();

    let skills = &options.skills;

    let prompt = if let Some(custom_prompt) = options.custom_prompt {
        let mut prompt = custom_prompt;
        prompt.push_str(&append_section);
        append_project_context(&mut prompt, &options.context_files);

        if !skills.is_empty() {
            prompt.push_str(&crate::core::skills::format_skills_for_system_prompt(skills));
        }

        append_date_and_cwd(&mut prompt, &date, &prompt_cwd);
        prompt
    } else {
        let selected_tools = normalize_default_prompt_tools(options.selected_tools);
        let tool_snippets = normalize_default_prompt_snippets(options.tool_snippets);
        let tools_list = format_tools_list(&selected_tools, &tool_snippets);
        let guidelines = format_guidelines(&selected_tools, &options.prompt_guidelines);

        let mut prompt = format!(
            "You are a coding assistant. You help users by reading files, executing commands, editing code, and writing new files.\n\n\
Available tools:\n{tools_list}\n\n\
In addition to the tools above, you may have access to other custom tools depending on the project.\n\n\
Guidelines:\n{guidelines}\n\n\
Project context may provide additional instructions below.",
        );

        prompt.push_str(&append_section);
        append_project_context(&mut prompt, &options.context_files);

        if !skills.is_empty() {
            prompt.push_str(&crate::core::skills::format_skills_for_system_prompt(skills));
        }

        append_date_and_cwd(&mut prompt, &date, &prompt_cwd);

        prompt
    };

    persist_system_prompt(&prompt);
    prompt
}

fn persist_system_prompt(prompt: &str) {
    let path = dirs::home_dir()
        .unwrap_or_else(|| Path::new(".").to_path_buf())
        .join(".flown")
        .join("system_prompt.md");

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let _ = std::fs::write(&path, prompt);
}

pub fn default_selected_tools() -> Vec<String> {
    ["read", "bash", "edit", "write"]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect()
}

pub fn default_tool_snippets() -> Vec<ToolPromptSnippet> {
    vec![
        ToolPromptSnippet::new("read", "Read file contents"),
        ToolPromptSnippet::new("bash", "Execute bash commands"),
        ToolPromptSnippet::new("edit", "Make surgical edits"),
        ToolPromptSnippet::new("write", "Create or overwrite files"),
    ]
}

pub fn load_project_context_files(cwd: impl AsRef<Path>) -> Vec<ProjectContextFile> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut ancestors = Vec::new();
    let Ok(mut current) = std::fs::canonicalize(cwd.as_ref()) else {
        return files;
    };

    loop {
        if let Some(file) = load_context_file_from_dir(&current)
            && seen.insert(file.path.clone())
        {
            ancestors.push(file);
        }

        if !current.pop() {
            break;
        }
    }

    ancestors.reverse();
    files.extend(ancestors);
    files
}

fn load_context_file_from_dir(dir: &Path) -> Option<ProjectContextFile> {
    for filename in ["AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"] {
        let path = dir.join(filename);
        if path.exists()
            && let Ok(content) = std::fs::read_to_string(&path)
        {
            return Some(ProjectContextFile {
                path: path.to_string_lossy().to_string(),
                content,
            });
        }
    }
    None
}

fn format_tools_list(selected_tools: &[String], snippets: &[ToolPromptSnippet]) -> String {
    let lines = selected_tools
        .iter()
        .filter_map(|tool| {
            snippets
                .iter()
                .find(|snippet| snippet.name == *tool)
                .map(|snippet| format!("- {}: {}", snippet.name, snippet.snippet))
        })
        .collect::<Vec<_>>();

    if lines.is_empty() {
        "(none)".to_string()
    } else {
        lines.join("\n")
    }
}

fn format_guidelines(selected_tools: &[String], prompt_guidelines: &[String]) -> String {
    let mut seen = HashSet::new();
    let mut guidelines = Vec::new();
    let mut add = |guideline: String| {
        if !guideline.is_empty() && seen.insert(guideline.clone()) {
            guidelines.push(guideline);
        }
    };

    let has_bash = selected_tools.iter().any(|tool| tool == "bash");
    let has_grep = selected_tools.iter().any(|tool| tool == "grep");
    let has_find = selected_tools.iter().any(|tool| tool == "find");
    let has_ls = selected_tools.iter().any(|tool| tool == "ls");

    if has_bash && !has_grep && !has_find && !has_ls {
        add("Use bash for file operations like ls, rg, find".to_string());
    } else if has_bash && (has_grep || has_find || has_ls) {
        add("Prefer grep/find/ls tools over bash for file exploration (faster, respects .gitignore)".to_string());
    }

    for guideline in prompt_guidelines {
        add(guideline.trim().to_string());
    }

    add("Be concise in your responses".to_string());
    add("Show file paths clearly when working with files".to_string());

    guidelines
        .into_iter()
        .map(|guideline| format!("- {guideline}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_default_prompt_tools(selected_tools: Vec<String>) -> Vec<String> {
    if selected_tools.is_empty() {
        return selected_tools;
    }

    selected_tools
}

fn normalize_default_prompt_snippets(snippets: Vec<ToolPromptSnippet>) -> Vec<ToolPromptSnippet> {
    let mut merged = default_tool_snippets();

    for snippet in snippets {
        if let Some(existing) = merged.iter_mut().find(|existing| existing.name == snippet.name) {
            *existing = snippet;
        } else {
            merged.push(snippet);
        }
    }

    merged
}

fn append_project_context(prompt: &mut String, context_files: &[ProjectContextFile]) {
    if context_files.is_empty() {
        return;
    }

    prompt.push_str("\n\n<project_context>\n\n");
    prompt.push_str("Project-specific instructions and guidelines:\n\n");
    for file in context_files {
        prompt.push_str(&format!(
            "<project_instructions path=\"{}\">\n{}\n</project_instructions>\n\n",
            file.path, file.content
        ));
    }
    prompt.push_str("</project_context>\n");
}

fn append_date_and_cwd(prompt: &mut String, date: &str, cwd: &str) {
    prompt.push_str(&format!("\nCurrent date: {date}"));
    prompt.push_str(&format!("\nCurrent working directory: {cwd}"));
}

fn normalize_prompt_path(path: &str) -> String {
    path.replace('\\', "/")
}

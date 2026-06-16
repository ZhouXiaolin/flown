//! Slash command registry and handlers for the TUI.

use crate::config::Config;
use crate::tui::state::TranscriptHandle;

/// A subcommand definition used by autocomplete, help, and dispatch.
#[derive(Debug, Clone)]
pub struct SubcommandDef {
    pub name: &'static str,
    pub description: &'static str,
}

/// A slash command definition. This is the single metadata source for
/// completion, help text, aliases, and dispatch.
#[derive(Debug, Clone)]
pub struct SlashCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub subcommands: &'static [SubcommandDef],
    pub(crate) action: CommandAction,
}

impl SlashCommand {
    pub fn matches(&self, name: &str) -> bool {
        self.name == name || self.aliases.contains(&name)
    }
}

/// A unified, owned view of one slash command for completion and `/help`.
///
/// Static commands (`SLASH_COMMANDS`) and extension-registered commands
/// (`CommandSide`) have different source types but identical completion
/// metadata (name, description, subcommands). Code that needs to present them
/// uniformly — the autocomplete popup and `/help` — works against a merged
/// `Vec<CommandEntry>` snapshot rather than reaching into either source.
#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub name: String,
    pub description: String,
    pub subcommands: Vec<SubcommandEntry>,
}

/// A named subcommand shown in the second-level popup and in `/help`.
#[derive(Debug, Clone)]
pub struct SubcommandEntry {
    pub name: String,
    pub description: String,
}

impl CommandEntry {
    /// Whether this command has any subcommands.
    pub fn has_subcommands(&self) -> bool {
        !self.subcommands.is_empty()
    }
}

/// Build the static-command portion of the merged view from `SLASH_COMMANDS`.
/// Extension commands (e.g. `/mcp`) are appended by the caller from
/// `CommandSide` so they appear in autocomplete and `/help` without living in
/// the static dispatch table.
pub fn static_command_entries() -> Vec<CommandEntry> {
    SLASH_COMMANDS
        .iter()
        .map(|cmd| CommandEntry {
            name: cmd.name.to_string(),
            description: cmd.description.to_string(),
            subcommands: cmd
                .subcommands
                .iter()
                .map(|s| SubcommandEntry {
                    name: s.name.to_string(),
                    description: s.description.to_string(),
                })
                .collect(),
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum CommandAction {
    Help,
    Clear,
    Quit,
    Skills,
    Placeholder,
}

/// Built-in slash commands.
pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/help",
        aliases: &["/h", "/?"],
        description: "Show available commands",
        subcommands: &[],
        action: CommandAction::Help,
    },
    SlashCommand {
        name: "/clear",
        aliases: &["/cls"],
        description: "Clear the transcript",
        subcommands: &[],
        action: CommandAction::Clear,
    },
    SlashCommand {
        name: "/model",
        aliases: &[],
        description: "Switch model",
        subcommands: &[],
        action: CommandAction::Placeholder,
    },
    SlashCommand {
        name: "/compact",
        aliases: &[],
        description: "Compact conversation",
        subcommands: &[],
        action: CommandAction::Placeholder,
    },
    SlashCommand {
        name: "/skills",
        aliases: &[],
        description: "List available skills",
        subcommands: &[],
        action: CommandAction::Skills,
    },
    SlashCommand {
        name: "/quit",
        aliases: &["/exit", "/q"],
        description: "Exit the application",
        subcommands: &[],
        action: CommandAction::Quit,
    },
];

/// Dispatch a slash command. Returns `true` if the user wants to quit.
///
/// `commands` is the merged command view (static + extension) used by `/help`
/// so it lists extension-registered commands like `/mcp` even though they
/// dispatch through the extension layer, not this function.
pub fn handle_slash_command(
    text: &str,
    transcript: &mut dyn TranscriptHandle,
    config: &Config,
    commands: &[CommandEntry],
) -> bool {
    let mut parts = text.split_whitespace();
    let Some(name) = parts.next() else {
        return false;
    };
    let rest = parts.collect::<Vec<_>>().join(" ");
    let _ = rest;

    let Some(command) = SLASH_COMMANDS.iter().find(|cmd| cmd.matches(name)) else {
        transcript.push_error(format!(
            "Unknown command: {name}. Type /help for available commands."
        ));
        return false;
    };

    match command.action {
        CommandAction::Help => handle_help(commands, transcript),
        CommandAction::Clear => transcript.clear(),
        CommandAction::Quit => return true,
        CommandAction::Skills => handle_skills(config, transcript),
        CommandAction::Placeholder => {
            transcript.push_error(format!(
                "{} is not yet implemented. Coming soon!",
                command.name
            ));
        }
    }
    false
}

fn handle_help(commands: &[CommandEntry], transcript: &mut dyn TranscriptHandle) {
    let mut lines = vec!["Available commands:".to_string()];
    for command in commands {
        let usage = if command.subcommands.is_empty() {
            command.name.clone()
        } else {
            format!("{} <sub>", command.name)
        };
        lines.push(format!("  {usage:<16} {}", command.description));
    }
    transcript.push_system(lines.join("\n"));
}

fn handle_skills(config: &Config, transcript: &mut dyn TranscriptHandle) {
    let skills = list_installed_skills(config);

    if skills.is_empty() {
        transcript.push_system(
            "No skills found.\n  Place skill directories under ~/.flown/skills/<name>/SKILL.md"
                .to_string(),
        );
        return;
    }

    let mut lines = vec!["**Available Skills:**".to_string()];
    for (name, desc) in &skills {
        lines.push(format!("- **{name}** - {desc}"));
    }
    lines.push(String::new());
    lines.push(format!("{} skills loaded", skills.len()));

    transcript.push_system(lines.join("\n"));
}

/// Scan the same directories as `/skills` (config `skills_dir` + local
/// `./.claude/skills`) and return `(name, description)` pairs, sorted by name.
/// Single source of truth for both listing and `/skill:<name>` validation, so
/// the two views never disagree on what counts as "installed".
pub(crate) fn list_installed_skills(config: &Config) -> Vec<(String, String)> {
    let mut skills: Vec<(String, String)> = Vec::new();
    collect_skills(&config.skills_dir, &mut skills);

    if let Ok(cwd) = std::env::current_dir() {
        let local = cwd.join(".claude").join("skills");
        if local.exists() {
            collect_skills(&local, &mut skills);
        }
    }
    skills
}

/// Collect skill names and descriptions from a directory by scanning SKILL.md files.
fn collect_skills(dir: &std::path::Path, skills: &mut Vec<(String, String)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skill_file = path.join("SKILL.md");
            if skill_file.exists()
                && let Some((name, desc)) = parse_skill_metadata(&skill_file)
            {
                skills.push((name, desc));
            }
        }
    }

    skills.sort_by(|a, b| a.0.cmp(&b.0));
}

/// Parse frontmatter from a SKILL.md file to extract name and description.
fn parse_skill_metadata(path: &std::path::Path) -> Option<(String, String)> {
    let content = std::fs::read_to_string(path).ok()?;
    let (frontmatter, _) = crate::core::skills::parse_frontmatter_static(&content).ok()?;

    let dir_name = path.parent()?.file_name()?.to_string_lossy().to_string();
    let name = frontmatter.get("name").cloned().unwrap_or(dir_name);
    let desc = frontmatter.get("description").cloned().unwrap_or_default();

    if desc.trim().is_empty() {
        return None;
    }

    Some((name, desc))
}

// ---------------------------------------------------------------------------
// `/skill:<name>` command family
//
// Unlike the static `SLASH_COMMANDS` table (exact-name match), `/skill:<name>`
// is a parameterized family — one entry per installed skill. It is parsed and
// dispatched up front in `app.rs` (before the generic slash path) because it
// must trigger an agent turn, and only `app.rs` holds the agent handle. These
// helpers are pure functions so they stay unit-testable without a TUI.
// ---------------------------------------------------------------------------

/// A parsed `/skill:<name> [<request>]` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInvocation {
    /// The skill name after the colon (may be empty on malformed input like
    /// a bare `/skill:`; validation surfaces that as "not found").
    pub skill_name: String,
    /// Optional extra context after the first space. `None` when the user
    /// invoked `/skill:<name>` with no further text — that is a complete,
    /// valid invocation on its own.
    pub request: Option<String>,
}

/// If `text` is a `/skill:<name> ...` line, parse it. Returns `None` for any
/// other input (incl. `/skills` and `/help`) so the caller falls through to
/// the generic slash dispatcher.
///
/// The skill name runs from just after `/skill:` up to the first whitespace;
/// anything beyond that whitespace is the optional `<request>`.
pub fn parse_skill_command(text: &str) -> Option<SkillInvocation> {
    let rest = text.strip_prefix("/skill:")?;
    // Name = up to the first whitespace; request = the remainder.
    let (name_part, request_part) = match rest.find(char::is_whitespace) {
        Some(idx) => (&rest[..idx], rest[idx..].trim()),
        None => (rest, ""),
    };
    let request = if request_part.is_empty() {
        None
    } else {
        Some(request_part.to_string())
    };
    Some(SkillInvocation {
        skill_name: name_part.to_string(),
        request,
    })
}

/// Build the text actually sent to the model:
/// - no request → `use skill:<name>`
/// - with request → `use skill:<name> and 考虑 <request>`
pub fn build_skill_prompt(inv: &SkillInvocation) -> String {
    match &inv.request {
        None => format!("use skill:{}", inv.skill_name),
        Some(req) => format!("use skill:{} and 考虑 {}", inv.skill_name, req),
    }
}

/// Validate `name` against installed skills (same scan as `/skills`). On
/// success returns `Ok(())`; on failure returns the sorted list of available
/// names so the caller can surface them in an error message.
pub fn validate_skill_name(name: &str, config: &Config) -> Result<(), Vec<String>> {
    let installed = list_installed_skills(config);
    if installed.iter().any(|(n, _)| n == name) {
        Ok(())
    } else {
        Err(installed.into_iter().map(|(n, _)| n).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skill_name_only() {
        let inv = parse_skill_command("/skill:docx").unwrap();
        assert_eq!(inv.skill_name, "docx");
        assert_eq!(inv.request, None);
    }

    #[test]
    fn parse_skill_name_and_request() {
        let inv = parse_skill_command("/skill:docx 怎么改首页").unwrap();
        assert_eq!(inv.skill_name, "docx");
        assert_eq!(inv.request.as_deref(), Some("怎么改首页"));
    }

    #[test]
    fn parse_skill_collapses_extra_whitespace() {
        let inv = parse_skill_command("/skill:docx   怎么改   首页").unwrap();
        assert_eq!(inv.skill_name, "docx");
        assert_eq!(inv.request.as_deref(), Some("怎么改   首页"));
    }

    #[test]
    fn parse_skill_empty_name_is_malformed() {
        // `/skill:` with nothing after the colon → empty name. parse still
        // succeeds (it's syntactically a skill command); validation rejects it.
        let inv = parse_skill_command("/skill:").unwrap();
        assert_eq!(inv.skill_name, "");
        assert_eq!(inv.request, None);
    }

    #[test]
    fn parse_skill_empty_name_with_request() {
        let inv = parse_skill_command("/skill: something").unwrap();
        assert_eq!(inv.skill_name, "");
        assert_eq!(inv.request.as_deref(), Some("something"));
    }

    #[test]
    fn parse_non_skill_lines_return_none() {
        assert!(parse_skill_command("/help").is_none());
        assert!(parse_skill_command("/skills").is_none());
        assert!(parse_skill_command("hello world").is_none());
        assert!(parse_skill_command("").is_none());
        // `/skillsx` must NOT be mistaken for the skill family.
        assert!(parse_skill_command("/skillsx").is_none());
    }

    #[test]
    fn build_prompt_without_request() {
        let inv = SkillInvocation {
            skill_name: "docx".into(),
            request: None,
        };
        assert_eq!(build_skill_prompt(&inv), "use skill:docx");
    }

    #[test]
    fn build_prompt_with_request() {
        let inv = SkillInvocation {
            skill_name: "docx".into(),
            request: Some("怎么改首页".into()),
        };
        assert_eq!(build_skill_prompt(&inv), "use skill:docx and 考虑 怎么改首页");
    }
}

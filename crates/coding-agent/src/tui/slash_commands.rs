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

#[derive(Debug, Clone, Copy)]
pub(crate) enum CommandAction {
    Help,
    Clear,
    Quit,
    Mcp,
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
        name: "/mcp",
        aliases: &[],
        description: "Manage MCP servers",
        subcommands: &[
            SubcommandDef {
                name: "list",
                description: "List configured servers",
            },
            SubcommandDef {
                name: "status",
                description: "Show server connection status",
            },
            SubcommandDef {
                name: "help",
                description: "Show MCP help",
            },
        ],
        action: CommandAction::Mcp,
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
pub fn handle_slash_command(
    text: &str,
    transcript: &mut dyn TranscriptHandle,
    config: &Config,
) -> bool {
    let mut parts = text.split_whitespace();
    let Some(name) = parts.next() else {
        return false;
    };
    let rest = parts.collect::<Vec<_>>().join(" ");

    let Some(command) = SLASH_COMMANDS.iter().find(|cmd| cmd.matches(name)) else {
        transcript.push_error(format!(
            "Unknown command: {name}. Type /help for available commands."
        ));
        return false;
    };

    match command.action {
        CommandAction::Help => handle_help(transcript),
        CommandAction::Clear => transcript.clear(),
        CommandAction::Quit => return true,
        CommandAction::Mcp => handle_mcp(&rest, config, transcript),
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

fn handle_help(transcript: &mut dyn TranscriptHandle) {
    let mut lines = vec!["Available commands:".to_string()];
    for command in SLASH_COMMANDS {
        let usage = if command.subcommands.is_empty() {
            command.name.to_string()
        } else {
            format!("{} <sub>", command.name)
        };
        lines.push(format!("  {usage:<16} {}", command.description));
    }
    transcript.push_system(lines.join("\n"));
}

fn handle_mcp(subcommand: &str, config: &Config, transcript: &mut dyn TranscriptHandle) {
    match subcommand.trim() {
        "" | "help" => handle_mcp_help(transcript),
        "list" => mcp_list(config, transcript),
        "status" => mcp_status(config, transcript),
        other => {
            transcript.push_error(format!(
                "Unknown /mcp subcommand: {other}. Type /mcp help for usage."
            ));
        }
    }
}

fn handle_mcp_help(transcript: &mut dyn TranscriptHandle) {
    let Some(command) = SLASH_COMMANDS.iter().find(|cmd| cmd.name == "/mcp") else {
        return;
    };
    let mut lines = vec!["MCP server management:".to_string()];
    for subcommand in command.subcommands {
        lines.push(format!(
            "  /mcp {:<10} {}",
            subcommand.name, subcommand.description
        ));
    }
    transcript.push_system(lines.join("\n"));
}

fn mcp_list(config: &Config, transcript: &mut dyn TranscriptHandle) {
    if config.mcp_servers.is_empty() {
        transcript.push_system("No MCP servers configured.".to_string());
        return;
    }

    let mut lines = vec!["MCP Servers:".to_string()];
    for (name, server) in &config.mcp_servers {
        let status = if server.disabled {
            "disabled"
        } else {
            "enabled"
        };
        let full_cmd = if server.args.is_empty() {
            server.command.clone()
        } else {
            format!("{} {}", server.command, server.args.join(" "))
        };
        lines.push(format!("  {name}  - {full_cmd} ({status})"));
    }
    lines.push(String::new());
    lines.push("Use /mcp status to check connection state.".to_string());

    transcript.push_system(lines.join("\n"));
}

fn mcp_status(config: &Config, transcript: &mut dyn TranscriptHandle) {
    if config.mcp_servers.is_empty() {
        transcript.push_system("No MCP servers configured.".to_string());
        return;
    }

    let mut lines = vec!["MCP Servers (configured):".to_string()];
    for (name, server) in &config.mcp_servers {
        let icon = if server.disabled { "x" } else { "*" };
        let full_cmd = if server.args.is_empty() {
            server.command.clone()
        } else {
            format!("{} {}", server.command, server.args.join(" "))
        };
        lines.push(format!("  {icon} {name}  - {full_cmd}"));
    }
    lines.push(String::new());
    lines.push(
        "Note: /mcp status shows config state. Run `flown mcp status` for live connection info."
            .to_string(),
    );

    transcript.push_system(lines.join("\n"));
}

fn handle_skills(config: &Config, transcript: &mut dyn TranscriptHandle) {
    let skills_dir = &config.skills_dir;

    let mut skills: Vec<(String, String)> = Vec::new();
    collect_skills(skills_dir, &mut skills);

    if let Ok(cwd) = std::env::current_dir() {
        let local = cwd.join(".claude").join("skills");
        if local.exists() {
            collect_skills(&local, &mut skills);
        }
    }

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
